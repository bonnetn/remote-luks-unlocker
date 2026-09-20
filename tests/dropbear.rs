use std::{env, path::Path, process::Command};

const PASSWORD: &str = "test-passphrase";

struct FixtureGuard<'a> {
    fixture_dir: &'a Path,
}

impl Drop for FixtureGuard<'_> {
    fn drop(&mut self) {
        let _ = Command::new("make")
            .args(["stop-test-server", "clean-test-data"])
            .current_dir(self.fixture_dir)
            .status();
    }
}

#[test]
fn polling_client_authenticates_to_dropbear_fixture() {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dropbear-luks");

    let clean = Command::new("make")
        .arg("clean-test-data")
        .current_dir(&fixture_dir)
        .status()
        .expect("failed to run the fixture cleanup command");
    assert!(clean.success(), "fixture cleanup failed");

    let _fixture = FixtureGuard {
        fixture_dir: &fixture_dir,
    };

    let start = Command::new("make")
        .arg("test-server")
        .current_dir(&fixture_dir)
        .status()
        .expect("failed to run the fixture startup command");
    assert!(start.success(), "Dropbear fixture failed to start");
    let binary = env::var("CARGO_BIN_EXE_remote-luks-unlocker")
        .or_else(|_| env::var("CARGO_BIN_EXE_remote_luks_unlocker"))
        .expect("Cargo did not provide the polling binary path");
    let result = Command::new(binary)
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            "2222",
            "--user",
            "root",
            "--wait-seconds",
            "30",
            "--interval-seconds",
            "1",
            "--command",
            "unlock-luks unlock",
        ])
        .env("REMOTE_LUKS_PASSWORD", PASSWORD)
        .output()
        .expect("failed to execute the polling client");

    assert!(
        result.status.success(),
        "polling client failed: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
