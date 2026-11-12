//! Regression tests for problems found by running the tool on a real support bundle:
//! link-local routes, rotated logs, and unreadable syslog evidence. All data here is
//! synthetic (made-up addresses, names and times), shaped like the real formats.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::string_slice
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use trace_sonic::{
    logs::{LogSource, Logs},
    route,
    snapshot::{BundleLayout, Snapshot},
};

// --- Link-local routes ---

/// A default VRF (single virtual router) with a link-local APPL_DB route, optionally
/// with orchagent's fe80::/10 CPU route.
fn link_local_snap(with_cpu_route: bool) -> Snapshot {
    let cpu = if with_cpu_route {
        r#","ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:{\"dest\":\"fe80::/10\",\"switch_id\":\"oid:0x21\",\"vr\":\"oid:0x30\"}":{"SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID":"oid:0x1"}"#
    } else {
        ""
    };
    Snapshot::from_json(&format!(
        r#"{{"APPL_DB":{{"ROUTE_TABLE:fe80::/64":{{"nexthop":"::","ifname":"Vlan200"}}}},
            "ASIC_DB":{{"ASIC_STATE:SAI_OBJECT_TYPE_VIRTUAL_ROUTER:oid:0x30":{{"NULL":"NULL"}}{cpu}}}}}"#
    ))
    .unwrap()
}

#[test]
fn link_local_route_covered_by_cpu_route_is_normal() {
    let r = route::trace(&link_local_snap(true), "fe80::/64").unwrap();
    assert_eq!(r.diagnosis.stopped_at, None, "{:?}", r.diagnosis);
    assert!(r.diagnosis.message.contains("link-local"));
    assert!(r.stages[2].detail.contains("fe80::/10"));
}

#[test]
fn link_local_route_without_cpu_route_is_flagged() {
    let r = route::trace(&link_local_snap(false), "fe80::/64").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("ASIC_DB"));
    assert!(r.diagnosis.message.contains("fe80::/10"));
}

#[test]
fn non_link_local_ipv6_route_is_still_flagged() {
    // fec0:: is outside fe80::/10 and must not be excused.
    let snap = Snapshot::from_json(
        r#"{"APPL_DB":{"ROUTE_TABLE:fec0::/64":{"nexthop":"fc00::1"}},
            "ASIC_DB":{"ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:{\"dest\":\"fe80::/10\",\"vr\":\"oid:0x30\"}":{}}}"#,
    )
    .unwrap();
    let r = route::trace(&snap, "fec0::/64").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("ASIC_DB"));
}

// --- Rotated logs ---

fn gz(path: &Path, text: &str) {
    let f = std::fs::File::create(path).unwrap();
    let mut e = flate2::write::GzEncoder::new(f, flate2::Compression::default());
    e.write_all(text.as_bytes()).unwrap();
    e.finish().unwrap();
}

/// A bundle whose route was only received before the last two log rotations.
fn rotated_bundle(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sonic-trace-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("dump")).unwrap();
    std::fs::create_dir_all(d.join("log")).unwrap();
    std::fs::write(
        d.join("dump/APPL_DB.json"),
        r#"{"ROUTE_TABLE:10.99.0.0/24":{"type":"hash","value":{"nexthop":"10.98.0.1"}}}"#,
    )
    .unwrap();
    std::fs::write(d.join("dump/ASIC_DB.json"), "{}").unwrap();
    let log = d.join("log");
    gz(
        &log.join("swss.rec.2.gz"),
        "2000-01-01.00:00:01.000000|ROUTE_TABLE:10.99.0.0/24|SET|nexthop:10.98.0.1\n",
    );
    gz(
        &log.join("swss.rec.1.gz"),
        "2000-01-01.01:00:00.000000|PORT_TABLE:Ethernet0|SET|mtu:9100\n",
    );
    gz(
        &log.join("swss.rec.gz"),
        "2000-01-01.02:00:00.000000|PORT_TABLE:Ethernet0|SET|mtu:9100\n",
    );
    // Not rotations: must be ignored.
    std::fs::write(log.join("swss.rec.bak"), "junk").unwrap();
    std::fs::write(log.join("swss.recX.1"), "junk").unwrap();
    d
}

