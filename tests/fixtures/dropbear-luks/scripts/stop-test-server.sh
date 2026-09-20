#!/bin/sh
set -eu
container_name=${DROPBEAR_CONTAINER_NAME:-remote-luks-dropbear}
podman rm -f "$container_name" >/dev/null 2>&1 || true
echo "Dropbear test server stopped: $container_name"
