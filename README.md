# remote-luks-unlocker

Poll an SSH server and run the unlock command:

```sh
cargo run -- \
  --host 127.0.0.1 \
  --port 2222 \
  --user root \
  --identity-file "$HOME/.ssh/id_ed25519" \
  --luks-password "$REMOTE_LUKS_LUKS_PASSWORD" \
  --command "unlock-luks unlock"
```
