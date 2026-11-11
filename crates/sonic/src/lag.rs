use crate::logs::Logs;
use crate::snapshot::Snapshot;
use std::collections::{BTreeMap, BTreeSet};
use trace_core::{Diagnosis, Presence, Report, Stage, StageResult};

const SAI_LAG: &str = "ASIC_STATE:SAI_OBJECT_TYPE_LAG:";
const SAI_LAG_MEMBER: &str = "ASIC_STATE:SAI_OBJECT_TYPE_LAG_MEMBER:";
const HOSTIF: &str = "ASIC_STATE:SAI_OBJECT_TYPE_HOSTIF:";

fn stage(name: &str, owner: &str) -> Stage {
    Stage {
        name: name.into(),
        owner: owner.into(),
    }
}

fn pres(b: bool) -> Presence {
    if b {
        Presence::Present
    } else {
        Presence::Missing
    }
}

fn members(snap: &Snapshot, db: &str, prefix: &str) -> BTreeSet<String> {
    snap.iter(db)
        .filter_map(|(k, _)| k.strip_prefix(prefix).map(String::from))
        .collect()
}

fn join(set: &BTreeSet<String>) -> String {
    set.iter().cloned().collect::<Vec<_>>().join(", ")
}

/// Member port names of every ASIC LAG, keyed by LAG OID. SAI LAG objects carry no
/// name, so members are resolved through the host interface that names each port.
fn asic_lags(snap: &Snapshot) -> BTreeMap<String, BTreeSet<String>> {
    let mut port_names = BTreeMap::new();
    for (k, f) in snap.iter("ASIC_DB") {
        if k.starts_with(HOSTIF)
            && let (Some(name), Some(oid)) = (
                f.get("SAI_HOSTIF_ATTR_NAME"),
                f.get("SAI_HOSTIF_ATTR_OBJ_ID"),
            )
        {
            port_names.insert(oid.clone(), name.clone());
        }
    }
    let mut lags: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (k, _) in snap.iter("ASIC_DB") {
        if let Some(oid) = k.strip_prefix(SAI_LAG) {
            lags.entry(oid.to_string()).or_default();
        }
    }
    for (k, f) in snap.iter("ASIC_DB") {
        if k.starts_with(SAI_LAG_MEMBER)
            && let (Some(lag), Some(port)) = (
                f.get("SAI_LAG_MEMBER_ATTR_LAG_ID"),
                f.get("SAI_LAG_MEMBER_ATTR_PORT_ID"),
            )
        {
            let name = port_names.get(port).unwrap_or(port).clone();
            lags.entry(lag.clone()).or_default().insert(name);
        }
    }
    lags
}

/// Trace a `PortChannel<n>` through CONFIG_DB, APPL_DB, STATE_DB and ASIC_DB, comparing
/// its member ports at each stage.
pub fn trace(snap: &Snapshot, name: &str) -> Result<Report, String> {
    trace_with(snap, &Logs::default(), name)
}

