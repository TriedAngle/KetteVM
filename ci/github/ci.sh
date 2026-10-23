#!/usr/bin/env bash
# Run the same checks as CI locally.
#
#   ./ci/github/ci.sh              # fast checks (no test262)
#   CI_TEST262=1 ./ci/github/ci.sh # also run test262 in the CI image
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

cargo fmt --all --check
(
    cd interpreter_become
    cargo +nightly fmt --check
)

cargo test --workspace --locked
cargo +nightly test --workspace --locked --features vm/fast
cargo test --workspace --locked --features vm/stress-minor-gc
cargo build --release -p vm --locked
cargo +nightly build --release -p vm --features fast --locked
(
    cd interpreter_become
    cargo +nightly test --release --locked
)

# Clippy is intentionally disabled until the current errors/warnings are
# cleaned up:
# cargo clippy --workspace --all-targets -- -D warnings

if [[ "${CI_TEST262:-0}" == 1 ]]; then
    read -r -a engines <<< "${TEST262_ENGINES:-match become}"
    for engine in "${engines[@]}"; do
        docker run --rm \
            --user "$(id -u):$(id -g)" \
            -e CARGO_HOME=/tmp/cargo \
            -e HOME=/tmp \
            -e TEST262_DIR=/test262 \
            -e TEST262_ENGINE="$engine" \
            -e TEST262_MIN_PASS="${TEST262_MIN_PASS:-}" \
            -e TEST262_MAX_PANIC="${TEST262_MAX_PANIC:-}" \
            -e KETTEVM_JOBS="${KETTEVM_JOBS:-}" \
            -v "$root:$root" \
            -w "$root" \
            -v kettevm-test262:/test262:ro \
            kettevm-ci:1 \
            ./ci/github/test262.sh
    done
fi
