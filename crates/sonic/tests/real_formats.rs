//! Regression tests for formats taken from the SONiC source: generate_dump's bundle layout,
//! fpmsyncd's route keys, sairedis bulk records, gzip logs, and VRF routes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::string_slice
)]

use std::path::{Path, PathBuf};
use trace_sonic::{
    logs::Logs,
    route,
    snapshot::{BundleLayout, Snapshot},
    vlan,
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bundle() -> PathBuf {
    root().join("fixtures/bundle")
}

fn snap() -> Snapshot {
    Snapshot::from_path(&bundle(), 1 << 30).unwrap()
}

fn bundle_logs() -> Logs {
    let l = BundleLayout::locate(&bundle(), None).unwrap();
    Logs::load(
        l.log_file("swss.rec").as_deref(),
        l.log_file("sairedis.rec").as_deref(),
        l.log_file("syslog").as_deref(),
        &mut (1 << 30),
    )
    .unwrap()
}

fn stop(target: &str) -> Option<String> {
    route::trace(&snap(), target).unwrap().diagnosis.stopped_at
}

// --- Bug 1: host routes are written to APPL_DB without the mask ---

#[test]
fn host_route_is_healthy_with_or_without_mask() {
    for t in ["10.1.0.32", "10.1.0.32/32"] {
        let r = route::trace(&snap(), t).unwrap();
        assert_eq!(r.diagnosis.stopped_at, None, "{t}");
        assert_eq!(r.stages[1].detail, "ROUTE_TABLE:10.1.0.32", "{t}");
        assert_eq!(r.object, "10.1.0.32/32");
    }
}

#[test]
fn asic_route_through_a_next_hop_without_appl_entry_is_flagged() {
    // Previously reported as "normal for local routes".
    let s = Snapshot::from_json(
        r#"{"APPL_DB":{},
            "ASIC_DB":{
             "ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:{\"dest\":\"50.0.0.0/24\",\"vr\":\"oid:0x30\"}":{"SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID":"oid:0x40"},
             "ASIC_STATE:SAI_OBJECT_TYPE_NEXT_HOP:oid:0x40":{"SAI_NEXT_HOP_ATTR_IP":"10.0.0.1"},
             "ASIC_STATE:SAI_OBJECT_TYPE_VIRTUAL_ROUTER:oid:0x30":{}}}"#,
    )
    .unwrap();
    let r = route::trace(&s, "50.0.0.0/24").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("APPL_DB"));
    assert!(r.diagnosis.message.contains("stale"));
}

// --- Bug 2: bulk records in sairedis.rec ---

#[test]
fn bulk_route_create_is_recognized() {
    let r = route::trace_with(&snap(), &bundle_logs(), "40.0.0.0/24").unwrap();
    let ev = r.diagnosis.evidence.join("\n");
    assert!(ev.contains("last SAI create (bulk)"), "{ev}");
    assert!(!ev.contains("no SAI call recorded"), "{ev}");
}

#[test]
fn bulk_line_with_several_entries_matches_only_the_right_one() {
    let line = "t|C|SAI_OBJECT_TYPE_ROUTE_ENTRY||{\"dest\":\"1.1.1.0/24\",\"vr\":\"oid:0x30\"}|A=1||{\"dest\":\"2.2.2.0/24\",\"vr\":\"oid:0x30\"}|A=2\n";
    let logs = Logs::from_bytes(None, Some(line.as_bytes()), None);
    let ev = logs.evidence(
        &["ROUTE_TABLE:2.2.2.0/24"],
        Some(&|r| r.key.contains("\"dest\":\"2.2.2.0/24\"") && r.attrs.contains(&"A=2")),
        &[],
    );
    assert_eq!(
        ev,
        vec!["sairedis.rec: last SAI create (bulk) at t (1 call; the log records no result)"]
    );
}

// --- Bug 3: logs are gzip-compressed in a bundle ---

#[test]
fn gzipped_logs_are_found_and_read() {
    let l = BundleLayout::locate(&bundle(), None).unwrap();
    assert!(l.log_file("swss.rec").unwrap().ends_with("log/swss.rec.gz"));
    let r = vlan::trace_with(&snap(), &bundle_logs(), "Vlan8").unwrap();
    let ev = r.diagnosis.evidence.join("\n");
    assert!(
        ev.contains("orchagent last received SET for VLAN_TABLE:Vlan8"),
        "{ev}"
    );
    assert!(ev.contains("mentioning Vlan8"), "{ev}");
}

#[test]
fn gzip_that_expands_past_the_limit_is_refused() {
    use std::io::Write;
    let dir = std::env::temp_dir().join("sonic-trace-gzip-limit");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("swss.rec.gz");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&vec![b'a'; 4 << 20]).unwrap();
    std::fs::write(&p, gz.finish().unwrap()).unwrap();
    let e = Logs::load(Some(&p), None, None, &mut (1 << 20)).unwrap_err();
    assert!(e.contains("size limit"), "{e}");
}

// --- Bug 4: generate_dump bundle layout and multi-ASIC naming ---

#[test]
fn bundle_root_dump_folder_and_plain_folder_all_load() {
    assert!(Snapshot::from_path(&bundle(), 1 << 30).is_ok());
    assert!(Snapshot::from_path(&bundle().join("dump"), 1 << 30).is_ok());
    assert!(Snapshot::from_path(&root().join("fixtures/community/vlan"), 1 << 30).is_ok());
    // From the dump/ folder, the logs in the sibling log/ folder are still found.
    let l = BundleLayout::locate(&bundle().join("dump"), None).unwrap();
    assert!(l.log_file("sairedis.rec").is_some());
}

