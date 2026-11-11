#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use trace_sonic::{snapshot::Snapshot, vlan};

fn load(name: &str) -> Snapshot {
    let p = format!(
        "{}/../../fixtures/vlan/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    Snapshot::from_json(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn programmed_everywhere() {
    let r = vlan::trace(&load("ok"), "Vlan100").unwrap();
    assert_eq!(r.diagnosis.stopped_at, None);
}

#[test]
fn stops_at_asic() {
    let r = vlan::trace(&load("missing_asic"), "Vlan100").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("ASIC_DB"));
}

#[test]
fn stops_at_appl() {
    let r = vlan::trace(&load("missing_appl"), "Vlan100").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("APPL_DB"));
}

#[test]
fn not_configured() {
    let r = vlan::trace(&load("ok"), "Vlan200").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("CONFIG_DB"));
}

#[test]
fn rejects_bad_names_and_bad_json() {
    assert!(vlan::trace(&load("ok"), "Vlan0").is_err());
    assert!(vlan::trace(&load("ok"), "Vlan5000").is_err());
    assert!(vlan::trace(&load("ok"), "Ethernet0").is_err());
    assert!(Snapshot::from_json("not json").is_err());
    assert!(Snapshot::from_json("[1,2]").is_err());
}

#[test]
fn accepts_sonic_db_dump_wrapped_form() {
    let s = Snapshot::from_json(
        r#"{"CONFIG_DB":{"VLAN|Vlan7":{"type":"hash","value":{"vlanid":"7"}}}}"#,
    )
    .unwrap();
    assert!(s.get("CONFIG_DB", "VLAN|Vlan7").is_some());
}

#[test]
fn loads_per_database_bundle_dir() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vlan/bundle");
    let snap = Snapshot::from_dir(&dir).unwrap();
    let r = vlan::trace(&snap, "Vlan100").unwrap();
    assert_eq!(r.diagnosis.stopped_at, None);
    assert_eq!(r.stages.len(), 4);
}

#[test]
fn empty_dir_is_an_error() {
    let dir = std::env::temp_dir().join("sonic-trace-empty-test");
    std::fs::create_dir_all(&dir).unwrap();
    assert!(Snapshot::from_dir(&dir).is_err());
}

fn community() -> Snapshot {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/community/vlan");
    Snapshot::from_dir(&dir).unwrap()
}

#[test]
fn community_healthy_vlans_have_consistent_members() {
    for v in ["Vlan2", "Vlan3", "Vlan4"] {
        let r = vlan::trace(&community(), v).unwrap();
        assert_eq!(r.diagnosis.stopped_at, None, "{v}");
    }
    let r = vlan::trace(&community(), "Vlan3").unwrap();
    assert_eq!(
        r.diagnosis.evidence[0],
        "members: CONFIG_DB 2, APPL_DB 2, STATE_DB 2, ASIC_DB 2"
    );
}

#[test]
fn community_vlan6_databases_disagree() {
    let r = vlan::trace(&community(), "Vlan6").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("APPL_DB"));
    assert!(r.diagnosis.message.contains("disagree"));
    assert!(
        r.diagnosis
            .evidence
            .iter()
            .any(|e| e.contains("Ethernet32"))
    );
}

#[test]
fn community_vlan8_missing_in_asic() {
    let r = vlan::trace(&community(), "Vlan8").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("ASIC_DB"));
}
