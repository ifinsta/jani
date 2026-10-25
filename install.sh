#!/bin/sh
# Source-checkout installer for published local Linux/macOS binaries (Python stdlib only).
set -eu
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec python3 "$SCRIPT_DIR/tools/install_unix.py" --root "$SCRIPT_DIR" "$@"
