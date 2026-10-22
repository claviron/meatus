#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 ]]; then
    echo "Usage: scripts/check.sh QUINCY_SOURCE ORBITIVE_SOURCE" >&2
    exit 2
fi
quincy_source=$(cd "$1" && pwd -P)
orbit_source=$(cd "$2" && pwd -P)
cd "$(dirname "$0")/.."

patches=(
    --config "patch.crates-io.quincy.path=\"$quincy_source/quincy\""
    --config "patch.crates-io.quincy-server.path=\"$quincy_source/quincy-server\""
    --config "patch.crates-io.orbitive.path=\"$orbit_source\""
)
for package in core auth cache lock events counter quota cell arena invoke link metrics pool client-pool stream rustls; do
    patches+=(--config "patch.crates-io.orbit-$package.path=\"$orbit_source/$package\"")
done

cargo fmt --check
cargo clippy "${patches[@]}" --workspace --all-features --all-targets -- -D warnings
cargo test "${patches[@]}" --workspace --all-features --no-fail-fast
