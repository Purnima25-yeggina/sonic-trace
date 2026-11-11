//! Feed truncated and corrupted copies of the fixtures to the parser and every
//! trace. Nothing may panic; errors are fine.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::string_slice
)]

use trace_sonic::{port, route, snapshot::Snapshot, vlan};

const FILES: [&str; 3] = [
    "fixtures/community/vlan/asic_db.json",
    "fixtures/community/port/asic_db.json",
    "fixtures/community/route/asic_db.json",
];

fn exercise(text: &str) {
    if let Ok(snap) = Snapshot::from_json(text) {
        let _ = vlan::trace(&snap, "Vlan3");
        let _ = port::trace(&snap, "Ethernet160");
        let _ = route::trace(&snap, "10.0.0.16/16");
    }
}

fn fixture(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(p).unwrap()
}

#[test]
fn every_truncation_is_handled() {
    for f in FILES {
        let text = fixture(f);
        for end in 0..text.len() {
            if text.is_char_boundary(end) {
                // Wrap as a multi-database file so truncation exercises the trace code too.
                exercise(&format!("{{\"ASIC_DB\":{}", &text[..end]));
                exercise(&text[..end]);
            }
        }
    }
}

#[test]
fn random_byte_corruption_is_handled() {
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for f in FILES {
        let base = fixture(f).into_bytes();
        for _ in 0..2000 {
            let mut b = base.clone();
            for _ in 0..(1 + next() % 5) {
                let i = (next() as usize) % b.len();
                b[i] = (next() % 256) as u8;
            }
            if let Ok(text) = String::from_utf8(b) {
                exercise(&format!("{{\"ASIC_DB\":{text}}}"));
            }
        }
    }
}

#[test]
fn odd_shapes_are_handled() {
    for t in [
        "",
        "{}",
        "[]",
        "null",
        "\"x\"",
        "{\"ASIC_DB\":null}",
        "{\"ASIC_DB\":[]}",
        "{\"ASIC_DB\":{\"k\":null}}",
        "{\"ASIC_DB\":{\"k\":{\"type\":1,\"value\":2}}}",
        "{\"APPL_DB\":{\"ROUTE_TABLE:1.1.1.0/24\":{\"nexthop_group\":\"loop\"}},\"CLASS_BASED_NEXT_HOP_GROUP_TABLE:loop\":1}",
    ] {
        exercise(t);
    }
    let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    exercise(&deep);
}
