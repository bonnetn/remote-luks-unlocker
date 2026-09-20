use std::{
    env,
    fs::Permissions,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    net::{TcpStream, lookup_host},
    process::Command,
    time::{sleep, timeout},
};

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Poll an SSH endpoint and authenticate with a password"
)]
struct Args {
    /// Hostname or IP address of the machine running Dropbear/OpenSSH.
    #[arg(long, env = "REMOTE_LUKS_HOST")]
    host: String,

    /// SSH port.
    #[arg(short = 'P', long, env = "REMOTE_LUKS_PORT", default_value_t = 22)]
    port: u16,

    /// SSH username.
    #[arg(short, long, env = "REMOTE_LUKS_USER")]
    user: String,

    /// SSH password. REMOTE_LUKS_PASSWORD can be used instead.
    #[arg(short, long, env = "REMOTE_LUKS_PASSWORD", hide_env_values = true)]
    password: String,

    /// Private SSH identity file whose public key is authorized on the server.
    #[arg(short = 'i', long, env = "REMOTE_LUKS_IDENTITY_FILE")]
    identity_file: Option<PathBuf>,

    /// known_hosts file used to verify the remote host key.
    #[arg(long, env = "REMOTE_LUKS_KNOWN_HOSTS")]
    known_hosts: Option<PathBuf>,

    /// Maximum time to wait for the SSH port and a successful login.
    #[arg(
        long,
        env = "REMOTE_LUKS_WAIT_SECONDS",
        default_value_t = 60,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    wait_seconds: u64,

    /// Seconds between port checks and login attempts.
    #[arg(
        long,
        env = "REMOTE_LUKS_INTERVAL_SECONDS",
        default_value_t = 1,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    interval_seconds: u64,

    /// Remote command to run after authentication.
    #[arg(long, env = "REMOTE_LUKS_COMMAND", default_value = "true")]
    command: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let ssh = find_ssh()
        .await
        .context("OpenSSH is required but the `ssh` binary was not found")?;
    let endpoint = resolve_endpoint(&args.host, args.port).await?;
    let askpass = Askpass::new(&args.password).await?;

    let result = poll_until_connected(&args, &ssh, &endpoint, &askpass).await;
    let cleanup_result = askpass.cleanup().await;
    result.and(cleanup_result)
}

async fn find_ssh() -> Result<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    for directory in env::split_paths(&path) {
        let candidate = directory.join("ssh");
        if let Ok(metadata) = fs::metadata(&candidate).await {
            if metadata.permissions().mode() & 0o111 != 0 {
                return Ok(candidate);
            }
        }
    }

    bail!("ssh binary is not installed or is not executable")
}

async fn resolve_endpoint(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    lookup_host((host, port))
        .await
        .with_context(|| format!("could not resolve {host}:{port}"))
        .map(|addresses| addresses.collect())
}

async fn poll_until_connected(
    args: &Args,
    ssh: &Path,
    endpoint: &[SocketAddr],
    askpass: &Askpass,
) -> Result<()> {
    let timeout_duration = Duration::from_secs(args.wait_seconds);
    let interval = Duration::from_secs(args.interval_seconds);
    let deadline = Instant::now() + timeout_duration;
    let target = format!("{}@{}", args.user, args.host);
    let mut last_error = None;

    loop {
        if Instant::now() >= deadline {
            break;
        }

        if port_is_open(endpoint).await {
            match connect_and_run(
                ssh,
                &target,
                args.port,
                &args.command,
                args.identity_file.as_deref(),
                args.known_hosts.as_deref(),
                askpass,
            )
            .await
            {
                Ok(()) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        sleep(interval.min(remaining)).await;
    }

    match last_error {
        Some(error) => Err(error).context("SSH did not become usable before the timeout"),
        None => bail!("SSH port {} did not open before the timeout", args.port),
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
    target: &str,
    port: u16,
    command: &str,
    identity_file: Option<&Path>,
    known_hosts: Option<&Path>,
    askpass: &Askpass,
) -> Result<()> {
    let mut arguments = vec![
        "-p".to_owned(),
        port.to_string(),
        "-o".to_owned(),
        "BatchMode=no".to_owned(),
        "-o".to_owned(),
        "NumberOfPasswordPrompts=1".to_owned(),
        "-o".to_owned(),
        "KbdInteractiveAuthentication=no".to_owned(),
        "-o".to_owned(),
        "ConnectTimeout=2".to_owned(),
    ];
    if let Some(known_hosts) = known_hosts {
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
    if let Some(identity_file) = identity_file {
        arguments.extend([
            "-i".to_owned(),
            identity_file.to_string_lossy().into_owned(),
            "-o".to_owned(),
            "IdentitiesOnly=yes".to_owned(),
            "-o".to_owned(),
            "PreferredAuthentications=publickey,password".to_owned(),
            "-o".to_owned(),
            "PubkeyAuthentication=yes".to_owned(),
        ]);
    } else {
        arguments.extend([
            "-o".to_owned(),
            "PreferredAuthentications=password".to_owned(),
            "-o".to_owned(),
            "PubkeyAuthentication=no".to_owned(),
        ]);
    }
    arguments.extend([target.to_owned(), command.to_owned()]);

    let mut child = Command::new(ssh)
        .args(arguments)
        .env("SSH_ASKPASS", &askpass.path)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("REMOTE_LUKS_PASSWORD", &askpass.password)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to execute {}", ssh.display()))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(askpass.password.as_bytes())
            .await
            .context("failed to send the password to the SSH command")?;
        stdin.write_all(b"\n").await?;
    }
    let status = child
        .wait()
        .await
        .with_context(|| format!("failed waiting for {}", ssh.display()))?;

    if status.success() {
        Ok(())
    } else {
        bail!("ssh exited with status {status}")
    }
}

struct Askpass {
    path: PathBuf,
    password: String,
}

impl Askpass {
    async fn new(password: &str) -> Result<Self> {
        let path = env::temp_dir().join(format!(
            "remote-luks-unlocker-askpass-{}",
            std::process::id()
        ));

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .with_context(|| format!("could not create askpass helper {}", path.display()))?;
        file.write_all(b"#!/bin/sh\nprintf '%s\\n' \"$REMOTE_LUKS_PASSWORD\"\n")
            .await?;
        fs::set_permissions(&path, Permissions::from_mode(0o700)).await?;

        Ok(Self {
            path,
            password: password.to_owned(),
        })
    }

    async fn cleanup(self) -> Result<()> {
        fs::remove_file(&self.path).await.ok();
        Ok(())
    }
}
