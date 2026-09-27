use std::{
    env,
    error::Error,
    ffi::OsString,
    fmt::{Display, Formatter},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    str,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::{Instant, sleep, timeout, timeout_at},
};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

use remote_luks_unlocker::cli::Args;

const MAX_STDERR_BYTES: usize = 16 * 1024;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    init_tracing();
    let args = Args::parse();
    info!(destination = %args.destination, port = args.port, "starting SSH polling client");
    let ssh = find_ssh()
        .await
        .context("OpenSSH is required but the `ssh` binary was not found")?;
    info!(path = %ssh.display(), "found OpenSSH binary");
    let result = tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("received Ctrl+C; stopping SSH polling");
            Err(anyhow!("interrupted"))
        },
        result = async {
            let ssh_arguments = build_ssh_arguments(&args);
            run_polling(&args, &ssh, &ssh_arguments).await
        } => result,
    };
    if let Err(error) = &result {
        error!(error = %error, "SSH polling stopped with an error");
    }
    result
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn find_ssh() -> Result<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    for directory in env::split_paths(&path) {
        let candidate = directory.join("ssh");
        if let Ok(metadata) = fs::metadata(&candidate).await
            && metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
        {
            debug!(path = %candidate.display(), "found executable SSH candidate");
            return Ok(candidate);
        }
    }

    bail!("ssh binary is not installed or is not executable")
}

async fn poll_until_connected(args: &Args, ssh: &Path, ssh_arguments: &[OsString]) -> Result<()> {
    let failure_interval = args.failure_interval;

    loop {
        debug!(port = args.port, "attempting SSH authentication");
        let delay = match connect_and_run(
            ssh,
            ssh_arguments,
            args.attempt_timeout,
            &args.luks_password,
        )
        .await
        {
            Ok(()) => {
                info!("SSH authentication and remote command succeeded");
                if args.once {
                    return Ok(());
                }
                args.success_interval
            }
            Err(AttemptError::Retry(error)) => {
                warn!(error = %error, "SSH attempt failed; will retry");
                failure_interval
            }
            Err(AttemptError::Fatal(error)) => return Err(error),
        };
        sleep(delay).await;
    }
}

async fn run_polling(args: &Args, ssh: &Path, ssh_arguments: &[OsString]) -> Result<()> {
    let polling = poll_until_connected(args, ssh, ssh_arguments);
    if let Some(max_runtime) = args.max_runtime {
        timeout(max_runtime, polling)
            .await
            .with_context(|| format!("SSH polling exceeded {max_runtime:?}"))?
    } else {
        polling.await
    }
}

#[derive(Debug)]
enum AttemptError {
    Retry(anyhow::Error),
    Fatal(anyhow::Error),
}

