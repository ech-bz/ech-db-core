#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo build -p counter-program --release --target wasm32-unknown-unknown

cargo test --workspace

exec "$ROOT/tests/services.sh" "$@"
