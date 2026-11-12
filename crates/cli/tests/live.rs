//! `live`: end to end against a fake `redis-cli` that plays back a made-up switch session.
//! All names, addresses and OIDs are invented.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::string_slice
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sonic-trace-live-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A fake redis-cli. Lookups are answered from a table; `psubscribe` plays a session:
/// Vlan100 is created and gets member Ethernet4 (both reach the ASIC); route 10.9.0.0/24
/// reaches APPL_DB but never the ASIC, and orchagent logs an error for it.
fn fake_redis(dir: &Path) -> PathBuf {
    let logs = dir.display();
    let script = format!(
        r#"#!/bin/sh
args="$*"
case "$args" in
  *"config get notify-keyspace-events"*) printf 'notify-keyspace-events\nAKE\n'; exit 0 ;;
  *"hgetall COUNTERS_PORT_NAME_MAP"*) printf 'Ethernet4\noid:0x1004\nEthernet8\noid:0x1008\n'; exit 0 ;;
  *"hgetall COUNTERS_LAG_NAME_MAP"*) exit 0 ;;
  *"hget ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x2600 SAI_VLAN_ATTR_VLAN_ID"*) echo 100; exit 0 ;;
  *"hget ASIC_STATE:SAI_OBJECT_TYPE_VLAN_MEMBER:oid:0x2700 SAI_VLAN_MEMBER_ATTR_VLAN_ID"*) echo oid:0x2600; exit 0 ;;
  *"hget ASIC_STATE:SAI_OBJECT_TYPE_VLAN_MEMBER:oid:0x2700 SAI_VLAN_MEMBER_ATTR_BRIDGE_PORT_ID"*) echo oid:0x3a00; exit 0 ;;
  *"hget ASIC_STATE:SAI_OBJECT_TYPE_BRIDGE_PORT:oid:0x3a00 SAI_BRIDGE_PORT_ATTR_PORT_ID"*) echo oid:0x1004; exit 0 ;;
  *"hget "*|*"keys "*) exit 0 ;;
  *psubscribe*) ;;
  *) exit 1 ;;
