#!/bin/sh
set -eu

machine_name=${PODMAN_MACHINE_NAME:-remote-luks-unlocker}

if podman machine inspect "$machine_name" >/dev/null 2>&1; then
    state=$(podman machine inspect --format '{{.State}}' "$machine_name")
    if [ "$state" = running ]; then
        podman machine stop "$machine_name"
    fi
    podman machine set --rootful "$machine_name"
    podman machine start "$machine_name"
else
    podman machine init --rootful --now "$machine_name"
fi

podman machine ssh "$machine_name" 'sudo modprobe dm_mod'

root_connection=$(podman system connection list --format '{{.Name}}' \
    | grep "${machine_name}.*root" \
    | head -n 1 || true)
if [ -z "$root_connection" ]; then
    echo "Could not find the rootful Podman connection for $machine_name" >&2
    podman system connection list >&2
    exit 1
fi

podman system connection default "$root_connection"
echo "Configured rootful Podman machine: $machine_name"
echo "Default connection: $root_connection"
