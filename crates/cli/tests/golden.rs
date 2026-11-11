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

const LOGS: &str = "fixtures/logs";

#[test]
fn vlan_with_log_evidence() {
    check(
        "vlan_with_logs",
        &[
            "--snapshot",
            VLAN,
            "--swss-rec",
            "fixtures/logs/swss.rec",
            "--sairedis-rec",
            "fixtures/logs/sairedis.rec",
            "--syslog",
            "fixtures/logs/syslog",
            "vlan",
            "Vlan8",
        ],
        1,
    );
    assert!(std::path::Path::new(LOGS).exists() || root().join(LOGS).exists());
}

#[test]
fn multi_asic_selects_the_folder() {
    check(
        "multi_asic_0",
        &[
            "--snapshot",
            "fixtures/multi-asic",
            "--asic",
            "0",
            "vlan",
            "Vlan3",
        ],
        0,
    );
    check(
        "multi_asic_1",
        &[
            "--snapshot",
            "fixtures/multi-asic",
            "--asic",
            "1",
            "vlan",
            "Vlan3",
        ],
        1,
    );
    check(
        "multi_asic_missing",
        &[
            "--snapshot",
            "fixtures/multi-asic",
            "--asic",
            "7",
            "vlan",
            "Vlan3",
        ],
        2,
    );
}

#[cfg(unix)]
#[test]
fn live_mode_runs_the_dump_command() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join("sonic-trace-live-test");
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("fake-db-dump");
    // Behaves like `sonic-db-dump -n <DB>`: prints that database's JSON.
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ \"$1\" = \"-n\" ] || exit 3\ncat \"{}/fixtures/community/vlan/$(echo \"$2\" | tr A-Z a-z).json\"\n",
            root().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_sonic-trace"))
            .args(["--live", "--db-dump-cmd", script.to_str().unwrap()])
            .args(extra)
            .output()
            .unwrap()
    };
    let ok = run(&["vlan", "Vlan3"]);
    assert_eq!(ok.status.code(), Some(0));
    let stuck = run(&["vlan", "Vlan8"]);
    assert_eq!(stuck.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&stuck.stderr).contains("not atomic"));
    // A failing dump command is an input error.
    let bad = Command::new(env!("CARGO_BIN_EXE_sonic-trace"))
        .args(["--live", "--db-dump-cmd", "false", "vlan", "Vlan3"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
    // --live and --snapshot together are rejected by the argument parser.
    let both = Command::new(env!("CARGO_BIN_EXE_sonic-trace"))
        .args(["--live", "--snapshot", "x", "vlan", "Vlan3"])
        .output()
        .unwrap();
    assert_eq!(both.status.code(), Some(2));
}

const LAG: &str = "fixtures/community/portchannel";

#[test]
fn lag_member_missing() {
    check(
        "lag_member_missing",
        &["--snapshot", LAG, "lag", "PortChannel001"],
        1,
    );
}

#[test]
fn lag_empty() {
    check(
        "lag_empty",
        &["--snapshot", LAG, "lag", "PortChannel002"],
        0,
    );
}
