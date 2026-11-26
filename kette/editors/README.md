# Kette image editors

The image is the server. `kette/tools/kettels/kettels.ktt` runs inside the Kette VM,
reads LSP on stdin, and writes LSP on stdout. There is no separate static
analysis process and no TextMate grammar: highlighting is supplied as LSP
semantic tokens from the image's parser.

## Build

    cargo build -p vm
    tools/build-kette-native.sh

`kettels.ktt` uses the parser through `libkette`, and loads the repository-root
relative `kette/std/startup.ktt` and `kette/tools/kettefmt.ktt`.

## VS Code

Open `kette/editors/vscode` and run the extension in a development window, or package
it with `vsce package`.

Settings:

- `kette.serverCommand`: VM executable (default `kettevm`).
- `kette.serverScript`: image server script (default `kette/tools/kettels/kettels.ktt`).
- `kette.root`: image root; empty uses the workspace root.

Commands: `Kette: Eval Selection`, `Eval Buffer`, `Load File`, `REPL`,
`Refresh Loaded Files`, `Show Transcript`, `Image Info`, `Restart Image`
(reloads the extension host). `Cmd/Ctrl+Enter` evaluates the selection or
current line. The Kette view lists every file evaluated into the image;
opening the REPL reveals the `Kette` panel in the bottom panel area next to
Problems, Output, and Terminal.

The image is a child process of the editor's LSP client. Reloading the window,
stopping the client, or killing the process ends the image and loses in-memory
state. Restart it and reload the files you need.

## Neovim

Add the plugin to the runtime path and set the VM path if it is not on `$PATH`:

    set runtimepath+=/path/to/kettevm/kette/editors/nvim
    let g:kette_server = '/path/to/kettevm/target/debug/kettevm'

The plugin starts the image on the first `.ktt` buffer. Commands:

    :KetteEval          " current line, or a range in visual mode
    :KetteEvalBuffer
    :KetteLoad
    :KetteTranscript
    :KetteInfo
    :KetteRestart       " stop the image client and start a new image
    :KetteStart         " start the image if it is not running

## Image transport

By default the server uses stdio, because that is what both editors speak.
The reader and writer each run as virtual threads on the thread pool and use
zero-timeout readiness checks before touching the descriptors, so a slow
editor never pins a carrier thread. It can also listen on TCP loopback and
serve multiple image connections:

    kettevm kette/tools/kettels/kettels.ktt -- --tcp 9433

Each connection has its own reader, writer, outbox, and open-document map.
Document changes are full-text updates and never evaluate or reload image
code; only the explicit image requests below mutate the image. Requests from
all connections are serialized on the image dispatcher.

The protocol is LSP plus image requests:

- `kette/eval`: evaluate in the image as a REPL unit.
- `kette/check`: parser diagnostics.
- `kette/format`: format through `kettefmt`.
- `kette/loadFile`: load a file into the image.
- `kette/transcript`: drain captured `print` output.
- `kette/imageInfo`: root, uptime, evaluation counters, image epoch, loaded
  modules, and the files evaluated into the image.

`print` is rebound to the transcript before user code runs, so user output
never corrupts the LSP stream.

Current limits: `kette/check` reports parser diagnostics only; compile errors
surface on `kette/eval`. The TCP listener accepts each connection and keeps
the shared image alive when a client disconnects. There is no TextMate
fallback, so an editor without the image shows plain text.
