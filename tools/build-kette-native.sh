#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

cargo build -p kette_native --release

case "$(uname -s)" in
    Darwin)
        name=libkette.dylib
        dir="${KETTE_LIB_DIR:-$HOME/lib}"
        ;;
    Linux)
        name=libkette.so
        dir="${KETTE_LIB_DIR:-$HOME/lib}"
        ;;
    MINGW* | MSYS* | CYGWIN*)
        name=kette.dll
        dir="${KETTE_LIB_DIR:-$HOME/lib}"
        ;;
    *)
        echo "unsupported platform" >&2
        exit 1
        ;;
esac

mkdir -p "$dir"
rm -f "$dir/$name"
cp "target/release/$name" "$dir/$name"
echo "installed $dir/$name"