#[test]
fn multi_asic_files_use_the_json_n_suffix() {
    let mut budget = 1 << 30;
    let s = Snapshot::from_path_budget(&bundle(), Some(1), &mut budget).unwrap();
    // ASIC 1 holds the community route data, not the default namespace's VLANs.
    assert!(s.get("APPL_DB", "ROUTE_TABLE:10.212.0.0/16").is_some());
    assert!(s.get("CONFIG_DB", "VLAN|Vlan3").is_none());
    let e = Snapshot::from_path_budget(&bundle(), Some(5), &mut budget).unwrap_err();
    assert!(e.contains("<DB>.json.5"), "{e}");
}

// --- Bug 5: VRF routes ---

#[test]
fn vrf_route_is_traced_in_its_own_virtual_router() {
    let r = route::trace(&snap(), "Vrf-red:20.0.0.0/24").unwrap();
    assert_eq!(r.diagnosis.stopped_at, None);
    assert!(r.stages[2].detail.contains("oid:0x3000000000099"));
}

#[test]
fn default_vrf_query_does_not_match_another_vrfs_route() {
    // Previously this matched Vrf-red's ASIC route and said "normal".
    assert_eq!(stop("20.0.0.0/24").as_deref(), Some("APPL_DB"));
    let r = route::trace(&snap(), "20.0.0.0/24").unwrap();
    assert!(r.diagnosis.message.contains("not found"));
}

#[test]
fn vrf_without_known_interfaces_falls_back_with_a_note() {
    let s = Snapshot::from_json(
        r#"{"APPL_DB":{"ROUTE_TABLE:Vrf-blue:9.0.0.0/8":{"nexthop":"1.1.1.1"}},
            "ASIC_DB":{
             "HIDDEN":{"SAI_SWITCH_ATTR_DEFAULT_VIRTUAL_ROUTER_ID":"oid:0x1"},
             "RIDTOVID":{"oid:0x1":"oid:0x30"},
             "ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:{\"dest\":\"9.0.0.0/8\",\"vr\":\"oid:0x31\"}":{"SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID":"oid:0x40"},
             "ASIC_STATE:SAI_OBJECT_TYPE_NEXT_HOP:oid:0x40":{"SAI_NEXT_HOP_ATTR_IP":"1.1.1.1"}}}"#,
    )
    .unwrap();
    let r = route::trace(&s, "Vrf-blue:9.0.0.0/8").unwrap();
    assert_eq!(r.diagnosis.stopped_at, None);
    assert!(
        r.diagnosis
            .evidence
            .iter()
            .any(|e| e.contains("could not map Vrf-blue"))
    );
}

#[test]
fn route_targets_are_validated_and_canonicalized() {
    for bad in [
        "",
        "10.0.0.0/33",
        "fe80::/129",
        "abc",
        "Vrf:1.1.1.0/24",
        "Vrf-x",
        "10.0.0.0/x",
        "1.2.3",
    ] {
        assert!(route::trace(&snap(), bad).is_err(), "{bad}");
    }
    let r = route::trace(&snap(), "FE80:0:0::1/64").unwrap();
    assert_eq!(r.object, "fe80::1/64");
}

// --- Parsing ambiguity: plain hashes with fields named "type" and "value" ---

#[test]
fn plain_hash_with_type_and_value_fields_is_not_unwrapped() {
    let s = Snapshot::from_json(
        r#"{"CONFIG_DB":{"X|a":{"type":"L3","value":"7"},
                          "X|b":{"type":"hash","value":{"f":"v"}},
                          "X|c":{"type":"hash","value":"oops"}}}"#,
    )
    .unwrap();
    assert_eq!(
        s.get("CONFIG_DB", "X|a").unwrap().get("type").unwrap(),
        "L3"
    );
    assert_eq!(s.get("CONFIG_DB", "X|b").unwrap().get("f").unwrap(), "v");
    assert_eq!(
        s.get("CONFIG_DB", "X|c").unwrap().get("value").unwrap(),
        "oops"
    );
}

#[test]
fn default_query_points_to_the_vrf_that_has_the_prefix() {
    let r = route::trace(&snap(), "20.0.0.0/24").unwrap();
    assert!(
        r.diagnosis
            .evidence
            .iter()
            .any(|e| e.contains("APPL_DB has this prefix in Vrf-red"))
    );
}

#[test]
fn unknown_default_virtual_router_is_stated() {
    // No HIDDEN/RIDTOVID and no VIRTUAL_ROUTER objects: the match cannot be attributed.
    let s = Snapshot::from_json(
        r#"{"APPL_DB":{"ROUTE_TABLE:Vrf-red:20.0.0.0/24":{"nexthop":"10.0.0.9"}},
            "ASIC_DB":{
             "ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:{\"dest\":\"20.0.0.0/24\",\"vr\":\"oid:0x99\"}":{"SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID":"oid:0x40"},
             "ASIC_STATE:SAI_OBJECT_TYPE_NEXT_HOP:oid:0x40":{"SAI_NEXT_HOP_ATTR_IP":"10.0.0.9"}}}"#,
    )
    .unwrap();
    let ev = route::trace(&s, "20.0.0.0/24")
        .unwrap()
        .diagnosis
        .evidence
        .join("\n");
    assert!(
        ev.contains("default virtual router could not be identified"),
        "{ev}"
    );
    assert!(ev.contains("Vrf-red"), "{ev}");
}
