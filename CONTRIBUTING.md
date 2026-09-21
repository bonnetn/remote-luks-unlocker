# Development

## Configuration

All CLI options can also be supplied through the corresponding
`REMOTE_LUKS_*` environment variables. Run `cargo run -- --help` for the full
list. The private identity and known-hosts files are required; SSH password
authentication is disabled. The unlock password is sent only to the remote
command on standard input.

The client polls indefinitely for transport failures until the remote command
succeeds, a command outcome is unsafe to retry, or Ctrl+C is pressed. Each SSH
attempt is bounded by `--attempt-timeout-seconds` (or
`REMOTE_LUKS_ATTEMPT_TIMEOUT_SECONDS`). Logging defaults to `info`; use, for
example, `RUST_LOG=remote_luks_unlocker=debug` for more detail.

## Tests

Run the focused and integration tests with:

```sh
cargo test
```

The integration suite under `tests/dropbear.rs` uses the rootless Podman
fixture in `tests/fixtures/dropbear-luks/`. It creates isolated data and
container names, verifies password delivery to the dummy unlock command, tests
public-key and host-key combinations, and cleans up its server and temporary
state.

Requirements are a running Podman engine, `ssh-keyscan`, and `ssh-keygen`. On
macOS or Windows, start the default Podman machine before running the tests.
The fixture is deliberately a dummy unlock-state server rather than a
privileged LUKS/device-mapper setup.

## Container image

Build and smoke-test the distroless image with:

```sh
podman build --file Containerfile --tag remote-luks-unlocker:local .
podman run --rm remote-luks-unlocker:local --help
```

To operate the fixture manually:

```sh
make test-server
make test-connection
make stop-test-server
```

The default fixture passphrase is `test-passphrase`; set `LUKS_PASSPHRASE`
consistently for `make test-server` and `make test-connection` to change it.

## Implementation notes

The client uses the system OpenSSH binary through Tokio’s current-thread
runtime. SSH arguments are constructed once and reused for retries. The SSH
child is configured with `kill_on_drop(true)`, and timed-out attempts are
explicitly killed and reaped. Askpass is intentionally not used: the unlock
password belongs to the remote command, while SSH transport authentication is
always public-key based.
