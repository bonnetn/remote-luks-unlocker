#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
data_dir="$repo_dir/.test-data"
ssh_dir="$data_dir/ssh"
state_dir="$data_dir/state"
container_name=remote-luks-dropbear
image_name=remote-luks-unlocker/dropbear-test:latest

mkdir -p "$ssh_dir" "$state_dir"
if [ ! -f "$ssh_dir/id_ed25519" ]; then
    ssh-keygen -q -t ed25519 -N '' -f "$ssh_dir/id_ed25519"
fi
cp "$ssh_dir/id_ed25519.pub" "$data_dir/authorized_keys"

podman build -t "$image_name" "$repo_dir"
podman rm -f "$container_name" >/dev/null 2>&1 || true
podman run -d \
    --name "$container_name" \
    --privileged \
    -e "LUKS_PASSPHRASE=${LUKS_PASSPHRASE:-test-passphrase}" \
    -p 2222:22 \
    -v "$data_dir:/run/test:ro" \
    -v "$state_dir:/var/lib/luks-test" \
    "$image_name"

for _ in $(seq 1 10); do
    if podman ps --quiet --filter "name=^${container_name}$" | grep -q .; then
        echo "Dropbear test server started on localhost:2222"
        echo "private key: $ssh_dir/id_ed25519"
        exit 0
    fi
    sleep 1
done

echo "Dropbear test server exited during startup:" >&2
podman logs "$container_name" >&2 || true
exit 1
