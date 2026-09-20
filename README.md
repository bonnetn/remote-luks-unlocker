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

Every option can be supplied through an environment variable using the
`REMOTE_LUKS_*` names shown by `--help`; for example:

```sh
export REMOTE_LUKS_HOST=127.0.0.1
export REMOTE_LUKS_PORT=2222
export REMOTE_LUKS_USER=root
export REMOTE_LUKS_PASSWORD='test-passphrase'
export REMOTE_LUKS_IDENTITY_FILE="$HOME/.ssh/id_ed25519"
export REMOTE_LUKS_KNOWN_HOSTS="$HOME/.ssh/known_hosts.remote-luks"
export REMOTE_LUKS_COMMAND='unlock-luks unlock'
cargo run
```

Explicit CLI arguments take precedence over environment variables. The
identity-file value is the private key; its matching `.pub` key must be
authorized on the remote Dropbear server. The password is provided to OpenSSH
through its askpass mechanism and to the remote command on standard input; it
is not added to the SSH argument list. When `REMOTE_LUKS_KNOWN_HOSTS` is set,
strict host-key checking is enabled and the file must contain the expected
OpenSSH `known_hosts` entry. Without it, host-key checking is disabled for the
local test fixture.

## Run tests

`cargo test` includes `tests/dropbear.rs`. That integration test builds and
starts the fixture, runs the polling client against it, sends the unlock
password to the remote command, verifies the dummy state transition, and cleans
up the container and generated state afterward.

## Run the fixture

Requirements: Podman and `ssh-keygen`. The default fixture is deliberately
rootless and uses a dummy unlock state instead of device-mapper/LUKS.

```sh
make test-server
make test-connection
make stop-test-server
```

The fixture listens on `127.0.0.1:2222`. It starts Dropbear while a dummy
unlock state is `LOCKED`, then the smoke test changes it to `UNLOCKED` through
SSH. Generated keys and test state are stored in the fixture directory and are
ignored by git. The default passphrase is `test-passphrase`; set
`LUKS_PASSPHRASE` consistently for both `make test-server` and
`make test-connection` to change it.

This fixture is for local integration tests only and is not a production SSH
or disk-unlock configuration. A real LUKS/device-mapper fixture can be added
as a separate privileged test when needed.
