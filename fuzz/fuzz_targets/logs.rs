#![no_main]
use libfuzzer_sys::fuzz_target;
use trace_sonic::logs::Logs;

// Arbitrary log bytes: evidence extraction must never panic.
fuzz_target!(|data: &[u8]| {
    let logs = Logs::from_bytes(Some(data), Some(data), Some(data));
    let _ = logs.evidence(&["VLAN_TABLE:Vlan8"], Some(&|r| r.attrs.contains(&"x")), &["Vlan8", "10.0.0.0/8"]);
});
