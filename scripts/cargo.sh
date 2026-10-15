#!/bin/sh
set -eu
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ -x "$repo_dir/.tools/cargo/bin/cargo" ]; then
    export CARGO_HOME="$repo_dir/.tools/cargo"
    export RUSTUP_HOME="$repo_dir/.tools/rustup"
    export PATH="$CARGO_HOME/bin:$PATH"
fi
exec cargo "$@"

