use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Poll an SSH endpoint and authenticate with a password"
)]
struct Args {
    /// Hostname or IP address of the machine running Dropbear/OpenSSH.
    #[arg(long)]
    host: String,

    /// SSH port.
    #[arg(short = 'P', long, default_value_t = 22)]
    port: u16,

    /// SSH username.
    #[arg(short, long)]
    user: String,

    /// SSH password. REMOTE_LUKS_PASSWORD can be used instead.
    #[arg(short, long, env = "REMOTE_LUKS_PASSWORD", hide_env_values = true)]
    password: String,

    /// Maximum time to wait for the SSH port and a successful login.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
    wait_seconds: u64,

    /// Seconds between port checks and login attempts.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..))]
    interval_seconds: u64,

    /// Remote command to run after authentication.
    #[arg(long, default_value = "true")]
    command: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let ssh = find_ssh().context("OpenSSH is required but the `ssh` binary was not found")?;
    let endpoint = resolve_endpoint(&args.host, args.port)?;
    let askpass = Askpass::new(&args.password)?;

    poll_until_connected(&args, &ssh, &endpoint, &askpass)
}

fn find_ssh() -> Result<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    for directory in env::split_paths(&path) {
        let candidate = directory.join("ssh");
        if candidate.is_file() {
            let metadata = fs::metadata(&candidate)?;
            if metadata.permissions().mode() & 0o111 != 0 {
                return Ok(candidate);
            }
        }
    }

    bail!("ssh binary is not installed or is not executable")
}

fn resolve_endpoint(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    (host, port)
        .to_socket_addrs()
        .with_context(|| format!("could not resolve {host}:{port}"))
        .map(|addresses| addresses.collect())
}

fn poll_until_connected(
    args: &Args,
    ssh: &PathBuf,
    endpoint: &[SocketAddr],
    askpass: &Askpass,
) -> Result<()> {
    let timeout = Duration::from_secs(args.wait_seconds);
    let interval = Duration::from_secs(args.interval_seconds);
    let deadline = Instant::now() + timeout;
    let target = format!("{}@{}", args.user, args.host);
    let mut last_error = None;

    loop {
        if Instant::now() >= deadline {
            break;
        }

        if port_is_open(endpoint) {
            match connect_and_run(ssh, &target, args.port, &args.command, askpass) {
                Ok(()) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(interval.min(remaining));
    }

    match last_error {
        Some(error) => Err(error).context("SSH did not become usable before the timeout"),
        None => bail!("SSH port {} did not open before the timeout", args.port),
    }
}

fn port_is_open(endpoint: &[SocketAddr]) -> bool {
    endpoint
        .iter()
        .any(|address| TcpStream::connect_timeout(address, Duration::from_secs(1)).is_ok())
}

fn connect_and_run(
    ssh: &PathBuf,
    target: &str,
    port: u16,
    command: &str,
    askpass: &Askpass,
) -> Result<()> {
    let port = port.to_string();
    let mut child = Command::new(ssh)
        .args([
            "-p",
            &port,
            "-o",
            "BatchMode=no",
            "-o",
            "NumberOfPasswordPrompts=1",
            "-o",
            "PreferredAuthentications=password",
            "-o",
            "PubkeyAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "ConnectTimeout=2",
            target,
            command,
        ])
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
            .context("failed to send the password to the SSH command")?;
        stdin.write_all(b"\n")?;
    }
    let status = child
        .wait()
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
    fn new(password: &str) -> Result<Self> {
        let path = env::temp_dir().join(format!(
            "remote-luks-unlocker-askpass-{}",
            std::process::id()
        ));

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("could not create askpass helper {}", path.display()))?;
        file.write_all(b"#!/bin/sh\nprintf '%s\\n' \"$REMOTE_LUKS_PASSWORD\"\n")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;

        Ok(Self {
            path,
            password: password.to_owned(),
        })
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
