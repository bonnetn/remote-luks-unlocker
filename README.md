# remote-luks-unlocker

[![Crates.io](https://img.shields.io/crates/v/remote-luks-unlocker.svg)](https://crates.io/crates/remote-luks-unlocker)

Poll an SSH server and run the unlock command:

```sh
cargo install remote-luks-unlocker

remote-luks-unlocker \
  --host 127.0.0.1 \
  --port 2222 \
  --user root \
  --identity-file "$HOME/.ssh/id_ed25519" \
  --luks-password "$REMOTE_LUKS_LUKS_PASSWORD"
```


Or (with `docker`/`podman`)

```sh
docker run --rm \
  --user "$(id -u):$(id -g)" \
  --volume "$HOME/.ssh/id_ed25519:/run/ssh/id_ed25519:ro" \
  --env REMOTE_LUKS_HOST=127.0.0.1 \
  --env REMOTE_LUKS_PORT=2222 \
  --env REMOTE_LUKS_USER=root \
  --env REMOTE_LUKS_IDENTITY_FILE=/run/ssh/id_ed25519 \
  --env REMOTE_LUKS_LUKS_PASSWORD \
  ghcr.io/bonnetn/remote-luks-unlocker
```
