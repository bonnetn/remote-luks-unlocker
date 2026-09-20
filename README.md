# remote-luks-unlocker

This repository contains the Rust client. Its Dropbear/LUKS container is an
integration-test fixture, not the application itself, and lives under
`tests/fixtures/dropbear-luks/`.

## Poll for SSH and authenticate

The client checks that the raw `ssh` binary is installed, polls the target TCP
port, and retries password authentication until the timeout expires:

```sh
cargo run -- \
  --host 127.0.0.1 \
  --port 2222 \
  --user root \
  --password "$REMOTE_LUKS_PASSWORD" \
  --wait-seconds 60 \
  --interval-seconds 1
```

`REMOTE_LUKS_PASSWORD` may be used instead of `--password`. The password is
provided to OpenSSH through its askpass mechanism and is not added to the SSH
argument list.

## Run the fixture

Requirements: Podman, `ssh-keygen`, and a Podman runtime capable of running a
privileged container with loop devices and device-mapper (`dm_mod`). Rootless
Podman on macOS may not expose `/dev/mapper/control`; use rootful Linux Podman
or configure the Podman VM accordingly.

```sh
make test-server
make test-connection
make stop-test-server
```

The fixture listens on `127.0.0.1:2222`. It starts Dropbear while a
file-backed LUKS2 volume is locked, then the smoke test unlocks it through SSH.
Generated keys and the test volume are stored in the fixture directory and are
ignored by git. The default passphrase is `test-passphrase`; set
`LUKS_PASSPHRASE` consistently for both `make test-server` and
`make test-connection` to change it.

The container is intentionally run with `--privileged`: cryptsetup needs
access to loop/device-mapper support. This fixture is for local integration
tests only and is not a production SSH or disk-unlock configuration.