impl Display for AttemptError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retry(error) | Self::Fatal(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for AttemptError {}

async fn connect_and_run(
    ssh: &Path,
    arguments: &[OsString],
    attempt_timeout: Duration,
    luks_password: &str,
) -> std::result::Result<(), AttemptError> {
    let deadline = Instant::now() + attempt_timeout;
    debug!("starting SSH child process");
    let mut child = Command::new(ssh)
        .args(arguments)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to execute {}", ssh.display()))
        .map_err(AttemptError::Retry)?;

    if let Some(mut stdin) = child.stdin.take() {
        match timeout_at(deadline, stdin.write_all(luks_password.as_bytes())).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                terminate_child(&mut child).await;
                return Err(AttemptError::Fatal(anyhow!(error).context(
                    "failed to send the LUKS passphrase to the remote command",
                )));
            }
            Err(_) => {
                terminate_child(&mut child).await;
                return Err(AttemptError::Retry(anyhow!(
                    "SSH attempt exceeded {attempt_timeout:?}"
                )));
            }
        }
    }

    let mut stderr = child
        .stderr
        .take()
        .context("SSH process did not provide a stderr pipe")
        .map_err(AttemptError::Fatal)?;
    let (status, stderr, stderr_truncated) = if let Ok(result) = timeout_at(deadline, async {
        let (status, stderr_result) = tokio::join!(child.wait(), read_stderr(&mut stderr));
        let status = status?;
        let (stderr_output, stderr_truncated) = stderr_result?;
        std::io::Result::Ok((status, stderr_output, stderr_truncated))
    })
    .await
    {
        result
            .with_context(|| format!("failed waiting for {}", ssh.display()))
            .map_err(AttemptError::Fatal)?
    } else {
        terminate_child(&mut child).await;
        return Err(AttemptError::Retry(anyhow!(
            "SSH attempt exceeded {attempt_timeout:?}"
        )));
    };

    if status.success() {
        Ok(())
    } else {
        let stderr =
            str::from_utf8(&stderr).map_or("SSH produced invalid UTF-8 on stderr", str::trim);
        let escaped_stderr = stderr.escape_debug().to_string();
        let (stderr, rendered_stderr_truncated) =
            truncate_with_tail(&escaped_stderr, MAX_STDERR_BYTES);
        let stderr = if stderr_truncated || rendered_stderr_truncated {
            format!("{stderr} [stderr truncated]")
        } else {
            stderr
        };
        debug!(%status, stderr, "SSH child process failed");
        if stderr.is_empty() {
            let error = anyhow!("ssh exited with status {status}");
            return Err(if status.code() == Some(255) {
                AttemptError::Retry(error)
            } else {
                AttemptError::Fatal(error)
            });
        }
        let error = anyhow!("ssh exited with status {status}: {stderr}");
        Err(if status.code() == Some(255) {
            AttemptError::Retry(error)
        } else {
            AttemptError::Fatal(error)
        })
    }
}

fn truncate_with_tail(value: &str, limit: usize) -> (String, bool) {
    let characters = value.chars().collect::<Vec<_>>();
    if characters.len() <= limit {
        return (value.to_owned(), false);
    }

    let head_length = limit / 2;
    let tail_length = limit - head_length;
    let mut truncated = String::with_capacity(limit);
    truncated.extend(&characters[..head_length]);
    truncated.extend(&characters[characters.len() - tail_length..]);
    (truncated, true)
}

async fn read_stderr(reader: &mut (impl AsyncRead + Unpin)) -> std::io::Result<(Vec<u8>, bool)> {
    let head_limit = MAX_STDERR_BYTES / 2;
    let tail_limit = MAX_STDERR_BYTES - head_limit;
    let mut head = Vec::with_capacity(head_limit);
    let mut tail = Vec::with_capacity(tail_limit);
    let mut buffer = [0_u8; 8 * 1024];
    let mut truncated = false;

    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            head.extend_from_slice(&tail);
            return Ok((head, truncated));
        }

        let mut remaining = &buffer[..bytes_read];
        if head.len() < head_limit {
            let bytes_to_keep = (head_limit - head.len()).min(remaining.len());
            head.extend_from_slice(&remaining[..bytes_to_keep]);
            remaining = &remaining[bytes_to_keep..];
        }
        if !remaining.is_empty() {
            truncated = true;
            tail.extend_from_slice(remaining);
            if tail.len() > tail_limit {
                let excess = tail.len() - tail_limit;
                tail.drain(..excess);
            }
        }
    }
}

async fn terminate_child(child: &mut tokio::process::Child) {
    if let Err(error) = child.kill().await {
        debug!(error = %error, "failed to kill SSH child process");
    }
    if let Err(error) = child.wait().await {
        debug!(error = %error, "failed to reap SSH child process");
    }
}

