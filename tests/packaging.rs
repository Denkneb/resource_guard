#![cfg(target_os = "linux")]

use std::process::Command;

const UNIT: &str = include_str!("../packaging/resource-guard.service");
const README: &str = include_str!("../README.md");

#[test]
fn user_service_has_the_expected_lifecycle_and_runtime_contract() {
    assert!(UNIT.contains("ExecStart=%h/.local/bin/resource-guard daemon"));
    assert!(UNIT.contains("Restart=on-failure"));
    assert!(UNIT.contains("KillSignal=SIGTERM"));
    assert!(UNIT.contains("RuntimeDirectory=resource-guard"));
    assert!(UNIT.contains("RuntimeDirectoryMode=0700"));
    assert!(UNIT.contains("WantedBy=default.target"));
}

#[test]
fn user_service_does_not_request_root_and_keeps_required_access() {
    assert!(!UNIT.lines().any(|line| line.starts_with("User=")));
    assert!(!UNIT.contains("/etc/systemd/system"));
    assert!(!UNIT.contains("ProtectHome="));
    assert!(UNIT.contains("NoNewPrivileges=yes"));
    assert!(UNIT.contains("RestrictAddressFamilies=AF_UNIX"));
    assert!(UNIT.contains("CapabilityBoundingSet=\n"));
}

#[test]
fn readme_install_path_matches_the_unit() {
    assert!(README.contains("$HOME/.local/bin/resource-guard"));
    assert!(README.contains("$HOME/.config/systemd/user/resource-guard.service"));
    assert!(README.contains("systemctl --user enable --now resource-guard.service"));
}

#[test]
fn example_configuration_is_accepted_by_the_binary() {
    let output = Command::new(env!("CARGO_BIN_EXE_resource-guard"))
        .args(["config", "check"])
        .env(
            "RESOURCE_GUARD_CONFIG",
            concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.toml"),
        )
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
