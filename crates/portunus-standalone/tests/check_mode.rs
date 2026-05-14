use assert_cmd::Command;
use predicates::prelude::*;

fn fixture(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    p
}

#[test]
fn check_valid_minimal_exits_0() {
    Command::cargo_bin("portunus-standalone")
        .unwrap()
        .args(["--check", "--config"])
        .arg(fixture("valid_minimal.toml"))
        .assert()
        .code(0);
}

#[test]
fn check_valid_full_exits_0() {
    Command::cargo_bin("portunus-standalone")
        .unwrap()
        .args(["--check", "--config"])
        .arg(fixture("valid_full.toml"))
        .assert()
        .code(0);
}

#[test]
fn check_valid_udp_exits_0() {
    Command::cargo_bin("portunus-standalone")
        .unwrap()
        .args(["--check", "--config"])
        .arg(fixture("valid_udp.toml"))
        .assert()
        .code(0);
}

#[test]
fn check_unknown_field_exits_2() {
    Command::cargo_bin("portunus-standalone")
        .unwrap()
        .args(["--check", "--config"])
        .arg(fixture("invalid_unknown_field.toml"))
        .assert()
        .code(2)
        .stderr(
            predicates::str::contains("unknown field").or(predicates::str::contains("bind_addr")),
        );
}

#[test]
fn check_range_mismatch_exits_2() {
    Command::cargo_bin("portunus-standalone")
        .unwrap()
        .args(["--check", "--config"])
        .arg(fixture("invalid_range_mismatch.toml"))
        .assert()
        .code(2)
        .stderr(
            predicates::str::contains("range size")
                .or(predicates::str::contains("range_len"))
                .or(predicates::str::contains("mismatch")),
        );
}

#[test]
fn check_no_rules_exits_2() {
    Command::cargo_bin("portunus-standalone")
        .unwrap()
        .args(["--check", "--config"])
        .arg(fixture("invalid_no_rules.toml"))
        .assert()
        .code(2)
        .stderr(
            predicates::str::contains("at least one")
                .or(predicates::str::contains("no_rules"))
                .or(predicates::str::contains("NoRules")),
        );
}
