use crate::logs::Logs;
use crate::snapshot::Snapshot;
use trace_core::{Diagnosis, Presence, Report, Stage, StageResult};

const HOSTIF: &str = "ASIC_STATE:SAI_OBJECT_TYPE_HOSTIF:";
const SAI_PORT: &str = "ASIC_STATE:SAI_OBJECT_TYPE_PORT:";

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

/// Trace an `Ethernet<n>` port from CONFIG_DB through APPL_DB and STATE_DB to
/// its ASIC host interface and SAI port object, then compare admin state and speed.
pub fn trace(snap: &Snapshot, name: &str) -> Result<Report, String> {
    trace_with(snap, &Logs::default(), name)
}

/// Like [`trace`], adding evidence from logs when the port stopped at a stage.
pub fn trace_with(snap: &Snapshot, logs: &Logs, name: &str) -> Result<Report, String> {
    let valid = name
        .strip_prefix("Ethernet")
        .is_some_and(|n| n.parse::<u32>().is_ok());
    if !valid {
        return Err(format!("'{name}' is not a port name like Ethernet0"));
    }
    let cfg_key = format!("PORT|{name}");
    let app_key = format!("PORT_TABLE:{name}");
    let state_key = format!("PORT_TABLE|{name}");

    let cfg = snap.get("CONFIG_DB", &cfg_key);
    let in_app = snap.get("APPL_DB", &app_key).is_some();
    let in_state = snap
        .get("STATE_DB", &state_key)
        .is_some_and(|f| f.get("state").map(String::as_str) == Some("ok"));

    // The host interface carries the port name and points at the SAI port OID.
    let hostif = snap.iter("ASIC_DB").find(|(k, f)| {
        k.starts_with(HOSTIF) && f.get("SAI_HOSTIF_ATTR_NAME").map(String::as_str) == Some(name)
    });
    let port_oid = hostif
        .and_then(|(_, f)| f.get("SAI_HOSTIF_ATTR_OBJ_ID"))
        .cloned();
    let sai_key = port_oid.as_ref().map(|o| format!("{SAI_PORT}{o}"));
    let sai = sai_key.as_deref().and_then(|k| snap.get("ASIC_DB", k));

    let stages = vec![
        StageResult {
            stage: stage("CONFIG_DB", "user/CLI"),
            presence: pres(cfg.is_some()),
            detail: cfg_key,
        },
        StageResult {
            stage: stage("APPL_DB", "portmgrd"),
            presence: pres(in_app),
            detail: app_key,
        },
        StageResult {
            stage: stage("STATE_DB", "portsyncd"),
            presence: pres(in_state),
            detail: if in_state {
                format!("{state_key} state=ok")
            } else {
                state_key.clone()
            },
        },
        StageResult {
            stage: stage("ASIC_HOSTIF", "orchagent"),
            presence: pres(hostif.is_some()),
            detail: hostif
                .map(|(k, _)| k.clone())
                .unwrap_or_else(|| format!("SAI host interface named {name}")),
        },
        StageResult {
            stage: stage("ASIC_PORT", "orchagent"),
            presence: pres(sai.is_some()),
            detail: sai_key.clone().unwrap_or_else(|| "SAI port object".into()),
        },
    ];

    // Compare what the user configured with what the ASIC has.
    let mut diffs = Vec::new();
    if let (Some(cfg), Some(sai)) = (cfg, sai) {
        if let (Some(c), Some(a)) = (cfg.get("speed"), sai.get("SAI_PORT_ATTR_SPEED"))
            && c != a
        {
            diffs.push(format!("speed: CONFIG_DB {c}, ASIC_DB {a}"));
        }
        if let (Some(c), Some(a)) = (
            cfg.get("admin_status"),
            sai.get("SAI_PORT_ATTR_ADMIN_STATE"),
        ) {
            let want = if c == "up" { "true" } else { "false" };
            if want != a {
                diffs.push(format!(
                    "admin_status: CONFIG_DB {c}, ASIC_DB admin_state={a}"
                ));
            }
        }
    }

    let (stopped_at, message) = if cfg.is_none() {
        (
            Some("CONFIG_DB"),
            format!("{name} is not configured in CONFIG_DB"),
        )
    } else if !in_app {
        (
            Some("APPL_DB"),
            if in_state {
                "configured and STATE_DB says ok, but APPL_DB has no entry: the databases disagree"
                    .to_string()
            } else {
                "configured but not published to APPL_DB (portmgrd or the swss container may not be running)"
                    .to_string()
            },
        )
    } else if hostif.is_none() {
        (
            Some("ASIC_HOSTIF"),
            "in APPL_DB but no ASIC host interface carries this name; orchagent has not created the netdev"
                .to_string(),
        )
    } else if sai.is_none() {
        (
            Some("ASIC_PORT"),
            format!(
                "host interface points at {} but ASIC_DB has no SAI port object with that OID",
                port_oid.as_deref().unwrap_or("?")
            ),
        )
    } else if !diffs.is_empty() {
        (
            Some("ASIC_PORT"),
            "programmed, but settings differ from CONFIG_DB".to_string(),
        )
    } else if !in_state {
        (
            None,
            "programmed in ASIC_DB, but STATE_DB does not show state=ok".to_string(),
        )
    } else {
        (None, "programmed through every stage".to_string())
    };

    let mut evidence = diffs;
    if stopped_at.is_some() {
        let want = format!("SAI_HOSTIF_ATTR_NAME={name}");
        evidence.extend(logs.evidence(
            &format!("PORT_TABLE:{name}"),
            Some(&|r| {
                r.key.starts_with("SAI_OBJECT_TYPE_HOSTIF:") && r.attrs.contains(&want.as_str())
            }),
            &[name],
        ));
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
