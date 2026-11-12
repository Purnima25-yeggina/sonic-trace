//! `check` (trace everything, list passed and failed) and `.tar.gz` bundle input.
//! All data is the synthetic fixtures in fixtures/.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::string_slice
)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sonic-trace"))
        .current_dir(root())
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sonic-trace-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Pack fixtures/bundle as generate_dump does: everything under one top folder.
fn pack_bundle(dest: &Path, extra: &[(&str, &[u8])]) {
    let f = std::fs::File::create(dest).unwrap();
    let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
    let mut t = tar::Builder::new(gz);
    t.append_dir_all(
        "sonic_dump_test_20000101_000000",
        root().join("fixtures/bundle"),
    )
    .unwrap();
    for (path, data) in extra {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        t.append_data(&mut h, path, *data).unwrap();
    }
    t.into_inner().unwrap().finish().unwrap();
}

#[test]
fn check_lists_failed_then_passed_then_summary() {
    let (code, out, _) = run(&["--snapshot", "fixtures/bundle", "check"]);
    assert_eq!(code, 1, "{out}");
    let failed = out.find("FAILED (4)").unwrap();
    let passed = out.find("PASSED (6)").unwrap();
    let summary = out.find("SUMMARY").unwrap();
    assert!(failed < passed && passed < summary, "{out}");
    for line in [
        "FAIL vlan  Vlan8",
        "FAIL route 30.0.0.0/24",
        "FAIL route 40.0.0.0/24          stopped at ASIC_NEXTHOP",
        "PASS route 10.1.0.32/32",
        "PASS route Vrf-red:20.0.0.0/24",
    ] {
        assert!(out.contains(line), "missing {line:?} in\n{out}");
    }
    assert!(
        out.contains("  all        10       6       4      0"),
        "{out}"
    );
}

#[test]
fn check_traces_a_host_route_once() {
    // 10.1.0.32 is in APPL_DB without its mask; it must not also appear as 10.1.0.32/32.
    let (_, out, _) = run(&["--snapshot", "fixtures/bundle", "check"]);
    assert_eq!(out.matches("10.1.0.32").count(), 1, "{out}");
}

#[test]
fn check_details_prints_the_full_trace_of_failures_only() {
    let (_, out, _) = run(&["--snapshot", "fixtures/bundle", "check", "--details"]);
    assert!(out.contains("       | trace: Vlan8"), "{out}");
    assert!(!out.contains("       | trace: Vlan2"), "{out}");
}

#[test]
fn check_json_has_one_entry_per_object() {
    let (code, out, _) = run(&["--snapshot", "fixtures/bundle", "--json", "check"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let entries = v["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 10);
    let failed = entries.iter().filter(|e| e["status"] == "failed").count();
    assert_eq!(failed, 4);
}

#[test]
fn check_exits_0_when_everything_passes() {
    let d = tmp("all-pass");
    std::fs::write(
        d.join("db.json"),
        r#"{"CONFIG_DB":{"VLAN|Vlan5":{"vlanid":"5"}},"APPL_DB":{"VLAN_TABLE:Vlan5":{}},
            "STATE_DB":{"VLAN_TABLE|Vlan5":{"state":"ok"}},
            "ASIC_DB":{"ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x26":{"SAI_VLAN_ATTR_VLAN_ID":"5"}}}"#,
    )
    .unwrap();
    let (code, out, _) = run(&["--snapshot", d.join("db.json").to_str().unwrap(), "check"]);
    std::fs::remove_dir_all(&d).ok();
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("=> all objects passed"), "{out}");
}

#[test]
fn tar_gz_bundle_gives_the_same_result_as_the_unpacked_bundle() {
    let d = tmp("targz");
    let archive = d.join("sonic_dump_test_20000101_000000.tar.gz");
    pack_bundle(&archive, &[]);
    let from_dir = run(&["--snapshot", "fixtures/bundle", "check"]);
    let from_tar = run(&["--snapshot", archive.to_str().unwrap(), "check"]);
    std::fs::remove_dir_all(&d).ok();
    assert_eq!(from_tar, from_dir);
}

#[test]
fn tar_gz_single_trace_reads_logs_from_the_archive() {
    let d = tmp("targz-logs");
    let archive = d.join("bundle.tgz");
    pack_bundle(&archive, &[]);
    let (code, out, _) = run(&["--snapshot", archive.to_str().unwrap(), "vlan", "Vlan8"]);
    std::fs::remove_dir_all(&d).ok();
    assert_eq!(code, 1);
    assert!(
        out.contains("swss.rec: orchagent last received SET for VLAN_TABLE:Vlan8"),
        "{out}"
    );
}

#[test]
fn archive_entries_cannot_write_outside_the_temporary_folder() {
    let d = tmp("targz-escape");
    let archive = d.join("evil.tar.gz");
    // Entries whose path tries to leave the bundle, and unrelated files: none may be
    // written anywhere; the dump/ and log/ files are rebuilt under fixed names.
    pack_bundle(
        &archive,
        &[
            ("sonic_dump_test_20000101_000000/etc/passwd", b"x"),
            ("sonic_dump_test_20000101_000000/dump/notes.txt", b"x"),
            ("sonic_dump_test_20000101_000000/log/other.log", b"x"),
        ],
    );
    let (code, _, err) = run(&["--snapshot", archive.to_str().unwrap(), "check"]);
    assert_eq!(code, 1, "{err}");
    // Nothing was written next to the archive, and the private folder was removed.
    let left: Vec<_> = std::fs::read_dir(&d).unwrap().collect();
    std::fs::remove_dir_all(&d).ok();
    assert_eq!(left.len(), 1);
}

#[test]
fn archive_without_dumps_is_rejected() {
    let d = tmp("targz-empty");
    let archive = d.join("not-a-bundle.tar.gz");
    let f = std::fs::File::create(&archive).unwrap();
    let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
    let mut t = tar::Builder::new(gz);
    let mut h = tar::Header::new_gnu();
    h.set_size(5);
    h.set_cksum();
    t.append_data(&mut h, "readme.txt", &b"hello"[..]).unwrap();
    t.into_inner().unwrap().finish().unwrap();
    let (code, _, err) = run(&["--snapshot", archive.to_str().unwrap(), "check"]);
    std::fs::remove_dir_all(&d).ok();
    assert_eq!(code, 2);
    assert!(err.contains("no dump/<DB>.json files"), "{err}");
}

#[test]
fn corrupt_archive_is_an_error_not_a_crash() {
    let d = tmp("targz-corrupt");
    let archive = d.join("broken.tar.gz");
    std::fs::write(&archive, [0x1f, 0x8b, 0x08, 0x00, 0xde, 0xad, 0xbe, 0xef]).unwrap();
    let (code, _, err) = run(&["--snapshot", archive.to_str().unwrap(), "check"]);
    std::fs::remove_dir_all(&d).ok();
    assert_eq!(code, 2, "{err}");
    assert!(err.starts_with("error:"), "{err}");
}
