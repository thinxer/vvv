#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/../.."
exec guest/start-session.sh "${APPLE_STATE_DIR:-$PWD/.cache/apple-build/session}"
