//! Route traces.
//!
//! Key formats (from sonic-swss fpmsyncd, sonic-sairedis and sonic-static-route.yang):
//! - APPL_DB:   `ROUTE_TABLE:[<vrf>:]<prefix>`, where a host route (/32, /128) is written
//!   **without** its mask, e.g. `ROUTE_TABLE:10.1.0.32`
//! - CONFIG_DB: `STATIC_ROUTE|<prefix>` or `STATIC_ROUTE|<vrf>|<prefix>`
//! - ASIC_DB:   `SAI_OBJECT_TYPE_ROUTE_ENTRY:{"dest":"<addr>/<mask>","switch_id":..,"vr":..}`,
//!   always with the mask, and with the virtual router instead of the VRF name
use crate::logs::Logs;
use crate::snapshot::{Fields, Snapshot};
use serde_json::Value;
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv6Addr};
use trace_core::{Diagnosis, Presence, Report, Stage, StageResult};

const ROUTE: &str = "ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:";
const NH_GROUP_MEMBER: &str = "ASIC_STATE:SAI_OBJECT_TYPE_NEXT_HOP_GROUP_MEMBER:";
const VIRTUAL_ROUTER: &str = "ASIC_STATE:SAI_OBJECT_TYPE_VIRTUAL_ROUTER:";
const RIF: &str = "ASIC_STATE:SAI_OBJECT_TYPE_ROUTER_INTERFACE:";
const HOSTIF: &str = "ASIC_STATE:SAI_OBJECT_TYPE_HOSTIF:";
const SAI_VLAN: &str = "ASIC_STATE:SAI_OBJECT_TYPE_VLAN:";

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

/// A parsed route target: `[Vrf-name:]<address>[/<mask>]`.
struct RouteName {
    vrf: Option<String>,
    /// Canonical address text, as `inet_ntop` would print it.
    addr: String,
    mask: u8,
    host: bool,
}

impl RouteName {
    fn parse(s: &str) -> Result<Self, String> {
        let bad =
            || format!("'{s}' is not a route like 10.1.1.0/24, 10.1.0.32 or Vrf-red:10.1.1.0/24");
        // SONiC VRF names start with "Vrf"; an IPv6 address never does.
        let (vrf, prefix) = match s.strip_prefix("Vrf") {
            Some(_) => {
                let (v, p) = s.split_once(':').ok_or_else(bad)?;
                let ok = v.len() > 3
                    && v.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
                if !ok {
                    return Err(bad());
                }
                (Some(v.to_string()), p)
            }
            None => (None, s),
        };
        let (addr, mask) = match prefix.split_once('/') {
            Some((a, m)) => (a, Some(m)),
            None => (prefix, None),
        };
        let addr: IpAddr = addr.parse().map_err(|_| bad())?;
        let max: u8 = if addr.is_ipv4() { 32 } else { 128 };
        let mask = match mask {
            Some(m) => m.parse::<u8>().ok().filter(|m| *m <= max).ok_or_else(bad)?,
            None => max,
        };
        Ok(Self {
            vrf,
            addr: addr.to_string(),
            mask,
            host: mask == max,
        })
    }

    /// `<addr>/<mask>`, as ASIC_DB and CONFIG_DB write it.
    fn with_mask(&self) -> String {
        format!("{}/{}", self.addr, self.mask)
    }

    fn vrf_part(&self) -> String {
        self.vrf
            .as_ref()
            .map(|v| format!("{v}:"))
            .unwrap_or_default()
    }

    /// APPL_DB keys to try: host routes are written without the mask; accept both.
    fn appl_keys(&self) -> Vec<String> {
        let vrf = self.vrf_part();
        let mut forms = Vec::new();
        if self.host {
            forms.push(format!("ROUTE_TABLE:{vrf}{}", self.addr));
        }
        forms.push(format!("ROUTE_TABLE:{vrf}{}", self.with_mask()));
        forms
    }

    fn config_keys(&self) -> Vec<String> {
        let p = self.with_mask();
        match &self.vrf {
            None => vec![
                format!("STATIC_ROUTE|{p}"),
                format!("STATIC_ROUTE|default|{p}"),
            ],
            Some(v) => vec![format!("STATIC_ROUTE|{v}|{p}")],
        }
    }

