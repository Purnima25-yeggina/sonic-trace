#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use trace_sonic::{route, snapshot::Snapshot};

fn community() -> Snapshot {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/community/route");
    Snapshot::from_dir(&dir).unwrap()
}

fn stop(prefix: &str) -> Option<String> {
    route::trace(&community(), prefix)
        .unwrap()
        .diagnosis
        .stopped_at
}

#[test]
fn healthy_routes() {
    for p in [
        "10.212.0.0/16",
        "20.0.0.0/24",
        "1.1.1.0/24",
        "192.168.0.4/24",
    ] {
        assert_eq!(stop(p), None, "{p}");
    }
}

#[test]
fn ecmp_members_are_resolved() {
    let r = route::trace(&community(), "192.168.0.4/24").unwrap();
    assert!(r.diagnosis.evidence[0].contains("4 members"));
    assert!(r.diagnosis.evidence[0].contains("fc00::72"));
}

#[test]
fn static_route_shows_config_stage() {
    let r = route::trace(&community(), "20.0.0.0/24").unwrap();
    assert_eq!(format!("{:?}", r.stages[0].presence), "Present");
    let r = route::trace(&community(), "10.212.0.0/16").unwrap();
    assert_eq!(format!("{:?}", r.stages[0].presence), "NotApplicable");
}

#[test]
fn nexthop_group_not_in_asic() {
    assert_eq!(stop("10.0.0.16/16").as_deref(), Some("ASIC_DB"));
    let r = route::trace(&community(), "10.0.0.16/16").unwrap();
    assert!(
        r.diagnosis
            .evidence
            .iter()
            .any(|e| e.contains("nexthop group testnhg"))
    );
    assert!(
        r.diagnosis
            .evidence
            .iter()
            .any(|e| e.contains("10.0.0.1") && e.contains("unresolved"))
    );
    assert_eq!(stop("10.1.1.16/16").as_deref(), Some("ASIC_DB"));
}

#[test]
fn asic_only_local_route_is_not_an_error() {
    assert_eq!(stop("1.1.1.1/32"), None);
}

#[test]
fn unknown_route_and_bad_input() {
    assert_eq!(stop("99.9.9.0/24").as_deref(), Some("APPL_DB"));
    assert!(route::trace(&community(), "").is_err());
    assert!(route::trace(&community(), "Vlan10").is_err());
}

#[test]
fn next_hop_mismatch_is_reported() {
    let snap = Snapshot::from_json(
        r#"{"APPL_DB":{"ROUTE_TABLE:10.9.0.0/24":{"nexthop":"1.2.3.4","ifname":"Ethernet0"}},
            "ASIC_DB":{
             "ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:{\"dest\":\"10.9.0.0/24\",\"switch_id\":\"oid:0x21\",\"vr\":\"oid:0x30\"}":{"SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID":"oid:0x40"},
             "ASIC_STATE:SAI_OBJECT_TYPE_NEXT_HOP:oid:0x40":{"SAI_NEXT_HOP_ATTR_IP":"5.6.7.8"}}}"#,
    )
    .unwrap();
    let r = route::trace(&snap, "10.9.0.0/24").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("ASIC_NEXTHOP"));
    assert!(
        r.diagnosis
            .evidence
            .iter()
            .any(|e| e.contains("1.2.3.4") && e.contains("5.6.7.8"))
    );
}

#[test]
fn dangling_next_hop_oid() {
    let snap = Snapshot::from_json(
        r#"{"APPL_DB":{"ROUTE_TABLE:10.9.0.0/24":{"nexthop":"1.2.3.4"}},
            "ASIC_DB":{"ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:{\"dest\":\"10.9.0.0/24\",\"vr\":\"oid:0x30\"}":{"SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID":"oid:0x99"}}}"#,
    )
    .unwrap();
    let r = route::trace(&snap, "10.9.0.0/24").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("ASIC_NEXTHOP"));
}