esac
ev() {{ printf '"pmessage","__keyspace@*__:*","__keyspace@%s__:%s","%s"\n' "$1" "$2" "$3"; }}
echo '"psubscribe","__keyspace@4__:*",1'
sleep 0.3
ev 4 'VLAN|Vlan100' hset
ev 0 '_VLAN_TABLE:Vlan100' hset
ev 0 'VLAN_TABLE_KEY_SET' sadd
ev 0 'VLAN_TABLE:Vlan100' hset
ev 0 '_VLAN_TABLE:Vlan100' del
ev 6 'VLAN_TABLE|Vlan100' hset
echo '2000-01-01.00:00:00.100000|VLAN_TABLE:Vlan100|SET|admin_status:up' >> {logs}/swss.rec
echo '2000-01-01.00:00:00.110000|c|SAI_OBJECT_TYPE_VLAN:oid:0x2600|SAI_VLAN_ATTR_VLAN_ID=100' >> {logs}/sairedis.rec
sleep 0.3
ev 1 'ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x2600' hset
ev 4 'VLAN_MEMBER|Vlan100|Ethernet4' hset
ev 0 'VLAN_MEMBER_TABLE:Vlan100:Ethernet4' hset
ev 1 'ASIC_STATE:SAI_OBJECT_TYPE_VLAN_MEMBER:oid:0x2700' hset
ev 0 'NEIGH_TABLE:Vlan100:10.8.0.5' hset
ev 2 'COUNTERS:oid:0x1004' hset
ev 0 'ROUTE_TABLE:10.9.0.0/24' hset
echo '2000 Jan 01 00:00:01.000000 host ERR swss#orchagent: :- addRoute: Failed to create route 10.9.0.0/24' >> {logs}/syslog
echo '2000 Jan 01 00:00:01.000000 host ERR swss#orchagent: unrelated failure for 10.99.0.0/24' >> {logs}/syslog
sleep 1.6
exit 0
"#
    );
    let path = dir.join("redis-cli");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn run_live(dir: &Path, extra: &[&str]) -> (i32, String, String) {
    for f in ["swss.rec", "sairedis.rec", "syslog"] {
        std::fs::write(dir.join(f), "existing line, must not be shown\n").unwrap();
    }
    let redis = fake_redis(dir);
    let mut args: Vec<String> = vec![
        "--swss-rec".into(),
        dir.join("swss.rec").display().to_string(),
        "--sairedis-rec".into(),
        dir.join("sairedis.rec").display().to_string(),
        "--syslog".into(),
        dir.join("syslog").display().to_string(),
        "live".into(),
        "--stuck-after".into(),
        "1".into(),
        "--redis-cli".into(),
        redis.display().to_string(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let out = Command::new(env!("CARGO_BIN_EXE_sonic-trace"))
        .args(&args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn line_with<'a>(out: &'a str, parts: &[&str]) -> Option<&'a str> {
    out.lines().find(|l| parts.iter().all(|p| l.contains(p)))
}

#[test]
fn follows_a_session_from_config_to_asic() {
    let d = tmp("session");
    let (code, out, err) = run_live(&d, &[]);
    std::fs::remove_dir_all(&d).ok();
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(
        out.starts_with("following VLANs, ports, LAGs and routes"),
        "{out}"
    );
    for parts in [
        &["vlan", "Vlan100", "CONFIG_DB", "set"][..],
        &["vlan", "Vlan100", "APPL_DB", "queued"],
        &["vlan", "Vlan100", "STATE_DB", "set"],
        &["vlan", "Vlan100", "swss.rec", "orchagent received SET"],
        &["vlan", "Vlan100", "sairedis.rec", "SAI create"],
        &[
            "vlan",
            "Vlan100",
            "ASIC_DB",
            "✅ reached the ASIC",
            "after the CONFIG_DB change",
        ],
        &["vlan", "Vlan100", "CONFIG_DB", "member Ethernet4"],
        &[
            "vlan",
            "Vlan100",
            "ASIC_DB",
            "member Ethernet4",
            "✅ reached the ASIC",
        ],
        &["route", "10.9.0.0/24", "APPL_DB", "set"],
        &[
            "route",
            "10.9.0.0/24",
            "syslog",
            "Failed to create route 10.9.0.0/24",
        ],
        &[
            "route",
            "10.9.0.0/24",
            "⚠ no ASIC update",
            "after the APPL_DB change",
            "sonic-trace --live route 10.9.0.0/24",
        ],
    ] {
        assert!(
            line_with(&out, parts).is_some(),
            "missing {parts:?} in\n{out}"
        );
    }
    // Noise and unrelated objects are not shown; old log content is not replayed.
    assert!(
        !out.contains("NEIGH_TABLE") && !out.contains("10.8.0.5"),
        "{out}"
    );
    assert!(!out.contains("COUNTERS"), "{out}");
    assert!(!out.contains("10.99.0.0"), "{out}");
    assert!(!out.contains("existing line"), "{out}");
    assert!(!out.contains("KEY_SET"), "{out}");
    // Vlan100 reached the ASIC, so it is never reported as stuck.
    assert!(line_with(&out, &["Vlan100", "⚠"]).is_none(), "{out}");
}

#[test]
fn filter_shows_only_the_chosen_kind() {
    let d = tmp("filter");
    let (code, out, _) = run_live(&d, &["route"]);
    std::fs::remove_dir_all(&d).ok();
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("following all routes"), "{out}");
    assert!(
        line_with(&out, &["route", "10.9.0.0/24", "APPL_DB"]).is_some(),
        "{out}"
    );
    assert!(!out.contains("Vlan100"), "{out}");
}

#[test]
fn filter_by_object_name() {
    let d = tmp("name");
    let (_, out, _) = run_live(&d, &["Vlan100"]);
    std::fs::remove_dir_all(&d).ok();
    assert!(line_with(&out, &["Vlan100", "✅"]).is_some(), "{out}");
    assert!(!out.contains("10.9.0.0"), "{out}");
}

#[test]
fn unreachable_redis_is_a_clear_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_sonic-trace"))
        .args(["live", "--redis-cli", "false"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot reach Redis"), "{err}");
}

#[test]
fn bad_target_is_rejected() {
    let out = Command::new(env!("CARGO_BIN_EXE_sonic-trace"))
        .args(["live", "vlans", "bogus"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("'bogus' is not a kind"));
}

#[test]
fn live_rejects_snapshot() {
    let out = Command::new(env!("CARGO_BIN_EXE_sonic-trace"))
        .args(["--snapshot", "x", "live"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
