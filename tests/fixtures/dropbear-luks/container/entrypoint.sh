#!/bin/sh
set -eu

state_dir=/var/lib/luks-test
volume_image="$state_dir/luks-volume.img"
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

# Create a small file-backed LUKS volume once. It starts locked on every
# container start, which mirrors the point at which initramfs Dropbear waits
# for an unlock command.
if [ ! -f "$volume_image" ]; then
    truncate -s 32M "$volume_image"
    printf '%s' "$passphrase" | cryptsetup luksFormat --batch-mode --type luks2 "$volume_image" -
    printf '%s' "$passphrase" | cryptsetup open "$volume_image" luks_test --key-file -
    mkfs.ext4 -q /dev/mapper/luks_test
    cryptsetup close luks_test
fi

printf '%s\n' "Dropbear is listening; LUKS volume is locked." >&2
exec dropbear -F -E -p 0.0.0.0:22 -r "$host_key"
