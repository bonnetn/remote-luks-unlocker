# Remote LUKS Unlock for Encrypted Linux Root Filesystems over SSH

[![CI](https://github.com/bonnetn/remote-luks-unlocker/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/bonnetn/remote-luks-unlocker/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/bonnetn/remote-luks-unlocker)](https://github.com/bonnetn/remote-luks-unlocker/releases)
[![Crates.io](https://img.shields.io/crates/v/remote-luks-unlocker.svg)](https://crates.io/crates/remote-luks-unlocker)

`remote-luks-unlocker` is a remote LUKS unlock tool for unattended, headless
Linux servers. It connects over SSH to Dropbear or OpenSSH running in the
initramfs and sends the LUKS passphrase to the remote unlock command.

It is designed for Debian or Ubuntu servers with an encrypted root filesystem:
after a reboot, the client can perform a remote root unlock before anyone is
available at the console. It supports continuous retry for long-running
services and bounded retry for cron jobs.

Typical uses include:

- remotely unlocking an encrypted root disk on a headless server;
- recovering a server that is waiting for its LUKS passphrase during boot; and
- automating SSH-based disk unlock through systemd or cron.

The client does not set up LUKS, install Dropbear, or change your bootloader.
The binary's only external runtime dependency is the system OpenSSH `ssh`
command. Set up the server first: [Dropbear initramfs setup](docs/dropbear-initramfs.md).

## TL;DR

Install it:

```sh
cargo install remote-luks-unlocker
```

Then run it with the server address, the SSH key, a verified initramfs
`known_hosts` file, and the LUKS passphrase:

```sh
remote-luks-unlocker \
  --host server.example.com \
  --port 2222 \
  --user root \
  --identity-file "$HOME/.ssh/remote-luks" \
  --known-hosts "$HOME/.config/remote-luks/known_hosts" \
  --command cryptroot-unlock \
  --luks-password "$REMOTE_LUKS_LUKS_PASSWORD"
```

Or run the published container:

```sh
podman run --rm \
  --user "$(id -u):$(id -g)" \
  -v "$HOME/.ssh/remote-luks:/run/ssh/id_ed25519:ro" \
  -v "$HOME/.config/remote-luks/known_hosts:/run/ssh/known_hosts:ro" \
  -e REMOTE_LUKS_HOST=server.example.com \
  -e REMOTE_LUKS_PORT=2222 \
  -e REMOTE_LUKS_USER=root \
  -e REMOTE_LUKS_IDENTITY_FILE=/run/ssh/id_ed25519 \
  -e REMOTE_LUKS_KNOWN_HOSTS=/run/ssh/known_hosts \
  -e REMOTE_LUKS_COMMAND=cryptroot-unlock \
  -e REMOTE_LUKS_LUKS_PASSWORD \
  ghcr.io/bonnetn/remote-luks-unlocker
```

## How to run it

This example assumes the server already has `dropbear-initramfs` configured,
listens on port `2222` during initramfs, and accepts the public key in
`~/.ssh/remote-luks.pub`.

Create a separate `known_hosts` file for the initramfs host key. Get the
fingerprint from the server console or your hosting provider first. Do not
treat `ssh-keyscan` by itself as verification.

```sh
mkdir -p "$HOME/.config/remote-luks"
ssh-keyscan -p 2222 server.example.com \
  > "$HOME/.config/remote-luks/known_hosts"
chmod 600 "$HOME/.config/remote-luks/known_hosts"
ssh-keygen -lf "$HOME/.config/remote-luks/known_hosts"
```

Start the client before, or just after, rebooting the server:

```sh
remote-luks-unlocker \
  --host server.example.com \
  --port 2222 \
  --user root \
  --identity-file "$HOME/.ssh/remote-luks" \
  --known-hosts "$HOME/.config/remote-luks/known_hosts" \
  --command cryptroot-unlock \
  --luks-password "$REMOTE_LUKS_LUKS_PASSWORD"
```

It keeps trying while SSH is unavailable. When SSH comes up, it authenticates,
sends the passphrase to `cryptroot-unlock`, and continues running after a
successful unlock. By default it waits one minute between successful runs.

For a long-running service, use systemd or another supervisor. The service
should start after networking is available, be able to read the identity and
`known_hosts` files, receive the required options or `REMOTE_LUKS_*` variables,
and keep the LUKS passphrase out of the unit file and source control.

For cron, use `--once` with `--max-runtime` to retry until the first successful
unlock, then exit. This example retries for up to ten minutes, returning
success when the unlock completes and a nonzero status if the timeout expires:

```sh
remote-luks-unlocker \
  --host server.example.com \
  --user root \
  --identity-file "$HOME/.ssh/remote-luks" \
  --known-hosts "$HOME/.config/remote-luks/known_hosts" \
  --luks-password "$REMOTE_LUKS_LUKS_PASSWORD" \
  --once \
  --max-runtime 10m
```

The default command is `unlock-luks unlock`. For Debian or Ubuntu
`dropbear-initramfs`, use `--command cryptroot-unlock`.

## Server setup

See the [Dropbear initramfs setup guide](docs/dropbear-initramfs.md) before
trying this on a real machine.

## Security

Keep host-key checking on. Do not use `/dev/null` for `known_hosts`. Do not use
`StrictHostKeyChecking=no`.

Use a key made only for this job. Restrict it in `authorized_keys` to the
unlock command. Disable forwarding and PTY access. Firewall the initramfs SSH
port to a management network.

The client sends the passphrase to the remote command on standard input. It is
not the SSH password. The passphrase is in memory on the client and server.
Do not put it in source control. Protect the environment file if you use the
systemd setup above.

Keep console or provider recovery access. Test a full reboot before relying on
this. LUKS does not protect an unencrypted `/boot`, the initramfs, firmware, or
a running machine.

## Configuration

Every option is also available as an environment variable. Run
`remote-luks-unlocker --help` for the full list.
Duration values use units such as `1s`, `30s`, or `10m`.

| Option | Environment variable | Default | What it does |
| --- | --- | --- | --- |
| `--host` | `REMOTE_LUKS_HOST` | required | Server hostname or IP address |
| `--port` | `REMOTE_LUKS_PORT` | `22` | SSH port |
| `--user` | `REMOTE_LUKS_USER` | required | SSH username |
| `--identity-file` | `REMOTE_LUKS_IDENTITY_FILE` | required | Private SSH key to use |
| `--known-hosts` | `REMOTE_LUKS_KNOWN_HOSTS` | required | File used to verify the server key |
| `--luks-password` | `REMOTE_LUKS_LUKS_PASSWORD` | required | Passphrase sent to the unlock command |
| `--command` | `REMOTE_LUKS_COMMAND` | `unlock-luks unlock` | Remote command to run |
| `--failure-interval` | `REMOTE_LUKS_FAILURE_INTERVAL` | `1s` | Wait after a failed SSH attempt before retrying |
| `--success-interval` | `REMOTE_LUKS_SUCCESS_INTERVAL` | `1m` | Wait after success before running the command again |
| `--once` | `REMOTE_LUKS_ONCE` | not set | Exit after the first successful unlock |
| `--max-runtime` | `REMOTE_LUKS_MAX_RUNTIME` | unlimited | Total time to keep trying before exiting with an error |
| `--attempt-timeout` | `REMOTE_LUKS_ATTEMPT_TIMEOUT` | `30s` | Maximum time allowed for one SSH connection and command |
