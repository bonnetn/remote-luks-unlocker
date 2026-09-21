use std::{
    env,
    net::SocketAddr,
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
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, lookup_host},
    process::Command,
    time::{sleep, timeout},
};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

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

    /// `known_hosts` file used to verify the remote host key.
    #[arg(long, env = "REMOTE_LUKS_KNOWN_HOSTS")]
    known_hosts: Option<PathBuf>,

    /// Seconds between port checks and login attempts.
    #[arg(
        long,
        env = "REMOTE_LUKS_INTERVAL_SECONDS",
        default_value_t = 1,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    interval_seconds: u64,

    /// Maximum time allowed for one SSH connection and command attempt.
    #[arg(
        long,
        env = "REMOTE_LUKS_ATTEMPT_TIMEOUT_SECONDS",
        default_value_t = 30,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    attempt_timeout_seconds: u64,

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
            let endpoint = resolve_endpoint(&args.host, args.port).await?;
            debug!(addresses = ?endpoint, "resolved SSH endpoint");
            let ssh_arguments = build_ssh_arguments(&args);
            poll_until_connected(&args, &ssh, &ssh_arguments, &endpoint).await
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

async fn resolve_endpoint(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    debug!(host, port, "resolving SSH endpoint");
    lookup_host((host, port))
        .await
        .with_context(|| format!("could not resolve {host}:{port}"))
        .map(Iterator::collect::<Vec<_>>)
}

async fn poll_until_connected(
    args: &Args,
    ssh: &Path,
    ssh_arguments: &[String],
    endpoint: &[SocketAddr],
) -> Result<()> {
    let interval = Duration::from_secs(args.interval_seconds);

    loop {
        let port_open = port_is_open(endpoint).await;
        if port_open {
            debug!(
                port = args.port,
                "SSH port is open; attempting authentication"
            );
            match connect_and_run(
                ssh,
                ssh_arguments,
                Duration::from_secs(args.attempt_timeout_seconds),
                &args.luks_password,
            )
            .await
            {
                Ok(()) => {
                    info!("SSH authentication and remote command succeeded");
                    return Ok(());
                }
                Err(error) => warn!(error = %error, "SSH attempt failed; will retry"),
            }
        } else {
            debug!("SSH port is not open; will retry");
        }

        sleep(interval).await;
    }
}

async fn port_is_open(endpoint: &[SocketAddr]) -> bool {
    for address in endpoint {
        if timeout(Duration::from_secs(1), TcpStream::connect(address))
            .await
            .is_ok_and(|result| result.is_ok())
        {
            return true;
        }
    }
    false
}

