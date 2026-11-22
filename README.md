# KetteVM

[![CI](https://github.com/TriedAngle/KetteVM/actions/workflows/ci.yml/badge.svg?branch=master&event=push)](https://github.com/TriedAngle/KetteVM/actions/workflows/ci.yml)
[![Extended Tests](https://github.com/TriedAngle/KetteVM/actions/workflows/extended-master.yml/badge.svg?branch=master&event=push)](https://github.com/TriedAngle/KetteVM/actions/workflows/extended-master.yml)
[![test262](https://img.shields.io/badge/test262-18309%20passing%20%2833.9%25%20of%2053909%29-yellow)](https://github.com/TriedAngle/KetteVM/actions/workflows/extended-master.yml)

A fast virtual machine for dynamic languages.

Default build

    cargo build --release -p vm

Fast build (nightly toolchain):

    cargo +nightly build --release -p vm --features fast

Run:

    target/release/kettevm script.js

Example Run (Deltablue):

    cargo +nightly run --release --features fast -- benchmarks/octane/base.js benchmarks/octane/deltablue/deltablue.js benchmarks/octane/run.js

UI sketch demo (layout tree and terminal preview):

    cargo run -q -p vm -- kette/std/startup.ktt kette/examples/ui/demo.ktt

The UI checks run with the normal Kette suite:

    python3 kette/kette_tests/check.py kette/kette_tests/ui.ktt

See `kette/examples/README.md` for the terminal modes (`TERMINAL=0/1/2`).

Minor-GC stress (collect on every allocation; much slower, catches GC bugs):

    cargo build --release -p vm --features stress-minor-gc

Combine with the fast interpreter:

    cargo +nightly build --release -p vm --features fast,stress-minor-gc

Kette tgpu particles example (build `tgpu-native` first; needs `slangc` and libglfw):

    cargo run -q -p vm -- kette/examples/tgpu/particles.ktt

JS tgpu particles example (loads the same Kette FFI modules):

    cargo run -q -p vm -- js/examples/particles.js
