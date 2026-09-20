use std::{
    env,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
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
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

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
    init_tracing();
    let args = Args::parse();
    info!(host = %args.host, port = args.port, user = %args.user, "starting SSH polling client");
    let ssh = find_ssh()
        .await
        .context("OpenSSH is required but the `ssh` binary was not found")?;
    info!(path = %ssh.display(), "found OpenSSH binary");
    let endpoint = tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("received Ctrl+C while resolving SSH endpoint");
            bail!("interrupted")
        },
        result = resolve_endpoint(&args.host, args.port) => result?,
    };
    debug!(addresses = ?endpoint, "resolved SSH endpoint");
    let askpass = Askpass::new(&args.password).await?;

    let result = poll_until_connected(&args, &ssh, &endpoint, &askpass).await;
    let cleanup_result = askpass.cleanup().await;
    if let Err(error) = &result {
        error!(error = %error, "SSH polling stopped with an error");
    }
    result.and(cleanup_result)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn find_ssh() -> Result<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    for directory in env::split_paths(&path) {
        let candidate = directory.join("ssh");
        if let Ok(metadata) = fs::metadata(&candidate).await {
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                debug!(path = %candidate.display(), "found executable SSH candidate");
                return Ok(candidate);
            }
        }
    }

    bail!("ssh binary is not installed or is not executable")
}

async fn resolve_endpoint(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    debug!(host, port, "resolving SSH endpoint");
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
    let interval = Duration::from_secs(args.interval_seconds);
    let target = format!("{}@{}", args.user, args.host);

    loop {
        let port_open = tokio::select! {
            _ = tokio::signal::ctrl_c() => bail!("interrupted"),
            open = port_is_open(endpoint) => open,
        };
        if port_open {
            debug!(
                port = args.port,
                "SSH port is open; attempting authentication"
            );
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
                Ok(()) => {
                    info!("SSH authentication and remote command succeeded");
                    return Ok(());
                }
                Err(error) => warn!(error = %error, "SSH attempt failed; will retry"),
            }
        } else {
            debug!("SSH port is not open; will retry");
        }

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("received Ctrl+C; stopping SSH polling");
                bail!("interrupted")
            },
            _ = sleep(interval) => {},
        }
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
    debug!(target, command, "starting SSH child process");
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
        if let Err(error) = stdin.write_all(askpass.password.as_bytes()).await {
            child.kill().await.ok();
            return Err(error).context("failed to send the password to the SSH command");
        }
        if let Err(error) = stdin.write_all(b"\n").await {
            child.kill().await.ok();
            return Err(error.into());
        }
    }
    let status = tokio::select! {
        result = child.wait() => result
            .with_context(|| format!("failed waiting for {}", ssh.display()))?,
        _ = tokio::signal::ctrl_c() => {
            info!("received Ctrl+C; terminating SSH child process");
            child.kill().await.ok();
            bail!("interrupted")
        }
    };

    if status.success() {
        Ok(())
    } else {
        debug!(%status, "SSH child process failed");
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
            .mode(0o700)
            .open(&path)
            .await
            .with_context(|| format!("could not create askpass helper {}", path.display()))?;
        file.write_all(b"#!/bin/sh\nprintf '%s\\n' \"$REMOTE_LUKS_PASSWORD\"\n")
            .await?;
        debug!(path = %path.display(), "created private SSH askpass helper");
        Ok(Self {
            path,
            password: password.to_owned(),
        })
    }

    async fn cleanup(self) -> Result<()> {
        fs::remove_file(&self.path).await.ok();
        debug!(path = %self.path.display(), "removed SSH askpass helper");
        Ok(())
    }
}
