#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use trace_sonic::{port, snapshot::Snapshot};

fn community() -> Snapshot {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/community/port");
    Snapshot::from_dir(&dir).unwrap()
}

#[test]
fn healthy_port() {
    let r = port::trace(&community(), "Ethernet176").unwrap();
    assert_eq!(r.diagnosis.stopped_at, None);
    assert_eq!(r.stages.len(), 5);
}

#[test]
fn hostif_points_at_missing_sai_port() {
    let r = port::trace(&community(), "Ethernet160").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("ASIC_PORT"));
    assert!(r.diagnosis.message.contains("0x10000000002e6"));
}

#[test]
fn no_hostif_with_that_name() {
    let r = port::trace(&community(), "Ethernet164").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("ASIC_HOSTIF"));
}

#[test]
fn configured_but_not_in_appl_db() {
    let r = port::trace(&community(), "Ethernet156").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("APPL_DB"));
}

#[test]
fn not_configured_and_bad_names() {
    let r = port::trace(&community(), "Ethernet4").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("CONFIG_DB"));
    assert!(port::trace(&community(), "Vlan10").is_err());
    assert!(port::trace(&community(), "Ethernetx").is_err());
    assert!(port::trace(&community(), "Eth").is_err());
}

#[test]
fn speed_mismatch_is_reported() {
    let snap = Snapshot::from_json(
        r#"{"CONFIG_DB":{"PORT|Ethernet0":{"speed":"100000","admin_status":"up"}},
            "APPL_DB":{"PORT_TABLE:Ethernet0":{}},
            "STATE_DB":{"PORT_TABLE|Ethernet0":{"state":"ok"}},
            "ASIC_DB":{
              "ASIC_STATE:SAI_OBJECT_TYPE_HOSTIF:oid:0xd1":{"SAI_HOSTIF_ATTR_NAME":"Ethernet0","SAI_HOSTIF_ATTR_OBJ_ID":"oid:0x10"},
              "ASIC_STATE:SAI_OBJECT_TYPE_PORT:oid:0x10":{"SAI_PORT_ATTR_SPEED":"25000","SAI_PORT_ATTR_ADMIN_STATE":"false"}}}"#,
    )
    .unwrap();
    let r = port::trace(&snap, "Ethernet0").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("ASIC_PORT"));
    assert_eq!(r.diagnosis.evidence.len(), 2);
}
