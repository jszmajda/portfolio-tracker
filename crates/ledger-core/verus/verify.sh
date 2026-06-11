#!/usr/bin/env bash
# Deductively verify the ledger-core accounting kernel with Verus.
#
# Runs `cargo verus verify` on crates/ledger-core — the crate whose `verus!{}`
# module IS the executable arithmetic kernel (residual-sweep basis consumption,
# largest-remainder allocation, MONEY_CAP-bounded i128 math). Under plain stable
# `cargo build` the `verus!{}` macro erases and the same source compiles as
# ordinary Rust; under this script's pinned Verus toolchain it is deductively
# verified.
#
# Dual-build wiring mirrors a prior private verified-Rust project: vstd from crates.io, the
# [package.metadata.verus] verify=true marker, the unexpected_cfgs allow for
# cfg(verus_only)/cfg(kani), and NO rust-toolchain.toml.
#
# The Verus toolchain (tools/verus-arm64-macos) is .gitignored and provisioned
# per-machine: a symlink to a vendored copy, or the pinned GitHub release
# verus-lang/verus 0.2026.05.13.fae8859 (arm64-macos; runs on rustup 1.95.0) —
# the build matching the crates' vstd =0.0.0-2026-05-10-0145 pin. CI fetches it
# in .github/workflows/ci.yml; bump the release and the vstd pin together.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
VERUS_TOOLS="${VERUS_TOOLS:-$REPO_ROOT/tools/verus-arm64-macos}"
CORE_CRATE="$REPO_ROOT/crates/ledger-core"

export PATH="$VERUS_TOOLS:$PATH"

cd "$CORE_CRATE"
# Touch so a cached build still re-emits the verification results line.
touch src/lib.rs
exec cargo verus verify
