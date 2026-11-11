#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::string_slice
)]

use std::path::{Path, PathBuf};
use trace_sonic::{logs::Logs, port, route, snapshot::Snapshot, vlan};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn snap(kind: &str) -> Snapshot {
    Snapshot::from_dir(&root().join("fixtures/community").join(kind)).unwrap()
}

fn logs() -> Logs {
    let d = root().join("fixtures/logs");
    Logs::load(
        Some(&d.join("swss.rec")),
        Some(&d.join("sairedis.rec")),
        Some(&d.join("syslog")),
        &mut 1_000_000,
    )
    .unwrap()
}

#[test]
fn vlan_that_never_reached_the_asic_shows_all_three_logs() {
    let r = vlan::trace_with(&snap("vlan"), &logs(), "Vlan8").unwrap();
    let ev = r.diagnosis.evidence.join("\n");
    assert!(
        ev.contains("swss.rec: orchagent last received SET for VLAN_TABLE:Vlan8"),
        "{ev}"
    );
    assert!(ev.contains("sairedis.rec: no SAI call recorded"), "{ev}");
    assert!(
        ev.contains("syslog:") && ev.contains("mentioning Vlan8"),
        "{ev}"
    );
    // Whole-word match: the Vlan80 line must not be attributed to Vlan8.
    assert!(!ev.contains("Vlan80"), "{ev}");
}

#[test]
fn route_never_received_by_orchagent() {
    let r = route::trace_with(&snap("route"), &logs(), "10.0.0.16/16").unwrap();
    let ev = r.diagnosis.evidence.join("\n");
    assert!(
        ev.contains("no record of orchagent receiving ROUTE_TABLE:10.0.0.16/16"),
        "{ev}"
    );
    assert!(ev.contains("warning for route 10.0.0.16/16"), "{ev}");
}

#[test]
fn port_sai_call_is_found_but_has_no_result() {
    let r = port::trace_with(&snap("port"), &logs(), "Ethernet160").unwrap();
    let ev = r.diagnosis.evidence.join("\n");
    assert!(ev.contains("last SAI create"), "{ev}");
    assert!(ev.contains("the log records no result"), "{ev}");
}

#[test]
fn healthy_object_gets_no_log_evidence() {
    let r = vlan::trace_with(&snap("vlan"), &logs(), "Vlan3").unwrap();
    assert_eq!(r.diagnosis.stopped_at, None);
    assert!(
        r.diagnosis
            .evidence
            .iter()
            .all(|e| !e.starts_with("swss.rec"))
    );
}

#[test]
fn no_logs_means_no_log_evidence() {
    let r = vlan::trace_with(&snap("vlan"), &Logs::default(), "Vlan8").unwrap();
    assert!(
        r.diagnosis
            .evidence
            .iter()
            .all(|e| !e.contains(".rec") && !e.starts_with("syslog"))
    );
}

#[test]
fn garbage_log_content_is_ignored() {
    let dir = std::env::temp_dir().join("sonic-trace-garbage-logs");
    std::fs::create_dir_all(&dir).unwrap();
    let junk = dir.join("junk");
    std::fs::write(&junk, b"\xff\xfe|||\n|\n\x00\x01|c|\n||||\nplain text\n").unwrap();
    let l = Logs::load(Some(&junk), Some(&junk), Some(&junk), &mut 1_000_000).unwrap();
    let r = vlan::trace_with(&snap("vlan"), &l, "Vlan8").unwrap();
    assert!(r.diagnosis.evidence.iter().any(|e| e.contains("no record")));
}

#[test]
fn log_size_limit_is_enforced() {
    let d = root().join("fixtures/logs");
    let e = Logs::load(Some(&d.join("swss.rec")), None, None, &mut 10).unwrap_err();
    assert!(e.contains("size limit"));
}
