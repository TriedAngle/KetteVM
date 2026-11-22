# Kette examples

Run any example with the release VM:

    cargo run -q -p vm -- <files...>

Consecutive files of the same language run as one isolated batch: they share
top-level bindings with each other, but not with `globalThis` or later
batches. Pass multi-file examples in order.

## Standard startup

`kette/std/startup.ktt` installs the array-side `size()` and `iterator()`
methods used by `for x in array` and array destructuring, plus the `Modules`
namespace and `ModuleRegistry` manager. It also loads `std/sync.ktt`,
`std/error.ktt`, `std/ffi.ktt`, `std/os.ktt`, `std/ui/ui.ktt`, and
`std/ui/verify.ktt`, and installs sync's `RecursiveMutex` as the module lock.
It is not loaded automatically; pass it first when a script uses those:

    cargo run -q -p vm -- kette/std/startup.ktt <your-files...>

`ffi` and `os` are modules, so use `Modules.ffi` and `Modules.os`; there are
no global `FFI` or `os` objects. Files are only evaluated when passed on the
command line or loaded explicitly with `KetteTools.evalFile`.
Scripts that do not iterate arrays or use modules do not need startup.

Vendor bindings are explicit modules and are not loaded by startup:

```text
kette/vendor/sys.ktt   -> sys
kette/vendor/tgpu.ktt  -> tgpu  (load after sys)
kette/vendor/glfw.ktt  -> glfw  (load after sys and tgpu)
```

## UI sketch

    cargo run -q -p vm -- kette/std/startup.ktt kette/examples/ui/demo.ktt

The demo prints the layout tree and a terminal preview of the paint list. The
layout/paint/invalidation checks live in `kette/kette_tests/ui.ktt` and run
with the normal Kette suite.

Switches in `kette/examples/ui/demo.ktt`:

- `TERMINAL`: `0` off, `1` ASCII, `2` ANSI truecolor.
- `SHOW_OPS`: `1` dumps the flat paint list under the tree.

The GLFW window demo draws the same scene with tgpu:

    cargo run -q -p vm -- kette/examples/ui/window.ktt

It needs the `tgpu-native` build, a Vulkan loader (MoltenVK on macOS), `slangc`,
and GLFW. It renders the flat `OP_RECT` list as instanced quads. `tgpu` falls back
to `$VULKAN_SDK/lib/libvulkan.dylib` when the loader is not on `DYLD_*PATH`.

## Other examples

Quick standalone scripts:

    cargo run -q -p vm -- kette/std/startup.ktt kette/examples/qsort.ktt
    cargo run -q -p vm -- kette/examples/threads.ktt
    cargo run -q -p vm -- kette/std/startup.ktt \
        kette/examples/user_scheduler.ktt

Modules use `mod name { export ... }`, `mod name extend part { ... }`,
`use module`, and `use module { name, nested { other } }`. The module example
covers base modules, nested modules, extensions, ownership, and explicit file
loading/reload:

    cargo run -q -p vm -- kette/std/startup.ktt \
        kette/examples/modules_live.ktt

Window examples need the system libraries (GLFW, raylib):

    cargo run -q -p vm -- kette/std/startup.ktt kette/examples/glfw/window_demo.ktt
    cargo run -q -p vm -- kette/std/startup.ktt kette/examples/raylib/window_demo.ktt

The tgpu examples need `tgpu-native` built first, plus `slangc` and libglfw:

    cargo run -q -p vm -- kette/examples/tgpu/particles.ktt
