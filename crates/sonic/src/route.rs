use crate::logs::Logs;
use crate::snapshot::{Fields, Snapshot};
use serde_json::Value;
use std::collections::BTreeSet;
use trace_core::{Diagnosis, Presence, Report, Stage, StageResult};

const ROUTE: &str = "ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:";
const NH_GROUP_MEMBER: &str = "ASIC_STATE:SAI_OBJECT_TYPE_NEXT_HOP_GROUP_MEMBER:";

fn stage(name: &str, owner: &str) -> Stage {
    Stage {
        name: name.into(),
        owner: owner.into(),
    }
}

fn asic_key(kind: &str, oid: &str) -> String {
    format!("ASIC_STATE:SAI_OBJECT_TYPE_{kind}:{oid}")
}

fn split(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
}

fn join(set: &BTreeSet<String>) -> String {
    set.iter().cloned().collect::<Vec<_>>().join(", ")
}

/// What a route's SAI next-hop OID points at.
struct Target {
    kind: &'static str,
    ips: BTreeSet<String>,
    members: usize,
}

fn resolve(snap: &Snapshot, oid: &str) -> Option<Target> {
    for kind in ["NEXT_HOP", "NEXT_HOP_GROUP", "ROUTER_INTERFACE", "PORT"] {
        let Some(f) = snap.get("ASIC_DB", &asic_key(kind, oid)) else {
            continue;
        };
        let mut ips = BTreeSet::new();
        let mut members = 0;
        match kind {
            "NEXT_HOP" => {
                ips.extend(f.get("SAI_NEXT_HOP_ATTR_IP").cloned());
            }
            "NEXT_HOP_GROUP" => {
                for (k, m) in snap.iter("ASIC_DB") {
                    let in_group = m
                        .get("SAI_NEXT_HOP_GROUP_MEMBER_ATTR_NEXT_HOP_GROUP_ID")
                        .map(String::as_str)
                        == Some(oid);
                    if !k.starts_with(NH_GROUP_MEMBER) || !in_group {
                        continue;
                    }
                    members += 1;
                    if let Some(nh) = m.get("SAI_NEXT_HOP_GROUP_MEMBER_ATTR_NEXT_HOP_ID") {
                        let nh_fields = snap.get("ASIC_DB", &asic_key("NEXT_HOP", nh));
                        ips.extend(
                            nh_fields
                                .and_then(|f| f.get("SAI_NEXT_HOP_ATTR_IP"))
                                .cloned(),
                        );
                    }
                }
            }
            _ => {}
        }
        return Some(Target { kind, ips, members });
    }
    None
}

/// Next-hop addresses APPL_DB expects, following `nexthop_group` and class-based groups.
/// Connected and local routes use `0.0.0.0` / `::`, which are not real next hops.
fn appl_nexthops(snap: &Snapshot, f: &Fields) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Some(g) = f.get("nexthop_group") {
        let cbf = snap
            .get("APPL_DB", &format!("CLASS_BASED_NEXT_HOP_GROUP_TABLE:{g}"))
            .and_then(|c| c.get("members"));
        let groups: Vec<String> = match cbf {
            Some(members) => split(members).collect(),
            None => vec![g.clone()],
        };
        for name in groups {
            let nhg = snap.get("APPL_DB", &format!("NEXTHOP_GROUP_TABLE:{name}"));
            if let Some(nh) = nhg.and_then(|x| x.get("nexthop")) {
                out.extend(split(nh));
            }
        }
    } else if let Some(nh) = f.get("nexthop") {
        out.extend(split(nh));
    }
    out.retain(|ip| ip != "0.0.0.0" && ip != "::");
    out
}

/// Trace a route prefix through CONFIG_DB (static routes), APPL_DB and ASIC_DB,
/// including the SAI object its next hop resolves to.
pub fn trace(snap: &Snapshot, prefix: &str) -> Result<Report, String> {
    trace_with(snap, &Logs::default(), prefix)
}