    fn display(&self) -> String {
        format!("{}{}", self.vrf_part(), self.with_mask())
    }
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

/// The default virtual router's OID. syncd stores the switch's default VR as a real ID
/// under `HIDDEN` (`SAI_SWITCH_ATTR_DEFAULT_VIRTUAL_ROUTER_ID`), and `RIDTOVID` maps it
/// to the OID that `ASIC_STATE` uses. Without those, a single virtual router must be it.
fn default_vr(snap: &Snapshot) -> Option<String> {
    let rid_to_vid = snap.get("ASIC_DB", "RIDTOVID");
    for (k, f) in snap.iter("ASIC_DB") {
        if (k == "HIDDEN" || k.starts_with("HIDDEN:"))
            && let Some(rid) = f.get("SAI_SWITCH_ATTR_DEFAULT_VIRTUAL_ROUTER_ID")
            && let Some(vid) = rid_to_vid.and_then(|m| m.get(rid))
        {
            return Some(vid.clone());
        }
    }
    let vrs: Vec<&str> = snap
        .iter("ASIC_DB")
        .filter_map(|(k, _)| k.strip_prefix(VIRTUAL_ROUTER))
        .collect();
    match vrs.as_slice() {
        [only] => Some((*only).to_string()),
        _ => None,
    }
}

/// Whether `addr` is an IPv6 link-local address (`fe80::/10`).
fn is_link_local(addr: &str) -> bool {
    addr.parse::<Ipv6Addr>()
        .is_ok_and(|a| a.segments().first().is_some_and(|s| s & 0xffc0 == 0xfe80))
}

/// Whether ASIC_DB has the `fe80::/10` route that orchagent creates in each virtual router
/// (routeorch for the default VRF, vrforch for the others) to send link-local traffic to
/// the CPU. `vrs` limits the search to those virtual routers, when known.
fn has_link_local_cpu_route(snap: &Snapshot, vrs: Option<&BTreeSet<String>>) -> bool {
    snap.iter("ASIC_DB").any(|(k, _)| {
        let Some(json) = k.strip_prefix(ROUTE) else {
            return false;
        };
        let Ok(v) = serde_json::from_str::<Value>(json) else {
            return false;
        };
        let vr = v.get("vr").and_then(Value::as_str).unwrap_or("");
        v.get("dest").and_then(Value::as_str) == Some("fe80::/10")
            && vrs.is_none_or(|set| set.contains(vr))
    })
}

/// Virtual-router OIDs that belong to `vrf`. SAI objects carry no VRF name, so this goes
/// through the VRF's interfaces: `INTF_TABLE:<ifname>` with `vrf_name` in APPL_DB, the
/// interface's ASIC object (port, VLAN or LAG), and the router interface built on it.
fn vrf_vrs(snap: &Snapshot, vrf: &str) -> BTreeSet<String> {
    let ifnames: Vec<String> = snap
        .iter("APPL_DB")
        .filter_map(|(k, f)| {
            let name = k.strip_prefix("INTF_TABLE:")?;
            let in_vrf = f.get("vrf_name").map(String::as_str) == Some(vrf);
            (!name.contains(':') && in_vrf).then(|| name.to_string())
        })
        .collect();
    let mut targets = BTreeSet::new();
    for name in &ifnames {
        let oid = if name.starts_with("Ethernet") {
            snap.iter("ASIC_DB").find_map(|(k, f)| {
                let named = f.get("SAI_HOSTIF_ATTR_NAME").map(String::as_str) == Some(name);
                (k.starts_with(HOSTIF) && named)
                    .then(|| f.get("SAI_HOSTIF_ATTR_OBJ_ID").cloned())
                    .flatten()
            })
        } else if let Some(id) = name.strip_prefix("Vlan") {
            snap.iter("ASIC_DB").find_map(|(k, f)| {
                let vid = f.get("SAI_VLAN_ATTR_VLAN_ID").map(String::as_str) == Some(id);
                if vid {
                    k.strip_prefix(SAI_VLAN).map(String::from)
                } else {
                    None
                }
            })
        } else if name.starts_with("PortChannel") {
            crate::lag::lag_oid(snap, name)
        } else {
            None
        };
        targets.extend(oid);
    }
    snap.iter("ASIC_DB")
        .filter(|(k, f)| {
            k.starts_with(RIF)
                && [
                    "SAI_ROUTER_INTERFACE_ATTR_PORT_ID",
                    "SAI_ROUTER_INTERFACE_ATTR_VLAN_ID",
                ]
                .iter()
                .any(|a| f.get(*a).is_some_and(|o| targets.contains(o)))
        })
        .filter_map(|(_, f)| {
            f.get("SAI_ROUTER_INTERFACE_ATTR_VIRTUAL_ROUTER_ID")
                .cloned()
        })
        .collect()
}

/// Trace a route through CONFIG_DB (static routes), APPL_DB and ASIC_DB, including the
/// SAI object its next hop resolves to. `target` is `[Vrf-name:]<address>[/<mask>]`.
pub fn trace(snap: &Snapshot, target: &str) -> Result<Report, String> {
    trace_with(snap, &Logs::default(), target)
}

/// Like [`trace`], adding evidence from logs when the route stopped at a stage.
pub fn trace_with(snap: &Snapshot, logs: &Logs, target: &str) -> Result<Report, String> {
    let route = RouteName::parse(target)?;
    let dest = route.with_mask();

    let cfg = route
        .config_keys()
        .into_iter()
        .find_map(|k| snap.get("CONFIG_DB", &k).map(|f| (k, f)));
    let app_keys = route.appl_keys();
    let app = app_keys
        .iter()
        .find_map(|k| snap.get("APPL_DB", k).map(|f| (k.clone(), f)));

    // ASIC routes for this prefix, in any virtual router.
    let mut all: Vec<(&Fields, String)> = Vec::new();
    for (k, f) in snap.iter("ASIC_DB") {
        let Some(json) = k.strip_prefix(ROUTE) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(json) else {
            continue;
        };
        if v.get("dest").and_then(Value::as_str) == Some(dest.as_str()) {
            let vr = v
                .get("vr")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            all.push((f, vr));
        }
    }

    // Keep only the virtual router(s) that belong to the requested VRF.
    let mut evidence = Vec::new();
    let default = default_vr(snap);
    // The virtual routers this VRF maps to, when known; used for ASIC_DB and for logs.
    let mut wanted: Option<BTreeSet<String>> = None;
    let candidates: Vec<(&Fields, String)> = match (&route.vrf, &default) {
        (None, Some(d)) => {
            wanted = Some(BTreeSet::from([d.clone()]));
            all.into_iter().filter(|(_, vr)| vr == d).collect()
        }
        (None, None) => {
            if !all.is_empty() {
                evidence.push(
                    "the default virtual router could not be identified (no HIDDEN/RIDTOVID entries), so the ASIC route shown may belong to another VRF".to_string(),
                );
            }
            all
        }
        (Some(v), _) => {
            let vrs = vrf_vrs(snap, v);
            if !vrs.is_empty() {
                let found = all.into_iter().filter(|(_, vr)| vrs.contains(vr)).collect();
                wanted = Some(vrs);
                found
            } else {
                let others: Vec<_> = match &default {
                    Some(d) => all.into_iter().filter(|(_, vr)| vr != d).collect(),
                    None => all,
                };
                if !others.is_empty() {
                    evidence.push(format!(
                        "could not map {v} to a virtual router (no interface of it found); matched a non-default one instead"
                    ));
                }
                others
            }
        }
    };
    // A default-VRF query that finds nothing may be looking in the wrong VRF.
    if route.vrf.is_none() {
        let forms: Vec<String> = app_keys
            .iter()
            .filter_map(|k| k.strip_prefix("ROUTE_TABLE:").map(|f| format!(":{f}")))
            .collect();
        let others: BTreeSet<String> = snap
            .iter("APPL_DB")
            .filter_map(|(k, _)| {
                let rest = k.strip_prefix("ROUTE_TABLE:Vrf")?;
                let f = forms.iter().find(|f| rest.ends_with(f.as_str()))?;
                rest.strip_suffix(f.as_str()).map(|v| format!("Vrf{v}"))
            })
            .collect();
        if !others.is_empty() {
            evidence.push(format!(
                "APPL_DB has this prefix in {}; trace it as <VRF>:{}",
                join(&others),
                route.with_mask()
            ));
        }
    }
    if candidates.len() > 1 {
        evidence.push(format!(
            "prefix is in {} matching virtual routers; showing the first",
            candidates.len()
        ));
    }
    let asic = candidates.first();

    let nh_oid = asic.and_then(|(f, _)| f.get("SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID"));
    let target_obj = nh_oid.and_then(|oid| resolve(snap, oid));
    let expected = app
        .as_ref()
        .map(|(_, f)| appl_nexthops(snap, f))
        .unwrap_or_default();

    let pres = |b| {
        if b {
            Presence::Present
        } else {
            Presence::Missing
        }
    };
    let nh_stage = match (asic, nh_oid, &target_obj) {
        (None, _, _) => (Presence::NotApplicable, "no ASIC route".to_string()),
        (Some((f, _)), None, _) => (
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
    // orchagent does not program link-local routes from APPL_DB one by one: a single
    // fe80::/10 route to the CPU per virtual router covers them, and route_check.py skips
    // them for the same reason. `Some(covered)` when that applies to this trace.
    let link_local = (app.is_some() && asic.is_none() && is_link_local(&route.addr))
        .then(|| has_link_local_cpu_route(snap, wanted.as_ref()));
    let primary_app = app_keys.first().cloned().unwrap_or_default();
    let stages = vec![
        StageResult {
            stage: stage("CONFIG_DB", "user/CLI"),
            presence: if cfg.is_some() {
                Presence::Present
            } else {
                Presence::NotApplicable
            },
            detail: cfg
                .as_ref()
                .map_or_else(|| "not a static route".to_string(), |(k, _)| k.clone()),
        },
        StageResult {
            stage: stage("APPL_DB", "fpmsyncd"),
            presence: pres(app.is_some()),
            detail: app.as_ref().map_or(primary_app, |(k, _)| k.clone()),
        },
        StageResult {
            stage: stage("ASIC_ROUTE", "orchagent"),
            presence: if link_local == Some(true) {
                Presence::NotApplicable
            } else {
                pres(asic.is_some())
            },
            detail: match (asic, link_local) {
                (Some((_, vr)), _) => format!("ROUTE_ENTRY dest={dest} vr={vr}"),
                (None, Some(true)) => "link-local: covered by the fe80::/10 CPU route".to_string(),
                (None, _) => format!("SAI route entry for {dest}"),
            },
        },
        StageResult {
            stage: stage("ASIC_NEXTHOP", "orchagent"),
            presence: nh_stage.0,
            detail: nh_stage.1,
        },
    ];

    let (stopped_at, message) = match (&app, asic) {
        (None, None) => (
            Some("APPL_DB"),
            if cfg.is_some() {
                "static route is configured but not in APPL_DB or ASIC_DB (check zebra and fpmsyncd)".to_string()
            } else {
                format!("{} was not found in CONFIG_DB, APPL_DB or ASIC_DB", route.display())
            },
        ),
        (Some(_), None) if link_local == Some(true) => (
            None,
            "link-local route: orchagent sends all of fe80::/10 to the CPU instead of programming each one (normal)".to_string(),
        ),
        (Some(_), None) if link_local == Some(false) => (
            Some("ASIC_DB"),
            "link-local route, but this VRF has no fe80::/10 route to the CPU in ASIC_DB (orchagent creates one per VRF)".to_string(),
        ),
        (Some((_, f)), None) => {
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
        // Interface subnet and local (IP2ME) routes are created by orchagent itself and
        // point at a router interface or the CPU port. A route through a real next hop
        // normally comes from APPL_DB, so its absence there is suspicious.
        (None, Some(_)) => match &target_obj {
            Some(t) if t.kind == "NEXT_HOP" || t.kind == "NEXT_HOP_GROUP" => (
                Some("APPL_DB"),
                "in ASIC_DB through a next hop, but APPL_DB has no entry: possibly a stale route whose removal was not processed".to_string(),
            ),
            _ => (
                None,
                "in ASIC_DB with no APPL_DB entry (normal for connected and local routes that orchagent creates itself)"
                    .to_string(),
            ),
        },
        (Some(_), Some(_)) => match &target_obj {
            None if nh_oid.is_some() => (
                Some("ASIC_NEXTHOP"),
                "the ASIC route points at a next-hop OID that does not exist in ASIC_DB".to_string(),
            ),
            Some(t) if !expected.is_empty() && expected != t.ips => {
                evidence.push(format!(
                    "next hops differ: APPL_DB {{{}}}, ASIC_DB {{{}}}",
                    join(&expected),
                    join(&t.ips)
                ));
                (
                    Some("ASIC_NEXTHOP"),
                    "programmed, but the next hops differ from APPL_DB".to_string(),
                )
            }
            Some(t) => {
                if t.kind == "NEXT_HOP_GROUP" {
                    evidence.push(format!(
                        "ECMP group with {} members: {}",
                        t.members,
                        join(&t.ips)
                    ));
                }
                (None, "programmed through every stage".to_string())
            }
            None => (
                None,
                "programmed through every stage (no next hop, e.g. a drop route)".to_string(),
            ),
        },
    };

    if stopped_at.is_some() {
        let dest_json = format!("\"dest\":\"{dest}\"");
        // Match the log entry to the virtual router: the one found in ASIC_DB, else the
        // ones the VRF maps to, else any.
        let vr_json: Option<Vec<String>> = match asic {
            Some((_, vr)) => Some(vec![format!("\"vr\":\"{vr}\"")]),
            None => wanted
                .as_ref()
                .map(|set| set.iter().map(|vr| format!("\"vr\":\"{vr}\"")).collect()),
        };
        let swss_keys: Vec<&str> = app_keys.iter().map(String::as_str).collect();
        let mut tokens = vec![dest.as_str()];
        if route.host {
            tokens.push(route.addr.as_str());
        }
        evidence.extend(logs.evidence(
            &swss_keys,
            Some(&|r| {
                r.key.starts_with("SAI_OBJECT_TYPE_ROUTE_ENTRY:")
                    && r.key.contains(&dest_json)
                    && vr_json
                        .as_ref()
                        .is_none_or(|vrs| vrs.iter().any(|v| r.key.contains(v.as_str())))
            }),
            &tokens,
        ));
    }

    Ok(Report {
        object: route.display(),
        stages,
        diagnosis: Diagnosis {
            stopped_at: stopped_at.map(Into::into),
            message,
            evidence,
        },
    })
}
