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

const MAX_STDERR_BYTES: usize = 16 * 1024;

fn parse_duration(value: &str) -> std::result::Result<Duration, String> {
    let (number, suffix) = value.split_at(value.len().saturating_sub(1));
    let multiplier = match suffix {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err("duration must end in s, m, h, or d".to_owned()),
    };
    let number = number
        .parse::<u64>()
        .map_err(|_| "duration must start with a positive integer".to_owned())?;
    let seconds = number
        .checked_mul(multiplier)
        .ok_or_else(|| "duration is too large".to_owned())?;
    if seconds == 0 {
        return Err("duration must be greater than zero".to_owned());
    }
    Ok(Duration::from_secs(seconds))
}

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Poll an SSH endpoint and run a LUKS unlock command"
)]
struct Args {
    /// Hostname or IP address of the machine running Dropbear/OpenSSH.
    #[arg(long, env = "REMOTE_LUKS_HOST")]
    host: String,

    /// SSH port.
    #[arg(short = 'p', long, env = "REMOTE_LUKS_PORT", default_value_t = 22)]
    port: u16,

    /// SSH username.
    #[arg(short = 'l', long, env = "REMOTE_LUKS_USER")]
    user: String,

    /// Passphrase sent to the remote unlock command.
    #[arg(long, env = "REMOTE_LUKS_LUKS_PASSWORD", hide_env_values = true)]
    luks_password: String,

    /// Required private SSH identity file whose public key is authorized on the server.
    #[arg(short = 'i', long, env = "REMOTE_LUKS_IDENTITY_FILE")]
    identity_file: PathBuf,

    /// Optional `known_hosts` file used to verify the remote host key. When omitted,
    /// OpenSSH uses its normal known-hosts files.
    #[arg(long, env = "REMOTE_LUKS_KNOWN_HOSTS")]
    known_hosts: Option<PathBuf>,