fn build_ssh_arguments(args: &Args) -> Vec<OsString> {
    let target = OsString::from(args.destination.clone());
    let mut arguments = Vec::new();
    if args.ipv4 {
        arguments.push(OsString::from("-4"));
    }
    if args.ipv6 {
        arguments.push(OsString::from("-6"));
    }
    if let Some(bind_interface) = &args.bind_interface {
        arguments.extend([OsString::from("-B"), OsString::from(bind_interface)]);
    }
    if let Some(bind_address) = &args.bind_address {
        arguments.extend([OsString::from("-b"), OsString::from(bind_address)]);
    }
    if args.compression {
        arguments.push(OsString::from("-C"));
    }
    if let Some(cipher_spec) = &args.cipher_spec {
        arguments.extend([OsString::from("-c"), OsString::from(cipher_spec)]);
    }
    if let Some(config_file) = &args.config_file {
        arguments.extend([OsString::from("-F"), OsString::from(config_file)]);
    }
    if let Some(pkcs11_provider) = &args.pkcs11_provider {
        arguments.extend([OsString::from("-I"), OsString::from(pkcs11_provider)]);
    }
    if let Some(jump_host) = &args.jump_host {
        arguments.extend([OsString::from("-J"), OsString::from(jump_host)]);
    }
    if let Some(mac_spec) = &args.mac_spec {
        arguments.extend([OsString::from("-m"), OsString::from(mac_spec)]);
    }
    if let Some(config_tag) = &args.config_tag {
        arguments.extend([OsString::from("-P"), OsString::from(config_tag)]);
    }
    arguments.extend([
        OsString::from("-i"),
        args.identity_file.clone().into_os_string(),
        OsString::from("-p"),
        OsString::from(args.port.to_string()),
        OsString::from("-o"),
        OsString::from("BatchMode=yes"),
        OsString::from("-o"),
        OsString::from("NumberOfPasswordPrompts=1"),
        OsString::from("-o"),
        OsString::from("KbdInteractiveAuthentication=no"),
        OsString::from("-o"),
        OsString::from(format!("ConnectTimeout={}", args.connect_timeout.as_secs())),
    ]);
    arguments.extend([
        OsString::from("-o"),
        OsString::from("StrictHostKeyChecking=yes"),
    ]);
    if let Some(known_hosts) = &args.known_hosts {
        arguments.extend([
            OsString::from("-o"),
            path_option("UserKnownHostsFile", known_hosts),
            OsString::from("-o"),
            OsString::from("GlobalKnownHostsFile=/dev/null"),
        ]);
    }
    arguments.extend([
        OsString::from("-o"),
        OsString::from("PreferredAuthentications=publickey"),
        OsString::from("-o"),
        OsString::from("PubkeyAuthentication=yes"),
        OsString::from("-o"),
        OsString::from("PasswordAuthentication=no"),
        OsString::from("-o"),
        OsString::from("ControlMaster=no"),
        OsString::from("-o"),
        OsString::from("ControlPath=none"),
    ]);
    arguments.extend(std::iter::repeat_n(
        OsString::from("-v"),
        args.verbose as usize,
    ));
    arguments.extend([target, OsString::from(args.command.clone())]);
    arguments
}

