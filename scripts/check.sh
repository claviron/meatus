#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 ]]; then
    echo "Usage: scripts/check.sh QUINCY_SOURCE" >&2
    exit 2
fi
quincy_source=$(cd "$1" && pwd -P)
cd "$(dirname "$0")/.."

patches=(
    --config "patch.crates-io.quincy.path=\"$quincy_source/quincy\""
    --config "patch.crates-io.quincy-server.path=\"$quincy_source/quincy-server\""
)

cargo fmt --check
cargo clippy "${patches[@]}" --workspace --all-features --all-targets -- -D warnings
cargo test "${patches[@]}" --workspace --all-features --no-fail-fast
