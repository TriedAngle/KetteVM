# KetteVM

[![CI](https://github.com/TriedAngle/KetteVM/actions/workflows/ci.yml/badge.svg?branch=master&event=push)](https://github.com/TriedAngle/KetteVM/actions/workflows/ci.yml)
[![test262 match](https://img.shields.io/badge/test262%20match-11745%20passing-brightgreen)](https://github.com/TriedAngle/KetteVM/actions/workflows/extended-master.yml)
[![test262 become](https://img.shields.io/badge/test262%20become-11828%20passing-brightgreen)](https://github.com/TriedAngle/KetteVM/actions/workflows/extended-master.yml)

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

CI runs on the project's self-hosted runner. The `CI` workflow runs `format`,
`build`, and `test` on every PR and on `master`. Extended checks
(`test262-match`, `test262-become`, `gc-stress-match`, `gc-stress-become`)
run automatically on `master` and can be triggered for a PR with the
**Extended checks (PR)** workflow; they are required for merging. See
[.github/CI.md](.github/CI.md) for the runner setup, test262 baseline, and
local equivalents:

    ./ci/github/ci.sh              # format, build, and tests
    CI_TEST262=1 ./ci/github/ci.sh # also run test262 for both engines
