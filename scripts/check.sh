#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 0 ]]; then
    echo "Usage: scripts/check.sh" >&2
    exit 2
fi
cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features --no-fail-fast
