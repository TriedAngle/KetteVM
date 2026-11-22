# Kette examples

- `qsort.ktt` - sorts an array through libc `qsort` with a Kette comparator callback.
  Run: `cargo run -- kette/examples/qsort.ktt`

- `threads.ktt` - compares atomic and racy counters across platform threads.
  Run: `cargo run -- kette/examples/threads.ktt`

- `user_scheduler.ktt` - runs a userspace scheduler that mounts virtual threads on a platform thread.
  Run: `cargo run -- kette/examples/user_scheduler.ktt`

- `modules_live.ktt` - demonstrates modules, extensions, and hot reload while preserving identity.
  Run: `cargo run -- kette/examples/modules_live.ktt`

- `ui/demo.ktt` - builds a layout tree and prints it with a terminal paint preview.
  Run: `cargo run -- kette/examples/ui/demo.ktt`

- `ui/window.ktt` - renders the UI paint list in a GLFW window through tgpu.
  Run: `cargo run -- kette/examples/ui/window.ktt`

- `tgpu/particles.ktt` - renders GPU particles with tgpu.
  Run: `cargo run -- kette/examples/tgpu/particles.ktt`

- `glfw/window_demo.ktt` - opens a GLFW + OpenGL window with a guest key callback.
  Run: `cargo run -- kette/examples/glfw/window_demo.ktt`

- `raylib/window_demo.ktt` - opens a raylib window and draws a few frames.
  Run: `cargo run -- kette/examples/raylib/window_demo.ktt`

- `raylib/portal_window.ktt` - ports raylib's portal-window example with render-to-texture 3D drawing.
  Run: `cargo run -- kette/examples/raylib/portal_window.ktt`

`modules/live_math.ktt` and `modules/live_stats.ktt` are support files loaded by
`modules_live.ktt`.
