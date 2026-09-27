use std::{path::PathBuf, time::Duration};

use clap::Parser;

/// Parse a duration written as a positive integer followed by `s`, `m`, `h`, or `d`.
///
/// # Errors
///
/// Returns an error when the value has an unsupported suffix, is not a positive
/// integer, overflows, or evaluates to zero.
pub fn parse_duration(value: &str) -> Result<Duration, String> {
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
#[allow(clippy::struct_excessive_bools)]
#[command(
    author,
    version,
    about = "Poll an SSH endpoint and run a LUKS unlock command"
)]
pub struct Args {
    /// SSH destination in `[user@]host` form.
    #[arg(env = "REMOTE_LUKS_DESTINATION")]
    pub destination: String,

    /// SSH port.
    #[arg(short = 'p', env = "REMOTE_LUKS_PORT", default_value_t = 22)]
    pub port: u16,

    /// Passphrase sent to the remote unlock command.
    #[arg(long, env = "REMOTE_LUKS_PASSWORD", hide_env_values = true)]
    pub luks_password: String,

    /// Private SSH identity file whose public key is authorized on the server.
    #[arg(short = 'i', env = "REMOTE_LUKS_IDENTITY_FILE")]
    pub identity_file: Option<PathBuf>,

    /// Force IPv4 address resolution.
    #[arg(short = '4', env = "REMOTE_LUKS_IPV4", conflicts_with = "ipv6")]
    pub ipv4: bool,

    /// Force IPv6 address resolution.
    #[arg(short = '6', env = "REMOTE_LUKS_IPV6", conflicts_with = "ipv4")]
    pub ipv6: bool,

    /// Bind to a network interface before connecting.
    #[arg(
        short = 'B',
        env = "REMOTE_LUKS_BIND_INTERFACE",
        value_name = "bind_interface"
    )]
    pub bind_interface: Option<String>,

    /// Bind to a local source address before connecting.
    #[arg(
        short = 'b',
        env = "REMOTE_LUKS_BIND_ADDRESS",
        value_name = "bind_address"
    )]
    pub bind_address: Option<String>,

    /// Request compression.
    #[arg(short = 'C', env = "REMOTE_LUKS_COMPRESSION")]
    pub compression: bool,

    /// Select the cipher specification.
    #[arg(
        short = 'c',
        env = "REMOTE_LUKS_CIPHER_SPEC",
        value_name = "cipher_spec"
    )]
    pub cipher_spec: Option<String>,

    /// Select an alternative SSH configuration file.
    #[arg(
        short = 'F',
        env = "REMOTE_LUKS_CONFIG_FILE",
        value_name = "configfile"
    )]
    pub config_file: Option<String>,

    /// Select a PKCS#11 provider.
    #[arg(
        short = 'I',
        env = "REMOTE_LUKS_PKCS11_PROVIDER",
        value_name = "pkcs11"
    )]
    pub pkcs11_provider: Option<String>,

    /// Connect through a jump host.
    #[arg(short = 'J', env = "REMOTE_LUKS_JUMP_HOST", value_name = "destination")]
    pub jump_host: Option<String>,

    /// Select the MAC algorithms.
    #[arg(short = 'm', env = "REMOTE_LUKS_MAC_SPEC", value_name = "mac_spec")]
    pub mac_spec: Option<String>,

    /// Select an SSH configuration tag.
    #[arg(short = 'P', env = "REMOTE_LUKS_CONFIG_TAG", value_name = "tag")]
    pub config_tag: Option<String>,

    /// Optional `known_hosts` file used to verify the remote host key. When omitted,
    /// OpenSSH uses its normal known-hosts files.
    #[arg(long, env = "REMOTE_LUKS_KNOWN_HOSTS")]
    pub known_hosts: Option<PathBuf>,

    /// Delay after a failed SSH attempt before retrying, such as `1s` or `1m`.
    #[arg(
        long,
        env = "REMOTE_LUKS_FAILURE_INTERVAL",
        default_value = "15s",
        value_parser = parse_duration
    )]
    pub failure_interval: Duration,

    /// Delay between successful remote command runs in continuous mode.
    #[arg(
        long,
        env = "REMOTE_LUKS_SUCCESS_INTERVAL",
        default_value = "1m",
        value_parser = parse_duration
    )]
    pub success_interval: Duration,

    /// Retry until the first successful unlock, then exit.
    #[arg(long, env = "REMOTE_LUKS_ONCE", default_value_t = false)]
    pub once: bool,

    /// Maximum total runtime before exiting with an error, such as `10m`.
    #[arg(
        long,
        env = "REMOTE_LUKS_MAX_RUNTIME",
        value_parser = parse_duration
    )]
    pub max_runtime: Option<Duration>,

    /// Maximum time allowed for one SSH connection and command attempt, such as `30s` or `1m`.
    #[arg(
        long,
        env = "REMOTE_LUKS_ATTEMPT_TIMEOUT",
        default_value = "30s",
        value_parser = parse_duration
    )]
    pub attempt_timeout: Duration,

    /// Maximum time OpenSSH may spend establishing one connection, such as `2s` or `10s`.
    #[arg(
        long,
        env = "REMOTE_LUKS_CONNECT_TIMEOUT",
        default_value = "2s",
        value_parser = parse_duration
    )]
    pub connect_timeout: Duration,

    /// Remote command to run after authentication.
    #[arg(long, env = "REMOTE_LUKS_COMMAND", default_value = "cryptroot-unlock")]
    pub command: String,

    /// Increase OpenSSH diagnostics; repeat for more detail (`-v`, `-vv`, or `-vvv`).
    #[arg(
        short = 'v',
        env = "REMOTE_LUKS_VERBOSE",
        action = clap::ArgAction::Count
    )]
    pub verbose: u8,
}
