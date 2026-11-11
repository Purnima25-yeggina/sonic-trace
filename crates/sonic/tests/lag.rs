#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::string_slice
)]

use trace_sonic::{lag, snapshot::Snapshot};

fn community() -> Snapshot {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/community/portchannel");
    Snapshot::from_dir(&dir).unwrap()
}

fn stop(snap: &Snapshot, name: &str) -> Option<String> {
    lag::trace(snap, name).unwrap().diagnosis.stopped_at
}

#[test]
fn member_missing_from_appl_db() {
    let r = lag::trace(&community(), "PortChannel001").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("APPL_DB"));
    assert!(r.diagnosis.evidence.iter().any(|e| e.contains("Ethernet8")));
    assert_eq!(
        r.diagnosis.evidence[0],
        "members: CONFIG_DB 3, APPL_DB 2, STATE_DB 2, ASIC_DB 2"
    );
}

#[test]
fn empty_lag_cannot_be_checked_in_asic() {
    assert_eq!(stop(&community(), "PortChannel002"), None);
    let r = lag::trace(&community(), "PortChannel002").unwrap();
    assert_eq!(format!("{:?}", r.stages[3].presence), "NotApplicable");
}

#[test]
fn configured_but_not_published() {
    assert_eq!(
        stop(&community(), "PortChannel003").as_deref(),
        Some("APPL_DB")
    );
}

#[test]
fn not_configured_and_bad_names() {
    assert_eq!(
        stop(&community(), "PortChannel099").as_deref(),
        Some("CONFIG_DB")
    );
    for bad in ["PortChannel", "PortChannelx", "Ethernet0", "Vlan1", ""] {
        assert!(lag::trace(&community(), bad).is_err(), "{bad}");
    }
}

fn synthetic(asic_members: &[(&str, &str)]) -> Snapshot {
    // Two ports, each with a host interface; PortChannel9 expects Ethernet0 and Ethernet4.
    let mut asic = String::from(
        r#""ASIC_STATE:SAI_OBJECT_TYPE_HOSTIF:oid:0xd1":{"SAI_HOSTIF_ATTR_NAME":"Ethernet0","SAI_HOSTIF_ATTR_OBJ_ID":"oid:0x10"},
           "ASIC_STATE:SAI_OBJECT_TYPE_HOSTIF:oid:0xd2":{"SAI_HOSTIF_ATTR_NAME":"Ethernet4","SAI_HOSTIF_ATTR_OBJ_ID":"oid:0x11"},
           "ASIC_STATE:SAI_OBJECT_TYPE_LAG:oid:0x20":{"NULL":"NULL"}"#,
    );
    for (i, (lag, port)) in asic_members.iter().enumerate() {
        asic.push_str(&format!(
            r#","ASIC_STATE:SAI_OBJECT_TYPE_LAG_MEMBER:oid:0x1b{i}":{{"SAI_LAG_MEMBER_ATTR_LAG_ID":"{lag}","SAI_LAG_MEMBER_ATTR_PORT_ID":"{port}"}}"#
        ));
    }
    Snapshot::from_json(&format!(
        r#"{{"CONFIG_DB":{{"PORTCHANNEL|PortChannel9":{{}},"PORTCHANNEL_MEMBER|PortChannel9|Ethernet0":{{}},"PORTCHANNEL_MEMBER|PortChannel9|Ethernet4":{{}}}},
            "APPL_DB":{{"LAG_TABLE:PortChannel9":{{}},"LAG_MEMBER_TABLE:PortChannel9:Ethernet0":{{}},"LAG_MEMBER_TABLE:PortChannel9:Ethernet4":{{}}}},
            "STATE_DB":{{"LAG_TABLE|PortChannel9":{{}}}},
            "ASIC_DB":{{{asic}}}}}"#
    ))
    .unwrap()
}

#[test]
fn asic_lag_with_all_members_is_healthy() {
    let s = synthetic(&[("oid:0x20", "oid:0x10"), ("oid:0x20", "oid:0x11")]);
    assert_eq!(stop(&s, "PortChannel9"), None);
}

#[test]
fn asic_lag_missing_a_member_is_reported() {
    let s = synthetic(&[("oid:0x20", "oid:0x10")]);
    let r = lag::trace(&s, "PortChannel9").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("ASIC_LAG"));
    assert!(
        r.diagnosis
            .evidence
            .iter()
            .any(|e| e.contains("ASIC_DB LAG has {Ethernet0}"))
    );
}

#[test]
fn no_asic_lag_contains_the_members() {
    let s = synthetic(&[]);
    let r = lag::trace(&s, "PortChannel9").unwrap();
    assert_eq!(r.diagnosis.stopped_at.as_deref(), Some("ASIC_LAG"));
    assert_eq!(format!("{:?}", r.stages[3].presence), "Missing");
}