fn path_option(name: &str, path: &Path) -> OsString {
    let mut option = OsString::from(name);
    option.push("=");
    option.push(path);
    option
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        os::unix::ffi::OsStringExt,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use remote_luks_unlocker::cli::parse_duration;

    fn test_args() -> Args {
        Args {
            destination: "root@example.test".to_owned(),
            port: 2222,
            luks_password: "secret".to_owned(),
            identity_file: PathBuf::from("/tmp/id_ed25519"),
            ipv4: false,
            ipv6: false,
            bind_interface: None,
            bind_address: None,
            compression: false,
            cipher_spec: None,
            config_file: None,
            pkcs11_provider: None,
            jump_host: None,
            mac_spec: None,
            config_tag: None,
            known_hosts: Some(PathBuf::from("/tmp/known_hosts")),
            failure_interval: Duration::from_secs(3),
            success_interval: Duration::from_secs(60),
            once: false,
            max_runtime: None,
            attempt_timeout: Duration::from_secs(7),
            connect_timeout: Duration::from_secs(2),
            command: "cryptroot-unlock".to_owned(),
            verbose: 0,
        }
    }

    #[test]
    fn builds_public_key_ssh_arguments_with_host_key_verification() {
        let arguments = build_ssh_arguments(&test_args());

        assert_eq!(
            arguments,
            [
                "-i",
                "/tmp/id_ed25519",
                "-p",
                "2222",
                "-o",
                "BatchMode=yes",
                "-o",
                "NumberOfPasswordPrompts=1",
                "-o",
                "KbdInteractiveAuthentication=no",
                "-o",
                "ConnectTimeout=2",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "UserKnownHostsFile=/tmp/known_hosts",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
                "-o",
                "PreferredAuthentications=publickey",
                "-o",
                "PubkeyAuthentication=yes",
                "-o",
                "PasswordAuthentication=no",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPath=none",
                "root@example.test",
                "cryptroot-unlock",
            ]
        );
    }

    #[test]
    fn uses_openssh_known_hosts_defaults_when_not_configured() {
        let mut args = test_args();
        args.known_hosts = None;

        let arguments = build_ssh_arguments(&args);

        assert!(arguments.iter().all(|argument| {
            !argument
                .to_string_lossy()
                .starts_with("UserKnownHostsFile=")
        }));
        assert!(
            arguments
                .iter()
                .all(|argument| argument != "GlobalKnownHostsFile=/dev/null")
        );
    }

    #[test]
    fn builds_public_key_and_known_hosts_arguments() {
        let mut args = test_args();
        args.identity_file = PathBuf::from("/tmp/id_ed25519");
        args.known_hosts = Some(PathBuf::from("/tmp/known_hosts"));

        let arguments = build_ssh_arguments(&args);

        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["-i", "/tmp/id_ed25519"])
        );
        let known_hosts_index = arguments
            .iter()
            .position(|argument| argument == "UserKnownHostsFile=/tmp/known_hosts")
            .expect("known_hosts argument is missing");
        assert_eq!(arguments[known_hosts_index - 1], "-o");
        assert!(
            arguments
                .iter()
                .any(|argument| argument == "PreferredAuthentications=publickey")
        );
        assert!(
            arguments
                .iter()
                .any(|argument| argument == "PasswordAuthentication=no")
        );
        assert!(
            !arguments
                .iter()
                .any(|argument| argument == "StrictHostKeyChecking=no")
        );
    }

    #[test]
    fn preserves_non_utf8_identity_paths() {
        let mut args = test_args();
        let identity_file = OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0x80]);
        args.identity_file = PathBuf::from(&identity_file);

        let arguments = build_ssh_arguments(&args);
        let identity_index = arguments
            .iter()
            .position(|argument| argument == "-i")
            .expect("identity argument is missing");
        assert_eq!(arguments[identity_index + 1], identity_file);
    }

    #[test]
    fn parses_attempt_timeout_default() {
        let args = Args::try_parse_from([
            "remote-luks-unlocker",
            "root@example.test",
            "--luks-password",
            "secret",
            "-i",
            "/tmp/id_ed25519",
            "--known-hosts",
            "/tmp/known_hosts",
            "-p",
            "2222",
        ])
        .expect("default CLI arguments should parse");

        assert_eq!(args.attempt_timeout, Duration::from_secs(30));
        assert_eq!(args.port, 2222);
        assert_eq!(args.destination, "root@example.test");
        assert_eq!(args.known_hosts, Some(PathBuf::from("/tmp/known_hosts")));
        assert!(!args.once);
        assert_eq!(args.failure_interval, Duration::from_secs(15));
        assert_eq!(args.success_interval, Duration::from_secs(60));
        assert_eq!(args.max_runtime, None);
        assert_eq!(args.connect_timeout, Duration::from_secs(2));
        assert_eq!(args.command, "cryptroot-unlock");
    }

    #[test]
    fn configures_connect_timeout() {
        let mut args = test_args();
        args.connect_timeout = Duration::from_secs(11);

        let arguments = build_ssh_arguments(&args);

        assert!(
            arguments
                .iter()
                .any(|argument| argument == "ConnectTimeout=11")
        );
    }

    #[test]
    fn forwards_supported_ssh_connection_options() {
        let mut args = test_args();
        args.ipv4 = true;
        args.bind_interface = Some("en0".to_owned());
        args.bind_address = Some("192.0.2.10".to_owned());
        args.compression = true;
        args.cipher_spec = Some("chacha20-poly1305@openssh.com".to_owned());
        args.config_file = Some("/tmp/ssh_config".to_owned());
        args.pkcs11_provider = Some("/tmp/pkcs11.so".to_owned());
        args.jump_host = Some("jump.example.test".to_owned());
        args.mac_spec = Some("hmac-sha2-256".to_owned());
        args.config_tag = Some("prod".to_owned());
        args.verbose = 2;

        let arguments = build_ssh_arguments(&args);

        for expected in [
            "-4",
            "-B",
            "en0",
            "-b",
            "192.0.2.10",
            "-C",
            "-c",
            "chacha20-poly1305@openssh.com",
            "-F",
            "/tmp/ssh_config",
            "-I",
            "/tmp/pkcs11.so",
            "-J",
            "jump.example.test",
            "-m",
            "hmac-sha2-256",
            "-P",
            "prod",
        ] {
            assert!(arguments.iter().any(|argument| argument == expected));
        }
        assert_eq!(
            arguments
                .iter()
                .filter(|argument| *argument == "-v")
                .count(),
            2
        );
        assert!(
            arguments
                .iter()
                .all(|argument| argument != "ProxyCommand=none" && argument != "ProxyJump=none")
        );
    }

    #[test]
    fn rejects_zero_duration() {
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("0m").is_err());
    }

    #[test]
    fn parses_once_option() {
        let args = Args::try_parse_from([
            "remote-luks-unlocker",
            "root@example.test",
            "--luks-password",
            "secret",
            "-i",
            "/tmp/id_ed25519",
            "--known-hosts",
            "/tmp/known_hosts",
            "--once",
            "--max-runtime",
            "10m",
            "--success-interval",
            "2m",
        ])
        .expect("--once should parse");

        assert!(args.once);
        assert_eq!(args.max_runtime, Some(Duration::from_secs(600)));
        assert_eq!(args.success_interval, Duration::from_secs(120));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminates_an_ssh_attempt_that_exceeds_its_timeout() {
        let script_path = env::temp_dir().join(format!(
            "remote-luks-unlocker-terminates-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before the Unix epoch")
                .as_nanos()
        ));
        fs::write(&script_path, "#!/bin/sh\nwhile :; do :; done\n")
            .expect("failed to write fake SSH script");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
            .expect("failed to make fake SSH script executable");
        let result = connect_and_run(&script_path, &[], Duration::from_millis(50), "secret").await;

        fs::remove_file(&script_path).expect("failed to remove fake SSH script");
        let error = result.expect_err("the fake SSH process should time out");
        assert!(matches!(&error, AttemptError::Retry(_)));
        assert!(error.to_string().contains("SSH attempt exceeded"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bounds_password_delivery_by_the_attempt_timeout() {
        let script_path = env::temp_dir().join(format!(
            "remote-luks-unlocker-password-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before the Unix epoch")
                .as_nanos()
        ));
        fs::write(&script_path, "#!/bin/sh\nwhile :; do :; done\n")
            .expect("failed to write fake SSH script");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
            .expect("failed to make fake SSH script executable");
        let result = connect_and_run(
            &script_path,
            &[],
            Duration::from_millis(50),
            &"secret".repeat(128 * 1024),
        )
        .await;

        fs::remove_file(&script_path).expect("failed to remove fake SSH script");
        let error = result.expect_err("the fake SSH process should time out while reading stdin");
        assert!(matches!(&error, AttemptError::Retry(_)));
        assert!(error.to_string().contains("SSH attempt exceeded"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn once_mode_exits_after_success() {
        let script_path = env::temp_dir().join(format!(
            "remote-luks-unlocker-once-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before the Unix epoch")
                .as_nanos()
        ));
        fs::write(&script_path, "#!/bin/sh\ncat >/dev/null\nexit 0\n")
            .expect("failed to write fake SSH script");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
            .expect("failed to make fake SSH script executable");
        let mut args = test_args();
        args.once = true;

        let result = poll_until_connected(&args, &script_path, &[]).await;

        fs::remove_file(&script_path).expect("failed to remove fake SSH script");
        result.expect("once mode should exit after the first success");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn max_runtime_stops_continuous_mode() {
        let script_path = env::temp_dir().join(format!(
            "remote-luks-unlocker-runtime-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before the Unix epoch")
                .as_nanos()
        ));
        fs::write(&script_path, "#!/bin/sh\ncat >/dev/null\nexit 0\n")
            .expect("failed to write fake SSH script");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
            .expect("failed to make fake SSH script executable");
        let mut args = test_args();
        args.max_runtime = Some(Duration::from_millis(50));

        let result = run_polling(&args, &script_path, &[]).await;

        fs::remove_file(&script_path).expect("failed to remove fake SSH script");
        assert!(
            result
                .expect_err("continuous mode should stop at max runtime")
                .to_string()
                .contains("SSH polling exceeded")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bounds_ssh_stderr_output() {
        let script_path = env::temp_dir().join(format!(
            "remote-luks-unlocker-stderr-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before the Unix epoch")
                .as_nanos()
        ));
        fs::write(
            &script_path,
            "#!/bin/sh\ncat >/dev/null\nprintf 'stderr-start\\n' >&2\nyes x | head -c 32768 >&2\nprintf '\\nstderr-end\\n' >&2\nexit 1\n",
        )
        .expect("failed to write fake SSH script");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
            .expect("failed to make fake SSH script executable");
        let result = connect_and_run(&script_path, &[], Duration::from_secs(1), "secret").await;

        fs::remove_file(&script_path).expect("failed to remove fake SSH script");
        let error = result
            .expect_err("the fake SSH process should return a failure")
            .to_string();
        assert!(error.contains("stderr truncated"));
        assert!(error.contains("stderr-start"));
        assert!(error.contains("stderr-end"));
        assert!(error.len() < 20_000);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completes_an_ssh_attempt_when_the_remote_command_succeeds() {
        let script_path = env::temp_dir().join(format!(
            "remote-luks-unlocker-success-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before the Unix epoch")
                .as_nanos()
        ));
        fs::write(
            &script_path,
            "#!/bin/sh\npassword=$(cat)\n[ \"$password\" = secret ]\n",
        )
        .expect("failed to write fake SSH script");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
            .expect("failed to make fake SSH script executable");
        let result = connect_and_run(&script_path, &[], Duration::from_secs(1), "secret").await;

        fs::remove_file(&script_path).expect("failed to remove fake SSH script");
        result.expect("successful fake SSH command should return success");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reports_ssh_stderr_when_the_remote_command_fails() {
        let script_path = env::temp_dir().join(format!(
            "remote-luks-unlocker-failure-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before the Unix epoch")
                .as_nanos()
        ));
        fs::write(
            &script_path,
            "#!/bin/sh\ncat >/dev/null\necho 'connection refused' >&2\nexit 255\n",
        )
        .expect("failed to write fake SSH script");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
            .expect("failed to make fake SSH script executable");
        let result = connect_and_run(&script_path, &[], Duration::from_secs(1), "secret").await;

        fs::remove_file(&script_path).expect("failed to remove fake SSH script");
        let error = result.expect_err("the fake SSH process should fail");
        assert!(error.to_string().contains("connection refused"));
        assert!(error.to_string().contains("exit status: 255"));
    }
}
