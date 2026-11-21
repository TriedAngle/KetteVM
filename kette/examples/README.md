# Kette examples

Run any example with the release VM:

    cargo run -q -p vm -- <files...>

Consecutive files of the same language run as one isolated file unit: they
share top-level bindings with each other, but not with `globalThis` or later
units. Pass multi-file examples in order.

## Standard startup

`kette/std/startup.ktt` installs the array-side `size()` and `iterator()`
methods used by `for x in array` and array destructuring, plus the `Modules`
namespace and `ModuleRegistry` manager. It also loads `std/sync.ktt` and
`std/error.ktt`, and installs sync's `RecursiveMutex` as the module lock. It
is not loaded automatically; pass it first when a script uses those:

    cargo run -q -p vm -- kette/std/startup.ktt <your-files...>

Scripts that do not iterate arrays or use modules do not need it.

## UI sketch

    cargo run -q -p vm -- \
        kette/std/ui/sketch.ktt \
        kette/std/ui/verify.ktt \
        kette/std/ui/tests.ktt \
        kette/examples/ui/demo.ktt

The demo prints the layout tree, a terminal preview of the paint list, and then
runs the layout/paint/invalidation checks.

Switches in `kette/examples/ui/demo.ktt`:

- `TERMINAL`: `0` off, `1` ASCII, `2` ANSI truecolor.
- `SHOW_OPS`: `1` dumps the flat paint list under the tree.

The GLFW window demo draws the same scene with tgpu:

    cargo run -q -p vm -- kette/std/ui/sketch.ktt kette/examples/ui/window.ktt

It needs the `tgpu-native` build, a Vulkan loader (MoltenVK on macOS), `slangc`,
and GLFW. It renders the flat `OP_RECT` list as instanced quads. `tgpu` falls back
to `$VULKAN_SDK/lib/libvulkan.dylib` when the loader is not on `DYLD_*PATH`.

## Other examples

Quick standalone scripts:

    cargo run -q -p vm -- kette/examples/qsort.ktt
    cargo run -q -p vm -- kette/examples/threads.ktt
    cargo run -q -p vm -- kette/std/startup.ktt \
        kette/examples/user_scheduler.ktt

The evolving module example covers base modules, extensions, ownership,
file units, and lazy dependency flush:

    cargo run -q -p vm -- kette/std/startup.ktt \
        kette/examples/modules_live.ktt

Window examples need the system libraries (GLFW, raylib):

    cargo run -q -p vm -- kette/examples/glfw/window_demo.ktt
    cargo run -q -p vm -- kette/examples/raylib/window_demo.ktt

The tgpu examples need `tgpu-native` built first, plus `slangc` and libglfw:

    cargo run -q -p vm -- kette/examples/tgpu/particles.ktt
