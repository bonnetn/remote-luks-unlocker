# remote-luks-unlocker

[![CI](https://github.com/bonnetn/remote-luks-unlocker/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/bonnetn/remote-luks-unlocker/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/remote-luks-unlocker)](https://github.com/bonnetn/remote-luks-unlocker/blob/main/LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/bonnetn/remote-luks-unlocker)](https://github.com/bonnetn/remote-luks-unlocker/releases)
[![Crates.io](https://img.shields.io/crates/v/remote-luks-unlocker.svg)](https://crates.io/crates/remote-luks-unlocker)

A deliberately simple, lightweight unlocker: roughly 400 source lines of code, a 2 MB binary, and minimal CPU and memory use.

Poll an SSH server and run the unlock command:

```sh
cargo install remote-luks-unlocker

remote-luks-unlocker \
  --host 127.0.0.1 \
  --port 2222 \
  --user root \
  --identity-file "$HOME/.ssh/id_ed25519" \
  --known-hosts "$HOME/.ssh/known_hosts" \
  --luks-password "$REMOTE_LUKS_LUKS_PASSWORD"
```


Or (with `docker`/`podman`)

```sh
docker run --rm \
  --user "$(id -u):$(id -g)" \
  --volume "$HOME/.ssh/id_ed25519:/run/ssh/id_ed25519:ro" \
  --volume "$HOME/.ssh/known_hosts:/run/ssh/known_hosts:ro" \
  --env REMOTE_LUKS_HOST=127.0.0.1 \
  --env REMOTE_LUKS_PORT=2222 \
  --env REMOTE_LUKS_USER=root \
  --env REMOTE_LUKS_IDENTITY_FILE=/run/ssh/id_ed25519 \
  --env REMOTE_LUKS_KNOWN_HOSTS=/run/ssh/known_hosts \
  --env REMOTE_LUKS_LUKS_PASSWORD \
  ghcr.io/bonnetn/remote-luks-unlocker
```
