#!/bin/sh
set -eu
podman rm -f remote-luks-dropbear >/dev/null 2>&1 || true
echo "Dropbear test server stopped"
