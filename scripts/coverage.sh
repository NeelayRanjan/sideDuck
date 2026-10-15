#!/bin/sh
# Coverage is isolated from the release binaries used by the performance/soak gate.
set -eu
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
runner_version=$(./scripts/cargo.sh llvm-cov --version 2>/dev/null || true)
if [ "$runner_version" != "cargo-llvm-cov 0.6.21" ]; then
    echo 'Install the pinned coverage runner: ./scripts/cargo.sh install cargo-llvm-cov --version 0.6.21 --locked' >&2
    echo 'The Rust 1.90.0 toolchain also requires its llvm-tools-preview component.' >&2
    exit 1
fi
command -v python3 >/dev/null 2>&1 || { echo 'Coverage reporting requires Python 3.' >&2; exit 1; }
export CARGO_LLVM_COV_TARGET_DIR="$repo_dir/target/llvm-cov-target"
mkdir -p target/coverage
for crate in sideduck-tile sideduck-composite sideduck-brush sideduck-input sideduck-doc; do
    ./scripts/cargo.sh llvm-cov -p "$crate" --locked --lcov \
        --output-path "target/coverage/$crate.lcov"
done
python3 scripts/coverage_report.py
