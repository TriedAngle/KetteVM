# KetteVM

[![CI](https://github.com/TriedAngle/KetteVM/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/TriedAngle/KetteVM/actions/workflows/ci.yml)

A fast virtual machine for dynamic languages.

Default build

    cargo build --release -p vm

Fast build (nightly toolchain):

    cargo +nightly build --release -p vm --features fast

Run:

    target/release/kettevm script.js

Example Run (Deltablue):

    cargo +nightly run --release --features fast -- benchmarks/octane/base.js benchmarks/octane/deltablue/deltablue.js benchmarks/octane/run.js

Minor-GC stress (collect on every allocation; much slower, catches GC bugs):

    cargo build --release -p vm --features stress-minor-gc

Combine with the fast interpreter:

    cargo +nightly build --release -p vm --features fast,stress-minor-gc

## CI

CI runs on the project's self-hosted runner. Regression checks run on every
PR; the full test262 suite runs on every `master` push, can be triggered
manually for a PR, and is required for merging. See
[.github/CI.md](.github/CI.md) for the runner setup,
test262 baseline, and local equivalents:

    ./ci/github/ci.sh              # regression checks
    CI_TEST262=1 ./ci/github/ci.sh # also run test262