    /// Delay after a failed SSH attempt before retrying, such as `1s` or `1m`.
    #[arg(
        long,
        env = "REMOTE_LUKS_FAILURE_INTERVAL",
        default_value = "1s",
        value_parser = parse_duration
    )]
    failure_interval: Duration,

    /// Delay between successful remote command runs in continuous mode.
    #[arg(
        long,
        env = "REMOTE_LUKS_SUCCESS_INTERVAL",
        default_value = "1m",
        value_parser = parse_duration
    )]
    success_interval: Duration,

    /// Retry until the first successful unlock, then exit.
    #[arg(long, env = "REMOTE_LUKS_ONCE", default_value_t = false)]
    once: bool,

    /// Maximum total runtime before exiting with an error, such as `10m`.
    #[arg(
        long,
        env = "REMOTE_LUKS_MAX_RUNTIME",
        value_parser = parse_duration
    )]
    max_runtime: Option<Duration>,

    /// Maximum time allowed for one SSH connection and command attempt, such as `30s` or `1m`.
    #[arg(
        long,
        env = "REMOTE_LUKS_ATTEMPT_TIMEOUT",
        default_value = "30s",
        value_parser = parse_duration
    )]
    attempt_timeout: Duration,

    /// Maximum time OpenSSH may spend establishing one connection, such as `2s` or `10s`.
    #[arg(
        long,
        env = "REMOTE_LUKS_CONNECT_TIMEOUT",
        default_value = "2s",
        value_parser = parse_duration
    )]
    connect_timeout: Duration,

    /// Remote command to run after authentication.
    #[arg(
        long,
        env = "REMOTE_LUKS_COMMAND",
        default_value = "unlock-luks unlock"
    )]
    command: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    init_tracing();
    let args = Args::parse();
    info!(host = %args.host, port = args.port, user = %args.user, "starting SSH polling client");
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
                return Err(AttemptError::Fatal(anyhow!(
                    "SSH attempt exceeded {attempt_timeout:?}"
                )));
            }
        }
        match timeout_at(deadline, stdin.write_all(b"\n")).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                terminate_child(&mut child).await;
                return Err(AttemptError::Fatal(
                    anyhow!(error).context("failed to terminate LUKS passphrase input"),
                ));
            }
            Err(_) => {
                terminate_child(&mut child).await;
                return Err(AttemptError::Fatal(anyhow!(
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
        return Err(AttemptError::Fatal(anyhow!(
            "SSH attempt exceeded {attempt_timeout:?}"
        )));
    };

    if status.success() {
        Ok(())
    } else {
        let stderr =
            str::from_utf8(&stderr).map_or("SSH produced invalid UTF-8 on stderr", str::trim);
        let escaped_stderr = stderr.escape_debug().to_string();
        let rendered_stderr_truncated = escaped_stderr.len() > MAX_STDERR_BYTES;
        let stderr = escaped_stderr
            .chars()
            .take(MAX_STDERR_BYTES)
            .collect::<String>();
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

async fn read_stderr(reader: &mut (impl AsyncRead + Unpin)) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::with_capacity(MAX_STDERR_BYTES.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    let mut truncated = false;

    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            return Ok((output, truncated));
        }

        let remaining = MAX_STDERR_BYTES.saturating_sub(output.len());
        let bytes_to_keep = remaining.min(bytes_read);
        output.extend_from_slice(&buffer[..bytes_to_keep]);
        truncated |= bytes_to_keep < bytes_read;
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
    let target = OsString::from(format!("{}@{}", args.user, args.host));
    let mut arguments = vec![
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
    ];
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
        OsString::from("-i"),
        args.identity_file.clone().into_os_string(),
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
        OsString::from("-o"),
        OsString::from("ProxyCommand=none"),
        OsString::from("-o"),
        OsString::from("ProxyJump=none"),
    ]);
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

    fn test_args() -> Args {
        Args {
            host: "example.test".to_owned(),
            port: 2222,
            user: "root".to_owned(),
            luks_password: "secret".to_owned(),
            identity_file: PathBuf::from("/tmp/id_ed25519"),
            known_hosts: Some(PathBuf::from("/tmp/known_hosts")),
            failure_interval: Duration::from_secs(3),
            success_interval: Duration::from_secs(60),
            once: false,
            max_runtime: None,
            attempt_timeout: Duration::from_secs(7),
            connect_timeout: Duration::from_secs(2),
            command: "unlock-luks unlock".to_owned(),
        }
    }

    #[test]
    fn builds_public_key_ssh_arguments_with_host_key_verification() {
        let arguments = build_ssh_arguments(&test_args());

        assert_eq!(
            arguments,
            [
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
                "-i",
                "/tmp/id_ed25519",
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
                "-o",
                "ProxyCommand=none",
                "-o",
                "ProxyJump=none",
                "root@example.test",
                "unlock-luks unlock",
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
            "--host",
            "example.test",
            "-l",
            "root",
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
        assert_eq!(args.user, "root");
        assert_eq!(args.known_hosts, Some(PathBuf::from("/tmp/known_hosts")));
        assert!(!args.once);
        assert_eq!(args.failure_interval, Duration::from_secs(1));
        assert_eq!(args.success_interval, Duration::from_secs(60));
        assert_eq!(args.max_runtime, None);
        assert_eq!(args.connect_timeout, Duration::from_secs(2));
        assert_eq!(args.command, "unlock-luks unlock");
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
    fn rejects_zero_duration() {
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("0m").is_err());
    }

    #[test]
    fn parses_once_option() {
        let args = Args::try_parse_from([
            "remote-luks-unlocker",
            "--host",
            "example.test",
            "-l",
            "root",
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
        assert!(
            result
                .expect_err("the fake SSH process should time out")
                .to_string()
                .contains("SSH attempt exceeded")
        );
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
        assert!(
            result
                .expect_err("the fake SSH process should time out while reading stdin")
                .to_string()
                .contains("SSH attempt exceeded")
        );
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
            "#!/bin/sh\ncat >/dev/null\nyes x | head -c 32768 >&2\nexit 1\n",
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
