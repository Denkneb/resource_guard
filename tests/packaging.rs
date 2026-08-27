#![cfg(target_os = "linux")]

use std::{fs, process::Command};

use tempfile::TempDir;

const UNIT: &str = include_str!("../packaging/resource-guard.service");
const CARGO_MANIFEST: &str = include_str!("../Cargo.toml");
const CI_WORKFLOW: &str = include_str!("../.github/workflows/ci.yml");
const README: &str = include_str!("../README.md");
const RELEASE_WORKFLOW: &str = include_str!("../.github/workflows/release.yml");
const PACKAGE_SCRIPT: &str = include_str!("../scripts/package-release.sh");
const DESKTOP_ENTRY: &str = include_str!("../packaging/io.github.denkneb.ResourceGuard.desktop");

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
    assert!(!UNIT.contains("CapabilityBoundingSet="));
    assert!(!UNIT.contains("AmbientCapabilities="));
    for incompatible_directive in ["PrivateDevices=", "ProtectClock=", "ProtectKernelModules="] {
        assert!(!UNIT.contains(incompatible_directive));
    }
}

#[test]
fn readme_install_path_matches_the_unit() {
    assert!(README.contains("$HOME/.local/bin/resource-guard"));
    assert!(README.contains("$HOME/.config/systemd/user/resource-guard.service"));
    assert!(README.contains("systemctl --user enable --now resource-guard.service"));
    assert!(README.contains("io.github.denkneb.ResourceGuard.desktop"));
}

#[test]
fn package_declares_and_ci_checks_the_msrv() {
    assert!(CARGO_MANIFEST.contains("rust-version = \"1.95\""));
    assert!(CI_WORKFLOW.contains("dtolnay/rust-toolchain@1.95.0"));
    assert!(CI_WORKFLOW.contains("cargo check --locked --all-targets --all-features"));
}

#[test]
fn desktop_entry_provides_the_notification_application_identity() {
    assert!(DESKTOP_ENTRY.contains("Type=Application"));
    assert!(DESKTOP_ENTRY.contains("Name=Resource Guard"));
    assert!(DESKTOP_ENTRY.contains("NoDisplay=true"));
    assert!(DESKTOP_ENTRY.contains("Exec=resource-guard status"));
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

#[test]
fn release_workflow_is_tag_only_and_uses_the_packaging_script() {
    assert!(RELEASE_WORKFLOW.contains("tags:"));
    assert!(RELEASE_WORKFLOW.contains("\"v*.*.*\""));
    assert!(!RELEASE_WORKFLOW.contains("workflow_dispatch:"));
    assert!(RELEASE_WORKFLOW.contains("./scripts/package-release.sh"));
    assert!(RELEASE_WORKFLOW.contains("cargo fmt --all -- --check"));
    assert!(
        RELEASE_WORKFLOW
            .contains("cargo clippy --locked --all-targets --all-features -- -D warnings")
    );
    assert!(RELEASE_WORKFLOW.contains("cargo test --locked --all-targets --all-features"));
    assert!(RELEASE_WORKFLOW.contains("sha256sum --check"));
    assert!(RELEASE_WORKFLOW.contains("gh release create"));
    assert!(RELEASE_WORKFLOW.contains("--verify-tag"));
}

#[test]
fn release_script_rejects_a_version_that_does_not_match_cargo() {
    let output = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/scripts/package-release.sh"
    ))
    .arg("999.0.0")
    .output()
    .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not match Cargo package version")
    );
}

#[test]
fn release_archives_are_reproducible_and_contain_the_installation_payload() {
    let first_output = TempDir::new().unwrap();
    let second_output = TempDir::new().unwrap();

    package_test_release(first_output.path());
    package_test_release(second_output.path());

    let archive_name = format!(
        "resource-guard-{}-x86_64-unknown-linux-gnu.tar.gz",
        env!("CARGO_PKG_VERSION")
    );
    let first_archive = first_output.path().join(&archive_name);
    let second_archive = second_output.path().join(&archive_name);
    assert_eq!(
        fs::read(&first_archive).unwrap(),
        fs::read(&second_archive).unwrap()
    );

    let checksum = Command::new("sha256sum")
        .args(["--check", &format!("{archive_name}.sha256")])
        .current_dir(first_output.path())
        .output()
        .unwrap();
    assert!(
        checksum.status.success(),
        "{}",
        String::from_utf8_lossy(&checksum.stderr)
    );

    let listing = Command::new("tar")
        .args(["-tzf", first_archive.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(listing.status.success());
    let listing = String::from_utf8(listing.stdout).unwrap();
    let archive_root = format!(
        "resource-guard-{}-x86_64-unknown-linux-gnu",
        env!("CARGO_PKG_VERSION")
    );
    for path in [
        "bin/resource-guard",
        "config/config.example.toml",
        "systemd/resource-guard.service",
        "applications/io.github.denkneb.ResourceGuard.desktop",
        "README.md",
        "CHANGELOG.md",
        "LICENSE",
    ] {
        assert!(
            listing
                .lines()
                .any(|line| line == format!("{archive_root}/{path}"))
        );
    }

    assert!(PACKAGE_SCRIPT.contains("--sort=name"));
    assert!(PACKAGE_SCRIPT.contains("gzip -n"));
    assert!(PACKAGE_SCRIPT.contains("SOURCE_DATE_EPOCH"));
}

fn package_test_release(output_directory: &std::path::Path) {
    let output = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/scripts/package-release.sh"
    ))
    .args([
        env!("CARGO_PKG_VERSION"),
        output_directory.to_str().unwrap(),
    ])
    .env(
        "RESOURCE_GUARD_RELEASE_BINARY",
        env!("CARGO_BIN_EXE_resource-guard"),
    )
    .env("SOURCE_DATE_EPOCH", "1700000000")
    .output()
    .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
