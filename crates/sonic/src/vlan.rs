use crate::logs::Logs;
use crate::snapshot::Snapshot;
use std::collections::BTreeSet;
use trace_core::{Diagnosis, Presence, Report, Stage, StageResult};

fn stage(name: &str, owner: &str) -> Stage {
    Stage {
        name: name.into(),
        owner: owner.into(),
    }
}

/// Port names of `<prefix><port>` keys, e.g. `VLAN_MEMBER|Vlan3|` -> {"Ethernet0", ..}.
fn members(snap: &Snapshot, db: &str, prefix: &str, need_ok: bool) -> BTreeSet<String> {
    snap.iter(db)
        .filter(|(_, f)| !need_ok || f.get("state").map(String::as_str) == Some("ok"))
        .filter_map(|(k, _)| k.strip_prefix(prefix).map(String::from))
        .collect()
}

fn missing_from(a: &BTreeSet<String>, b: &BTreeSet<String>, from: &str, to: &str) -> Vec<String> {
    a.difference(b)
        .map(|p| format!("member {p} is in {from} but not in {to}"))
        .collect()
}

/// Trace `Vlan<id>` from CONFIG_DB through APPL_DB and STATE_DB to ASIC_DB,
/// then compare its member ports across the same stages.
pub fn trace(snap: &Snapshot, name: &str) -> Result<Report, String> {
    trace_with(snap, &Logs::default(), name)
}

/// Like [`trace`], adding evidence from logs when the VLAN stopped at a stage.
pub fn trace_with(snap: &Snapshot, logs: &Logs, name: &str) -> Result<Report, String> {
    let id: u32 = name
        .strip_prefix("Vlan")
        .and_then(|n| n.parse().ok())
        .filter(|n| (1..=4094).contains(n))
        .ok_or_else(|| format!("'{name}' is not a VLAN name like Vlan100"))?;

    let cfg_key = format!("VLAN|{name}");
    let app_key = format!("VLAN_TABLE:{name}");
    let state_key = format!("VLAN_TABLE|{name}");
    let want = id.to_string();
    let asic = snap.iter("ASIC_DB").find(|(k, f)| {
        k.starts_with("ASIC_STATE:SAI_OBJECT_TYPE_VLAN:")
            && f.get("SAI_VLAN_ATTR_VLAN_ID") == Some(&want)
    });

    let in_cfg = snap.get("CONFIG_DB", &cfg_key).is_some();
    let in_app = snap.get("APPL_DB", &app_key).is_some();
    let in_asic = asic.is_some();
    let in_state = snap
        .get("STATE_DB", &state_key)
        .is_some_and(|f| f.get("state").map(String::as_str) == Some("ok"));

    let pres = |b| {
        if b {
            Presence::Present
        } else {
            Presence::Missing
        }
    };
    let stages = vec![
        StageResult {
            stage: stage("CONFIG_DB", "user/CLI"),
            presence: pres(in_cfg),
            detail: cfg_key.clone(),
        },
        StageResult {
            stage: stage("APPL_DB", "vlanmgrd"),
            presence: pres(in_app),
            detail: app_key.clone(),
        },
        StageResult {
            stage: stage("STATE_DB", "vlanmgrd"),
            presence: pres(in_state),
            detail: format!("{state_key} state=ok"),
        },
        StageResult {
            stage: stage("ASIC_DB", "orchagent"),
            presence: pres(in_asic),
            detail: asic
                .map(|(k, _)| k.clone())
                .unwrap_or_else(|| format!("SAI VLAN with VLAN_ID={id}")),
        },
    ];

    // Member ports. ASIC members only carry OIDs here, so compare by count.
    let cfg_m = members(snap, "CONFIG_DB", &format!("VLAN_MEMBER|{name}|"), false);
    let app_m = members(
        snap,
        "APPL_DB",
        &format!("VLAN_MEMBER_TABLE:{name}:"),
        false,
    );
    let state_m = members(
        snap,
        "STATE_DB",
        &format!("VLAN_MEMBER_TABLE|{name}|"),
        true,
    );
    let vlan_oid = asic.and_then(|(k, _)| {
        k.strip_prefix("ASIC_STATE:SAI_OBJECT_TYPE_VLAN:")
            .map(String::from)
    });
    let asic_n = vlan_oid.as_ref().map_or(0, |oid| {
        snap.iter("ASIC_DB")
            .filter(|(k, f)| {
                k.starts_with("ASIC_STATE:SAI_OBJECT_TYPE_VLAN_MEMBER:")
                    && f.get("SAI_VLAN_MEMBER_ATTR_VLAN_ID") == Some(oid)
            })
            .count()
    });
    let mut member_issues = Vec::new();
    member_issues.extend(missing_from(&cfg_m, &app_m, "CONFIG_DB", "APPL_DB"));
    member_issues.extend(missing_from(
        &app_m,
        &state_m,
        "APPL_DB",
        "STATE_DB (state=ok)",
    ));
    if in_asic && asic_n < app_m.len() {
        member_issues.push(format!(
            "ASIC_DB has {asic_n} of {} members present in APPL_DB",
            app_m.len()
        ));
    }

    let (stopped_at, message) = match (in_cfg, in_app, in_asic) {
        (false, _, _) => (
            Some("CONFIG_DB".into()),
            format!("{name} is not configured in CONFIG_DB"),
        ),
        (true, false, _) => (
            Some("APPL_DB".into()),
            if in_state {
                "configured and STATE_DB says ok, but APPL_DB has no entry: the databases disagree".into()
            } else {
                "configured but not propagated to APPL_DB; vlanmgrd did not publish it (is the swss container running?)".into()
            },
        ),
        (true, true, false) => (
            Some("ASIC_DB".into()),
            "in APPL_DB but not in ASIC_DB; orchagent has not programmed it (check swss.rec / syslog)".into(),
        ),
        (true, true, true) if !in_state => (
            None,
            "programmed in ASIC_DB, but vlanmgrd has not reported state=ok in STATE_DB".into(),
        ),
        (true, true, true) if !member_issues.is_empty() => (
            Some("VLAN_MEMBER".into()),
            format!("{name} is programmed, but its member ports are inconsistent"),
        ),
        (true, true, true) => (None, "programmed through every stage".into()),
    };

    let mut evidence = member_issues;
    if in_cfg {
        evidence.insert(
            0,
            format!(
                "members: CONFIG_DB {}, APPL_DB {}, STATE_DB {}, ASIC_DB {}",
                cfg_m.len(),
                app_m.len(),
                state_m.len(),
                asic_n
            ),
        );
    }

    if stopped_at.is_some() {
        let want = format!("SAI_VLAN_ATTR_VLAN_ID={id}");
        evidence.extend(logs.evidence(
            &format!("VLAN_TABLE:{name}"),
            Some(&|r| {
                r.key.starts_with("SAI_OBJECT_TYPE_VLAN:") && r.attrs.contains(&want.as_str())
            }),
            &[name],
        ));
    }

    Ok(Report {
        object: name.into(),
        stages,
        diagnosis: Diagnosis {
            stopped_at,
            message,
            evidence,
        },
    })
}
