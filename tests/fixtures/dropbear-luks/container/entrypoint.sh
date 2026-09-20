#!/bin/sh
set -eu

state_dir=/var/lib/luks-test
state_file="$state_dir/state"
host_key=/etc/dropbear/dropbear_ed25519_host_key
authorized_keys=/run/test/authorized_keys
passphrase=${LUKS_PASSPHRASE:-test-passphrase}

mkdir -p "$state_dir" /etc/dropbear /root/.ssh
chmod 0700 /root/.ssh

if [ ! -f "$host_key" ]; then
    dropbearkey -t ed25519 -f "$host_key" >/dev/null
    chmod 0600 "$host_key"
fi

if [ ! -s "$authorized_keys" ]; then
    echo "missing /run/test/authorized_keys" >&2
    exit 1
fi
cp "$authorized_keys" /root/.ssh/authorized_keys
chmod 0600 /root/.ssh/authorized_keys

# The Rust integration test authenticates through OpenSSH password auth. Keep
# the key-based login too, since it is useful for debugging the fixture.
printf 'root:%s\n' "$passphrase" | chpasswd

if [ ! -f "$state_file" ]; then
    printf '%s\n' LOCKED >"$state_file"
fi

printf '%s\n' "Dropbear is listening; dummy unlock state is $(cat "$state_file")." >&2
exec dropbear -F -E -p 0.0.0.0:22 -r "$host_key"