async fn connect_and_run(
    ssh: &Path,
    arguments: &[String],
    attempt_timeout: Duration,
    luks_password: &str,
) -> Result<()> {
    debug!("starting SSH child process");
    let mut child = Command::new(ssh)
        .args(arguments)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to execute {}", ssh.display()))?;

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(luks_password.as_bytes()).await {
            terminate_child(&mut child).await;
            return Err(error).context("failed to send the LUKS passphrase to the remote command");
        }
        if let Err(error) = stdin.write_all(b"\n").await {
            terminate_child(&mut child).await;
            return Err(error).context("failed to terminate LUKS passphrase input");
        }
    }

    let mut stderr = child
        .stderr
        .take()
        .context("SSH process did not provide a stderr pipe")?;
    let mut stderr_output = Vec::new();
    let (status, stderr) = if let Ok(result) = timeout(attempt_timeout, async {
        let (status, stderr_result) =
            tokio::join!(child.wait(), stderr.read_to_end(&mut stderr_output));
        let status = status?;
        stderr_result?;
        std::io::Result::Ok((status, stderr_output))
    })
    .await
    {
        result.with_context(|| format!("failed waiting for {}", ssh.display()))?
    } else {
        terminate_child(&mut child).await;
        bail!("SSH attempt exceeded {attempt_timeout:?}")
    };

    if status.success() {
        Ok(())
    } else {
        let stderr = str::from_utf8(&stderr)
            .map(str::trim)
            .unwrap_or("SSH produced invalid UTF-8 on stderr");
        debug!(%status, stderr, "SSH child process failed");
        if stderr.is_empty() {
            bail!("ssh exited with status {status}")
        } else {
            bail!("ssh exited with status {status}: {stderr}")
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

fn build_ssh_arguments(args: &Args) -> Vec<String> {
    let target = format!("{}@{}", args.user, args.host);
    let mut arguments = vec![
        "-p".to_owned(),
        args.port.to_string(),
        "-o".to_owned(),
        "BatchMode=no".to_owned(),
        "-o".to_owned(),
        "NumberOfPasswordPrompts=1".to_owned(),
        "-o".to_owned(),
        "KbdInteractiveAuthentication=no".to_owned(),
        "-o".to_owned(),
        "ConnectTimeout=2".to_owned(),
    ];
    if let Some(known_hosts) = args.known_hosts.as_deref() {
        arguments.extend([
            "-o".to_owned(),
            "StrictHostKeyChecking=yes".to_owned(),
            "-o".to_owned(),
            format!("UserKnownHostsFile={}", known_hosts.display()),
        ]);
    } else {
        arguments.extend([
            "-o".to_owned(),
            "StrictHostKeyChecking=no".to_owned(),
            "-o".to_owned(),
            "UserKnownHostsFile=/dev/null".to_owned(),
        ]);
    }
    arguments.extend([
        "-i".to_owned(),
        args.identity_file.to_string_lossy().into_owned(),
        "-o".to_owned(),
        "IdentitiesOnly=yes".to_owned(),
        "-o".to_owned(),
        "PreferredAuthentications=publickey".to_owned(),
        "-o".to_owned(),
        "PubkeyAuthentication=yes".to_owned(),
        "-o".to_owned(),
        "PasswordAuthentication=no".to_owned(),
    ]);
    arguments.extend([target, args.command.clone()]);
    arguments
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::Ipv4Addr,
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
            known_hosts: None,
            interval_seconds: 3,
            attempt_timeout_seconds: 7,
            command: "unlock-luks unlock".to_owned(),
        }
    }

    #[test]
    fn builds_public_key_ssh_arguments_without_host_key_verification() {
        let arguments = build_ssh_arguments(&test_args());

        assert_eq!(
            arguments,
            [
                "-p",
                "2222",
                "-o",
                "BatchMode=no",
                "-o",
                "NumberOfPasswordPrompts=1",
                "-o",
                "KbdInteractiveAuthentication=no",
                "-o",
                "ConnectTimeout=2",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-i",
                "/tmp/id_ed25519",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "PreferredAuthentications=publickey",
                "-o",
                "PubkeyAuthentication=yes",
                "-o",
                "PasswordAuthentication=no",
                "root@example.test",
                "unlock-luks unlock",
            ]
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
            "-p",
            "2222",
        ])
        .expect("default CLI arguments should parse");

        assert_eq!(args.attempt_timeout_seconds, 30);
        assert_eq!(args.port, 2222);
        assert_eq!(args.user, "root");
        assert_eq!(args.command, "unlock-luks unlock");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reports_open_port() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("failed to bind test listener");
        let address = listener.local_addr().expect("failed to get test address");

        assert!(port_is_open(&[address]).await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reports_closed_port() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("failed to bind test listener");
        let address = listener.local_addr().expect("failed to get test address");
        drop(listener);

        assert!(!port_is_open(&[address]).await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminates_an_ssh_attempt_that_exceeds_its_timeout() {
        let script_path = env::temp_dir().join(format!(
            "remote-luks-unlocker-test-{}-{}",
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
    async fn completes_an_ssh_attempt_when_the_remote_command_succeeds() {
        let script_path = env::temp_dir().join(format!(
            "remote-luks-unlocker-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before the Unix epoch")
                .as_nanos()
        ));
        fs::write(
            &script_path,
            "#!/bin/sh\nread password\n[ \"$password\" = secret ]\n",
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
            "remote-luks-unlocker-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before the Unix epoch")
                .as_nanos()
        ));
        fs::write(
            &script_path,
            "#!/bin/sh\necho 'connection refused' >&2\nexit 255\n",
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
