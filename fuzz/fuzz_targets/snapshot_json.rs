#![no_main]
use libfuzzer_sys::fuzz_target;
use trace_sonic::{port, route, snapshot::Snapshot, vlan};

// Any bytes as a snapshot: parsing and every trace must return, never panic.
fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    if let Ok(snap) = Snapshot::from_json(text) {
        let _ = vlan::trace(&snap, "Vlan3");
        let _ = port::trace(&snap, "Ethernet160");
        let _ = route::trace(&snap, "10.0.0.16/16");
        let _ = route::trace(&snap, "fe80::/64");
    }
});