fn load(bundle: &Path, budget: u64) -> Logs {
    let l = BundleLayout::locate(bundle, None).unwrap();
    let rot = l.log_rotations("swss.rec");
    let cur = l.log_file("swss.rec");
    Logs::load_sources(
        LogSource {
            current: cur.as_deref(),
            rotations: &rot,
        },
        LogSource::default(),
        LogSource::default(),
        &mut { budget },
    )
    .unwrap()
}

#[test]
fn rotations_are_found_newest_first() {
    let d = rotated_bundle("rot-order");
    let l = BundleLayout::locate(&d, None).unwrap();
    let names: Vec<String> = l
        .log_rotations("swss.rec")
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["swss.rec.1.gz", "swss.rec.2.gz"]);
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn record_in_an_older_rotation_is_found() {
    // Previously: "no record of orchagent receiving", because only swss.rec.gz was read.
    let d = rotated_bundle("rot-found");
    let snap = Snapshot::from_path(&d, 1 << 20).unwrap();
    let r = route::trace_with(&snap, &load(&d, 1 << 20), "10.99.0.0/24").unwrap();
    let ev = r.diagnosis.evidence.join("\n");
    assert!(
        ev.contains("swss.rec: orchagent last received SET for ROUTE_TABLE:10.99.0.0/24 at 2000-01-01.00:00:01"),
        "{ev}"
    );
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn not_found_says_what_was_checked_and_what_was_skipped() {
    let d = rotated_bundle("rot-skip");
    let snap = Snapshot::from_path(&d, 1 << 20).unwrap();
    // Enough for the current file (about 60 bytes) but not for the rotations' share.
    let r = route::trace_with(&snap, &load(&d, 150), "10.99.0.0/24").unwrap();
    let ev = r.diagnosis.evidence.join("\n");
    assert!(
        ev.contains("no record of orchagent receiving ROUTE_TABLE:10.99.0.0/24 (checked 1 file from 2000-01-01.02:00:00.000000; 2 older rotations not read (size limit"),
        "{ev}"
    );
    std::fs::remove_dir_all(&d).ok();
}

// --- Syslog evidence ---

#[test]
fn syslog_json_is_flattened_shortened_and_deduplicated() {
    let list: String = (0..40)
        .map(|i| format!("#012  \"10.97.{i}.1/32\","))
        .collect();
    let line = |t: &str| {
        format!(
            "2000 Jan 01 {t} host WARNING route_check.py: Failure results: {{#012  \"missed_ROUTE_TABLE_routes\": [#012  \"10.99.0.0/24\",{list}#012  ]#012}}\n"
        )
    };
    let syslog = format!("{}{}", line("00:00:01.0"), line("00:01:01.0"));
    let logs = Logs::from_bytes(None, None, Some(syslog.as_bytes()));
    let snap = Snapshot::from_json(
        r#"{"APPL_DB":{"ROUTE_TABLE:10.99.0.0/24":{"nexthop":"10.98.0.1"}},"ASIC_DB":{}}"#,
    )
    .unwrap();
    let r = route::trace_with(&snap, &logs, "10.99.0.0/24").unwrap();
    let sys: Vec<&String> = r
        .diagnosis
        .evidence
        .iter()
        .filter(|e| e.starts_with("syslog:"))
        .collect();
    assert_eq!(sys.len(), 1, "{sys:?}");
    let e = sys[0];
    assert!(!e.contains("#012"), "{e}");
    assert!(e.contains("00:01:01.0"), "latest occurrence shown: {e}");
    assert!(e.contains("route_check.py: Failure results"), "{e}");
    assert!(e.contains("10.99.0.0/24"), "{e}");
    assert!(e.contains("(seen 2 times"), "{e}");
    assert!(
        e.chars().count() < 320,
        "shortened: {} chars",
        e.chars().count()
    );
}
