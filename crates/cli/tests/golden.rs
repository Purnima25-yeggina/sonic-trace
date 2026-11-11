//! Golden tests: run the real binary and compare stdout and exit status with
//! files in tests/golden/. Regenerate with `UPDATE_GOLDEN=1 cargo test -p sonic-trace`,
//! then review the diff before committing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn check(name: &str, args: &[&str], want_code: i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_sonic-trace"))
        .args(args)
        .current_dir(root())
        .output()
        .unwrap();
    let code = out.status.code().unwrap();
    let text = format!(
        "exit: {code}\n--- stdout\n{}--- stderr\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let path = root().join("tests/golden").join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    let want =
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing golden file {name}.txt"));
    assert_eq!(text, want, "output changed for {name}");
    assert_eq!(code, want_code, "exit code for {name}");
}

const VLAN: &str = "fixtures/community/vlan";
const PORT: &str = "fixtures/community/port";
const ROUTE: &str = "fixtures/community/route";

#[test]
fn vlan_healthy() {
    check("vlan_healthy", &["--snapshot", VLAN, "vlan", "Vlan3"], 0);
}
#[test]
fn vlan_missing_in_asic() {
    check(
        "vlan_missing_in_asic",
        &["--snapshot", VLAN, "vlan", "Vlan8"],
        1,
    );
}
#[test]
fn vlan_databases_disagree() {
    check(
        "vlan_databases_disagree",
        &["--snapshot", VLAN, "vlan", "Vlan6"],
        1,
    );
}
#[test]
fn vlan_json() {
    check(
        "vlan_json",
        &["--snapshot", VLAN, "--json", "vlan", "Vlan8"],
        1,
    );
}
#[test]
fn port_sai_object_missing() {
    check(
        "port_sai_object_missing",
        &["--snapshot", PORT, "port", "Ethernet160"],
        1,
    );
}
#[test]
fn route_not_in_asic() {
    check(
        "route_not_in_asic",
        &["--snapshot", ROUTE, "route", "10.0.0.16/16"],
        1,
    );
}
#[test]
fn route_ecmp_healthy() {
    check(
        "route_ecmp_healthy",
        &["--snapshot", ROUTE, "route", "192.168.0.4/24"],
        0,
    );
}
#[test]
fn bad_vlan_name_exits_2() {
    check(
        "bad_vlan_name",
        &["--snapshot", VLAN, "vlan", "Ethernet0"],
        2,
    );
}
#[test]
fn missing_snapshot_exits_2() {
    check(
        "missing_snapshot",
        &["--snapshot", "does/not/exist", "vlan", "Vlan1"],
        2,
    );
}

#[test]
fn input_over_limit_exits_2() {
    check(
        "input_over_limit",
        &["--snapshot", VLAN, "--max-input-mb", "0", "vlan", "Vlan3"],
        2,
    );
}
