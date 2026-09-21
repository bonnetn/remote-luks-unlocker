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
docker run --rm ghcr.io/bonnetn/remote-luks-unlocker \
  --host 127.0.0.1 \
  --port 2222 \
  --user root \
  --identity-file "$HOME/.ssh/id_ed25519" \
  --luks-password "$REMOTE_LUKS_LUKS_PASSWORD"
```
