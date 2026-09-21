use std::{
    env, fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PASSWORD: &str = "test-passphrase";
static FIXTURE_LOCK: Mutex<()> = Mutex::new(());
static PODMAN_READY: OnceLock<()> = OnceLock::new();

struct Fixture {
    guard: FixtureGuard,
    port: u16,
}

impl Fixture {
    fn start() -> Self {
        ensure_podman_machine();
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dropbear-luks");
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

        let guard = FixtureGuard {
            fixture_dir: fixture_dir.clone(),
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

        Self { guard, port }
    }

    fn identity_file(&self) -> PathBuf {
        self.guard.data_dir.join("ssh/id_ed25519")
    }

    fn known_hosts_file(&self) -> PathBuf {
        let path = self.guard.data_dir.join("known_hosts");
        for _ in 0..10 {
            let output = Command::new("ssh-keyscan")
                .args(["-p", &self.port.to_string(), "127.0.0.1"])
                .output()
                .expect("ssh-keyscan is required for host-key tests");
            if output.status.success() && !output.stdout.is_empty() {
                fs::write(&path, output.stdout).expect("failed to write known_hosts fixture");
                return path;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("ssh-keyscan could not read the Dropbear host key");
    }

    fn run_cli(&self, args: &[String], environment: &[(&str, &str)]) -> Option<Output> {
        let binary = env::var("CARGO_BIN_EXE_remote-luks-unlocker")
            .or_else(|_| env::var("CARGO_BIN_EXE_remote_luks_unlocker"))
            .expect("Cargo did not provide the polling binary path");
        let mut command = Command::new(binary);
        command.args(args);
        for (name, value) in environment {
            command.env(name, value);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to execute the polling client");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if child
                .try_wait()
                .expect("failed to poll the polling client")
                .is_some()
            {
                return Some(
                    child
                        .wait_with_output()
                        .expect("failed to collect polling client output"),
                );
            }
            if std::time::Instant::now() >= deadline {
                child.kill().ok();
                child.wait().ok();
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn common_args(&self) -> Vec<String> {
        vec![
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            self.port.to_string(),
            "--user".into(),
            "root".into(),
            "--identity-file".into(),
            self.identity_file().to_string_lossy().into_owned(),
            "--interval-seconds".into(),
            "1".into(),
            "--attempt-timeout-seconds".into(),
            "10".into(),
            "--command".into(),
            "unlock-luks unlock".into(),
        ]
    }
}

struct FixtureGuard {
    fixture_dir: PathBuf,
    data_dir: PathBuf,
    container_name: String,
}

impl Drop for FixtureGuard {
    fn drop(&mut self) {
        let _ = Command::new("make")
            .args(["stop-test-server", "clean-test-data"])
            .current_dir(&self.fixture_dir)
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
    PODMAN_READY.get_or_init(|| {
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
    });
}

#[test]
fn public_key_authentication_without_host_key_verification() {
    let _lock = FIXTURE_LOCK.lock().unwrap();
    let fixture = Fixture::start();
    let mut args = fixture.common_args();
    args.extend(["--luks-password".into(), PASSWORD.into()]);
    let result = fixture
        .run_cli(&args, &[])
        .expect("polling client timed out");
    assert!(
        result.status.success(),
        "CLI public-key flow failed: {result:?}"
    );
}

#[test]
fn identity_and_host_key_with_cli_options() {
    let _lock = FIXTURE_LOCK.lock().unwrap();
    let fixture = Fixture::start();
    let mut args = fixture.common_args();
    args.extend([
        "--luks-password".into(),
        PASSWORD.into(),
        "--known-hosts".into(),
        fixture.known_hosts_file().to_string_lossy().into_owned(),
    ]);
    let result = fixture
        .run_cli(&args, &[])
        .expect("polling client timed out");
    assert!(
        result.status.success(),
        "CLI key/host-key flow failed: {result:?}"
    );
}

#[test]
fn all_options_from_environment() {
    let _lock = FIXTURE_LOCK.lock().unwrap();
    let fixture = Fixture::start();
    let known_hosts = fixture.known_hosts_file();
    let port = fixture.port.to_string();
    let identity_file = fixture.identity_file();
    let identity_file = identity_file.to_string_lossy();
    let known_hosts = known_hosts.to_string_lossy();
    let environment = [
        ("REMOTE_LUKS_HOST", "127.0.0.1"),
        ("REMOTE_LUKS_PORT", port.as_str()),
        ("REMOTE_LUKS_USER", "root"),
        ("REMOTE_LUKS_LUKS_PASSWORD", PASSWORD),
        ("REMOTE_LUKS_IDENTITY_FILE", identity_file.as_ref()),
        ("REMOTE_LUKS_KNOWN_HOSTS", known_hosts.as_ref()),
        ("REMOTE_LUKS_INTERVAL_SECONDS", "1"),
        ("REMOTE_LUKS_ATTEMPT_TIMEOUT_SECONDS", "10"),
        ("REMOTE_LUKS_COMMAND", "unlock-luks unlock"),
    ];
    let result = fixture
        .run_cli(&[], &environment)
        .expect("polling client timed out");
    assert!(
        result.status.success(),
        "environment flow failed: {result:?}"
    );
}

#[test]
fn mismatched_host_key_is_rejected() {
    let _lock = FIXTURE_LOCK.lock().unwrap();
    let fixture = Fixture::start();
    let wrong_known_hosts = fixture.guard.data_dir.join("wrong_known_hosts");
    fs::write(
        &wrong_known_hosts,
        format!("[127.0.0.1]:{} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n", fixture.port),
    )
    .expect("failed to write mismatched known_hosts fixture");
    let mut args = fixture.common_args();
    args.extend([
        "--luks-password".into(),
        PASSWORD.into(),
        "--identity-file".into(),
        fixture.identity_file().to_string_lossy().into_owned(),
        "--known-hosts".into(),
        wrong_known_hosts.to_string_lossy().into_owned(),
    ]);
    let result = fixture.run_cli(&args, &[]);
    assert!(
        result.is_none_or(|output| !output.status.success()),
        "mismatched host key was accepted"
    );
}
