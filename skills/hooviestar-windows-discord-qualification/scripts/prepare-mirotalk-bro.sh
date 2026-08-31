#!/usr/bin/env bash
set -euo pipefail

readonly MIROTALK_COMMIT=d932edddf9cf04ac96a305af753ef27a021630db
readonly MIROTALK_VERSION=1.3.77
readonly MIROTALK_REMOTE=https://github.com/miroslavpejic85/mirotalkbro.git
readonly QUALIFICATION_ROOT=${XDG_DATA_HOME:-"$HOME/.local/share"}/hooviestar-windows-discord-qualification
readonly MIROTALK_ROOT=$QUALIFICATION_ROOT/mirotalkbro

mkdir -p "$QUALIFICATION_ROOT"
if [[ ! -d "$MIROTALK_ROOT/.git" ]]; then
    git clone "$MIROTALK_REMOTE" "$MIROTALK_ROOT"
fi
if [[ -n $(git -C "$MIROTALK_ROOT" status --porcelain) ]]; then
    printf 'Refusing to replace dirty MiroTalk checkout: %s\n' "$MIROTALK_ROOT" >&2
    exit 1
fi

git -C "$MIROTALK_ROOT" fetch --depth 1 origin "$MIROTALK_COMMIT"
git -C "$MIROTALK_ROOT" switch --detach "$MIROTALK_COMMIT"
actual_version=$(node -p "require('$MIROTALK_ROOT/package.json').version")
if [[ "$actual_version" != "$MIROTALK_VERSION" ]]; then
    printf 'Unexpected MiroTalk version: %s\n' "$actual_version" >&2
    exit 1
fi

printf '%s\n' \
    'NODE_ENV=production' \
    'HOST=http://127.0.0.1:3016' \
    'PORT=3016' \
    'BROADCASTING=p2p' \
    'LOGS_DEBUG=false' \
    'LOGS_JSON=false' \
    'TRUST_PROXY=false' \
    "CORS_ORIGIN='*'" \
    "CORS_METHODS='[\"GET\", \"POST\"]'" \
    'OIDC_ENABLED=false' \
    'OIDC_AUTH_REQUIRED=false' \
    'STUN_SERVER_ENABLED=false' \
    'TURN_SERVER_ENABLED=false' \
    'NGROK_ENABLED=false' \
    'SENTRY_ENABLED=false' >"$MIROTALK_ROOT/.env"

npm --prefix "$MIROTALK_ROOT" ci
printf '%s\n' "$MIROTALK_ROOT"
