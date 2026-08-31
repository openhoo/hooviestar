#!/usr/bin/env bash
set -euo pipefail

readonly SCRIPT_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
readonly QUALIFICATION_ROOT=${XDG_DATA_HOME:-"$HOME/.local/share"}/hooviestar-windows-discord-qualification
readonly MIROTALK_ROOT=$QUALIFICATION_ROOT/mirotalkbro

if [[ ! -f "$MIROTALK_ROOT/package.json" ]]; then
    printf 'Prepare MiroTalk first: %s\n' "$MIROTALK_ROOT" >&2
    exit 1
fi

export NODE_OPTIONS="--require=$SCRIPT_ROOT/force-node-loopback.cjs${NODE_OPTIONS:+ $NODE_OPTIONS}"
exec npm --prefix "$MIROTALK_ROOT" start
