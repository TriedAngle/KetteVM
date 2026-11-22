# Kette examples

- `qsort.ktt`: sorts an array through libc `qsort` with a Kette comparator callback.

  ```sh
  cargo run -- kette/examples/qsort.ktt
  ```

- `threads.ktt`: compares atomic and racy counters across platform threads.

  ```sh
  cargo run -- kette/examples/threads.ktt
  ```

- `user_scheduler.ktt`: runs a userspace scheduler that mounts virtual threads on a platform thread.

  ```sh
  cargo run -- kette/examples/user_scheduler.ktt
  ```

- `modules_live.ktt`: demonstrates modules, extensions, and hot reload while preserving identity.

  ```sh
  cargo run -- kette/examples/modules_live.ktt
  ```

- `ui/demo.ktt`: builds a layout tree and prints it with a terminal paint preview.

  ```sh
  cargo run -- kette/examples/ui/demo.ktt
  ```

- `ui/window.ktt`: renders the UI paint list in a GLFW window through tgpu.

  ```sh
  cargo run -- kette/examples/ui/window.ktt
  ```

- `tgpu/particles.ktt`: renders 10,000 GPU particles with tgpu.

  ```sh
  cargo run -- kette/examples/tgpu/particles.ktt
  ```

- `glfw/window_demo.ktt`: opens a GLFW + OpenGL window with a guest key callback.

  ```sh
  cargo run -- kette/examples/glfw/window_demo.ktt
  ```

- `raylib/window_demo.ktt`: opens a raylib window and draws a few frames.

  ```sh
  cargo run -- kette/examples/raylib/window_demo.ktt
  ```

- `raylib/portal_window.ktt`: ports raylib's portal-window example with render-to-texture 3D drawing.

  ```sh
  cargo run -- kette/examples/raylib/portal_window.ktt
  ```

`modules/live_math.ktt` and `modules/live_stats.ktt` are support files loaded by `modules_live.ktt`.