/// Like [`trace`], adding evidence from logs when the route stopped at a stage.
pub fn trace_with(snap: &Snapshot, logs: &Logs, prefix: &str) -> Result<Report, String> {
    let valid = !prefix.is_empty()
        && prefix
            .chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, '.' | ':' | '/'));
    if !valid {
        return Err(format!("'{prefix}' is not a route prefix like 10.1.1.0/24"));
    }

    let cfg_key = format!("STATIC_ROUTE|{prefix}");
    let app_key = format!("ROUTE_TABLE:{prefix}");
    let cfg = snap.get("CONFIG_DB", &cfg_key);
    let app = snap.get("APPL_DB", &app_key);

    // ASIC route keys embed a JSON object: {"dest":"...","switch_id":"...","vr":"..."}.
    let mut asic_routes: Vec<(String, &Fields, String)> = Vec::new();
    for (k, f) in snap.iter("ASIC_DB") {
        let Some(json) = k.strip_prefix(ROUTE) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(json) else {
            continue;
        };
        if v.get("dest").and_then(Value::as_str) == Some(prefix) {
            let vr = v
                .get("vr")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            asic_routes.push((k.clone(), f, vr));
        }
    }
    let asic = asic_routes.first();

    let nh_oid = asic.and_then(|(_, f, _)| f.get("SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID"));
    let target = nh_oid.and_then(|oid| resolve(snap, oid));
    let expected = app.map(|f| appl_nexthops(snap, f)).unwrap_or_default();

    let pres = |b| {
        if b {
            Presence::Present
        } else {
            Presence::Missing
        }
    };
    let nh_stage = match (asic, nh_oid, &target) {
        (None, _, _) => (Presence::NotApplicable, "no ASIC route".to_string()),
        (Some((_, f, _)), None, _) => (
            Presence::NotApplicable,
            format!(
                "no next hop ({})",
                f.get("SAI_ROUTE_ENTRY_ATTR_PACKET_ACTION")
                    .map_or("no action", String::as_str)
            ),
        ),
        (Some(_), Some(oid), Some(t)) => (Presence::Present, format!("{} {oid}", t.kind)),
        (Some(_), Some(oid), None) => (Presence::Missing, format!("dangling next-hop {oid}")),
    };
    let stages = vec![
        StageResult {
            stage: stage("CONFIG_DB", "user/CLI"),
            presence: if cfg.is_some() {
                Presence::Present
            } else {
                Presence::NotApplicable
            },
            detail: if cfg.is_some() {
                cfg_key.clone()
            } else {
                "not a static route".into()
            },
        },
        StageResult {
            stage: stage("APPL_DB", "fpmsyncd"),
            presence: pres(app.is_some()),
            detail: app_key,
        },
        StageResult {
            stage: stage("ASIC_ROUTE", "orchagent"),
            presence: pres(asic.is_some()),
            detail: asic
                .map(|(_, _, vr)| format!("ROUTE_ENTRY dest={prefix} vr={vr}"))
                .unwrap_or_else(|| format!("SAI route entry for {prefix}")),
        },
        StageResult {
            stage: stage("ASIC_NEXTHOP", "orchagent"),
            presence: nh_stage.0,
            detail: nh_stage.1,
        },
    ];

    let mut evidence = Vec::new();
    if asic_routes.len() > 1 {
        evidence.push(format!(
            "prefix found in {} virtual routers; showing the first",
            asic_routes.len()
        ));
    }
    let (stopped_at, message) = match (app, asic) {
        (None, None) => (
            Some("APPL_DB"),
            if cfg.is_some() {
                "static route is configured but not in APPL_DB or ASIC_DB (check zebra and fpmsyncd)".to_string()
            } else {
                format!("{prefix} was not found in CONFIG_DB, APPL_DB or ASIC_DB")
            },
        ),
        (Some(f), None) => {
            if let Some(g) = f.get("nexthop_group") {
                evidence.push(format!("route uses nexthop group {g}"));
            }
            for ip in &expected {
                let known = snap.iter("ASIC_DB").any(|(k, f)| {
                    k.starts_with("ASIC_STATE:SAI_OBJECT_TYPE_NEXT_HOP:")
                        && f.get("SAI_NEXT_HOP_ATTR_IP") == Some(ip)
                });
                if !known {
                    evidence.push(format!(
                        "no SAI next hop exists for {ip}; its neighbor may be unresolved (check ARP/NDP)"
                    ));
                }
            }
            (
                Some("ASIC_DB"),
                "in APPL_DB but not in ASIC_DB; orchagent has not programmed it (check swss.rec / syslog)"
                    .to_string(),
            )
        }
        (None, Some(_)) if cfg.is_some() => (
            Some("APPL_DB"),
            "static route is configured and in ASIC_DB, but APPL_DB has no entry: the databases disagree".to_string(),
        ),
        (None, Some(_)) => (
            None,
            "in ASIC_DB with no APPL_DB entry (normal for connected and local routes that orchagent creates itself)"
                .to_string(),
        ),
        (Some(_), Some(_)) => match &target {
            None if nh_oid.is_some() => (
                Some("ASIC_NEXTHOP"),
                "the ASIC route points at a next-hop OID that does not exist in ASIC_DB".to_string(),
            ),
            Some(t) if !expected.is_empty() && expected != t.ips => {
                evidence.push(format!("next hops differ: APPL_DB {{{}}}, ASIC_DB {{{}}}", join(&expected), join(&t.ips)));
                (Some("ASIC_NEXTHOP"), "programmed, but the next hops differ from APPL_DB".to_string())
            }
            Some(t) => {
                if t.kind == "NEXT_HOP_GROUP" {
                    evidence.push(format!("ECMP group with {} members: {}", t.members, join(&t.ips)));
                }
                (None, "programmed through every stage".to_string())
            }
            None => (None, "programmed through every stage (no next hop, e.g. a drop route)".to_string()),
        },
    };

    if stopped_at.is_some() {
        let dest = format!("\"dest\":\"{prefix}\"");
        evidence.extend(logs.evidence(
            &format!("ROUTE_TABLE:{prefix}"),
            &|r| r.key.starts_with("SAI_OBJECT_TYPE_ROUTE_ENTRY:") && r.key.contains(&dest),
            &[prefix],
        ));
    }

    Ok(Report {
        object: prefix.into(),
        stages,
        diagnosis: Diagnosis {
            stopped_at: stopped_at.map(Into::into),
            message,
            evidence,
        },
    })
}
