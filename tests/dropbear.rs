use std::{
    env, fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const PASSWORD: &str = "test-passphrase";

struct FixtureGuard<'a> {
    fixture_dir: &'a Path,
    data_dir: PathBuf,
    container_name: String,
}

impl Drop for FixtureGuard<'_> {
    fn drop(&mut self) {
        let _ = Command::new("make")
            .args(["stop-test-server", "clean-test-data"])
            .current_dir(self.fixture_dir)
            .env("DROPBEAR_CONTAINER_NAME", &self.container_name)
            .env("DROPBEAR_DATA_DIR", &self.data_dir)
            .status();
        let _ = fs::remove_dir_all(&self.data_dir);
    }
}

fn temporary_data_dir() -> PathBuf {
    let pid = std::process::id();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_nanos();
    let path = env::temp_dir().join(format!("remote-luks-unlocker-{pid}-{timestamp}"));
    fs::create_dir(&path).expect("failed to create the fixture data directory");
    path
}

fn ensure_podman_machine() {
    let machine =
        env::var("PODMAN_MACHINE_NAME").unwrap_or_else(|_| "podman-machine-default".to_owned());
    let connection =
        env::var("PODMAN_CONNECTION").unwrap_or_else(|_| "podman-machine-default".to_owned());
    let inspect = Command::new("podman")
        .args(["machine", "inspect", "--format", "{{.State}}", &machine])
        .output()
        .expect("Podman is required for the Dropbear integration test");
    assert!(
        inspect.status.success(),
        "Podman machine {machine:?} does not exist: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );

    if String::from_utf8_lossy(&inspect.stdout).trim() != "running" {
        let start = Command::new("podman")
            .args(["machine", "start", &machine])
            .status()
            .expect("failed to start the Podman machine");
        assert!(
            start.success(),
            "failed to start Podman machine {machine:?}"
        );
    }

    let select = Command::new("podman")
        .args(["system", "connection", "default", &connection])
        .status()
        .expect("failed to select the Podman connection");
    assert!(
        select.success(),
        "failed to select Podman connection {connection:?}"
    );
}

#[test]
fn polling_client_authenticates_to_dropbear_fixture() {
    ensure_podman_machine();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dropbear-luks");
    let data_dir = temporary_data_dir();
    let data_id = data_dir
        .file_name()
        .expect("temporary data directory has no filename")
        .to_string_lossy();
    let container_name = format!("remote-luks-dropbear-{data_id}");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("failed to allocate a test port");
    let port = listener
        .local_addr()
        .expect("failed to inspect the test port")
        .port();
    drop(listener);
    let fixture = FixtureGuard {
        fixture_dir: &fixture_dir,
        data_dir: data_dir.clone(),
        container_name: container_name.clone(),
    };

    let clean = Command::new("make")
        .arg("clean-test-data")
        .current_dir(&fixture_dir)
        .env("DROPBEAR_DATA_DIR", &data_dir)
        .status()
        .expect("failed to run the fixture cleanup command");
    assert!(clean.success(), "fixture cleanup failed");

    let start = Command::new("make")
        .arg("test-server")
        .current_dir(&fixture_dir)
        .env("DROPBEAR_DATA_DIR", &data_dir)
        .env("DROPBEAR_CONTAINER_NAME", &container_name)
        .env("DROPBEAR_PORT", port.to_string())
        .status()
        .expect("failed to run the fixture startup command");
    assert!(start.success(), "Dropbear fixture failed to start");

    let _fixture = fixture;
    let binary = env::var("CARGO_BIN_EXE_remote-luks-unlocker")
        .or_else(|_| env::var("CARGO_BIN_EXE_remote_luks_unlocker"))
        .expect("Cargo did not provide the polling binary path");
    let result = Command::new(binary)
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
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

    let state = fs::read_to_string(data_dir.join("state/state"))
        .expect("fixture did not write its unlock state");
    assert_eq!(state.trim(), "UNLOCKED", "remote password was not accepted");
}