/// Like [`trace`], adding evidence from logs when the LAG stopped at a stage.
pub fn trace_with(snap: &Snapshot, logs: &Logs, name: &str) -> Result<Report, String> {
    let valid = name
        .strip_prefix("PortChannel")
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    if !valid {
        return Err(format!("'{name}' is not a LAG name like PortChannel001"));
    }
    let cfg_key = format!("PORTCHANNEL|{name}");
    let app_key = format!("LAG_TABLE:{name}");
    let state_key = format!("LAG_TABLE|{name}");
    let in_cfg = snap.get("CONFIG_DB", &cfg_key).is_some();
    let in_app = snap.get("APPL_DB", &app_key).is_some();
    let in_state = snap.get("STATE_DB", &state_key).is_some();

    let cfg_m = members(snap, "CONFIG_DB", &format!("PORTCHANNEL_MEMBER|{name}|"));
    let app_m = members(snap, "APPL_DB", &format!("LAG_MEMBER_TABLE:{name}:"));
    let state_m = members(snap, "STATE_DB", &format!("LAG_MEMBER_TABLE|{name}|"));

    // Identify the ASIC LAG by its members: exact match first, else the best overlap.
    let expected = if app_m.is_empty() { &cfg_m } else { &app_m };
    let lags = asic_lags(snap);
    let exact = lags.iter().find(|(_, s)| *s == expected);
    let best = lags
        .iter()
        .map(|(oid, s)| (oid, s, s.intersection(expected).count()))
        .filter(|(_, _, n)| *n > 0)
        .max_by_key(|(_, _, n)| *n)
        .map(|(oid, s, _)| (oid, s));
    let found = if expected.is_empty() {
        None
    } else {
        exact.or(best)
    };

    let asic_stage = if expected.is_empty() {
        (
            Presence::NotApplicable,
            "an empty LAG has no name in SAI, so it cannot be identified in ASIC_DB".to_string(),
        )
    } else {
        match found {
            Some((oid, _)) => (Presence::Present, format!("{SAI_LAG}{oid}")),
            None => (
                Presence::Missing,
                format!(
                    "no SAI LAG contains the member ports {{{}}}",
                    join(expected)
                ),
            ),
        }
    };
    let stages = vec![
        StageResult {
            stage: stage("CONFIG_DB", "user/CLI"),
            presence: pres(in_cfg),
            detail: cfg_key,
        },
        StageResult {
            stage: stage("APPL_DB", "teammgrd"),
            presence: pres(in_app),
            detail: app_key,
        },
        StageResult {
            stage: stage("STATE_DB", "teamd"),
            presence: pres(in_state),
            detail: state_key,
        },
        StageResult {
            stage: stage("ASIC_LAG", "orchagent"),
            presence: asic_stage.0,
            detail: asic_stage.1,
        },
    ];

    let asic_m = found.map(|(_, s)| s.clone()).unwrap_or_default();
    let mut issues = Vec::new();
    for p in cfg_m.difference(&app_m) {
        issues.push(format!(
            "member {p} is in CONFIG_DB but not in APPL_DB (teamd may not have enrolled it)"
        ));
    }
    if !asic_m.is_empty() && !app_m.is_empty() && asic_m != app_m {
        issues.push(format!(
            "ASIC_DB LAG has {{{}}}, APPL_DB has {{{}}}",
            join(&asic_m),
            join(&app_m)
        ));
    }

    let (stopped_at, message) = if !in_cfg {
        (
            Some("CONFIG_DB"),
            format!("{name} is not configured in CONFIG_DB"),
        )
    } else if !in_app {
        (
            Some("APPL_DB"),
            if in_state {
                "configured and in STATE_DB, but APPL_DB has no entry: the databases disagree"
                    .to_string()
            } else {
                "configured but not published to APPL_DB (teammgrd or the teamd container may not be running)"
                    .to_string()
            },
        )
    } else if cfg_m.difference(&app_m).next().is_some() {
        (
            Some("APPL_DB"),
            "some configured member ports are missing from APPL_DB".to_string(),
        )
    } else if !expected.is_empty() && found.is_none() {
        (
            Some("ASIC_LAG"),
            "in APPL_DB, but no ASIC LAG contains its member ports; orchagent has not programmed it".to_string(),
        )
    } else if !asic_m.is_empty() && !app_m.is_empty() && asic_m != app_m {
        (
            Some("ASIC_LAG"),
            "programmed, but the ASIC LAG members differ from APPL_DB".to_string(),
        )
    } else if expected.is_empty() {
        (
            None,
            "present through APPL_DB; an empty LAG cannot be checked in ASIC_DB".to_string(),
        )
    } else {
        (None, "programmed through every stage".to_string())
    };

    let mut evidence = Vec::new();
    if in_cfg {
        evidence.push(format!(
            "members: CONFIG_DB {}, APPL_DB {}, STATE_DB {}, ASIC_DB {}",
            cfg_m.len(),
            app_m.len(),
            state_m.len(),
            asic_m.len()
        ));
    }
    evidence.extend(issues);
    if stopped_at.is_some() {
        let lag_key = found.map(|(oid, _)| format!("SAI_OBJECT_TYPE_LAG:{oid}"));
        let by_oid = |r: &crate::logs::SaiRec| Some(r.key) == lag_key.as_deref();
        let matcher: Option<&dyn Fn(&crate::logs::SaiRec) -> bool> =
            lag_key.as_ref().map(|_| &by_oid as _);
        evidence.extend(logs.evidence(&format!("LAG_TABLE:{name}"), matcher, &[name]));
    }

    Ok(Report {
        object: name.into(),
        stages,
        diagnosis: Diagnosis {
            stopped_at: stopped_at.map(Into::into),
            message,
            evidence,
        },
    })
}
