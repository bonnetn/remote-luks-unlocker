#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
key="$repo_dir/.test-data/ssh/id_ed25519"
ssh_opts="-i $key -p 2222 -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=2"

status=''
for _ in $(seq 1 30); do
    status=$(ssh $ssh_opts root@127.0.0.1 unlock-luks status 2>/dev/null || true)
    [ "$status" = LOCKED ] && break
    sleep 1
done

[ "$status" = LOCKED ] || {
    echo "Dropbear did not become ready with a locked LUKS volume" >&2
    exit 1
}

printf '%s\n' "${LUKS_PASSPHRASE:-test-passphrase}" \
    | ssh $ssh_opts root@127.0.0.1 unlock-luks unlock

status=$(ssh $ssh_opts root@127.0.0.1 unlock-luks status)
[ "$status" = UNLOCKED ] || {
    echo "LUKS volume did not unlock" >&2
    exit 1
}

echo "SSH unlock smoke test passed"
