#!/usr/bin/env bash
# Deductively verify the tax arithmetic kernel with Verus.
#
# Runs `cargo verus verify` on crates/tax — the crate whose `verus!{}` module
# `kernel` IS the executable tax arithmetic (T_J bracket stacking, the marginal
# accrual increment, the max(0,·) gain clamp, the effective unrealized rate in
# ppm). Under plain stable `cargo build` the `verus!{}` macro erases and the
# same source compiles as ordinary Rust; under this script's pinned Verus
# toolchain it is deductively verified.
#
# Dual-build wiring mirrors ledger-core: vstd from crates.io, the
# [package.metadata.verus] verify=true marker, the unexpected_cfgs allow for
# cfg(verus_only)/cfg(kani), and NO rust-toolchain.toml.
#
# The Verus toolchain (tools/verus-arm64-macos) is .gitignored and provisioned
# per-machine (here: symlinked to the a vendored Verus toolchain).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
VERUS_TOOLS="${VERUS_TOOLS:-$REPO_ROOT/tools/verus-arm64-macos}"
CORE_CRATE="$REPO_ROOT/crates/tax"

export PATH="$VERUS_TOOLS:$PATH"

cd "$CORE_CRATE"
# Touch so a cached build still re-emits the verification results line.
touch src/lib.rs
exec cargo verus verify
