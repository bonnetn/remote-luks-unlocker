#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
data_dir=${DROPBEAR_DATA_DIR:-$repo_dir/.test-data}
ssh_dir="$data_dir/ssh"
state_dir="$data_dir/state"
container_name=${DROPBEAR_CONTAINER_NAME:-remote-luks-dropbear}
requested_port=${DROPBEAR_PORT:-2222}
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
    -e "LUKS_PASSPHRASE=${LUKS_PASSPHRASE:-test-passphrase}" \
    -p "$requested_port:22" \
    -v "$data_dir:/run/test:ro" \
    -v "$state_dir:/var/lib/luks-test" \
    "$image_name"

for _ in $(seq 1 10); do
    if podman ps --quiet --filter "name=^${container_name}$" | grep -q .; then
        port="$requested_port"
        if [ "$requested_port" = 0 ]; then
            for _ in $(seq 1 10); do
                port=$(podman port "$container_name" 22/tcp 2>/dev/null \
                    | sed -n 's/.*://p' | head -n 1)
                [ -n "$port" ] && break
                sleep 1
            done
        fi
        if [ -z "${port:-}" ]; then
            echo "Could not determine the published Dropbear port" >&2
            podman logs "$container_name" >&2 || true
            exit 1
        fi
        printf '%s\n' "$port" >"$data_dir/port"
        echo "Dropbear test server started on localhost:$port"
        echo "private key: $ssh_dir/id_ed25519"
        exit 0
    fi
    sleep 1
done

echo "Dropbear test server exited during startup:" >&2
podman logs "$container_name" >&2 || true
exit 1
