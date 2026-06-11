#!/usr/bin/env bash
# Canonical end-to-end CI gate for portfolio-tracker. Run locally or from
# .github/workflows/ci.yml — IDENTICAL local/CI, mirroring the prior verified project's ci.sh.
#
#   ./scripts/ci.sh                         # fmt/clippy advisory + build + test
#                                           # + Verus/Kani per verified crate (iff
#                                           #   their toolchains are present, else
#                                           #   SKIP) + @spec-coverage
#   PT_CI_REQUIRE_VERUS=1 ./scripts/ci.sh   # FAIL if the Verus toolchain is absent
#   PT_CI_REQUIRE_KANI=1  ./scripts/ci.sh   # FAIL if the Kani  toolchain is absent
#   PT_E2E=1 PT_WORKBOOK_ID=… GOOGLE_APPLICATION_CREDENTIALS=… ./scripts/ci.sh
#                                           # ALSO run the live real-Sheets e2e
#
# The default `cargo test` is offline and deterministic (the verified kernels are
# pure — no clock, no randomness, no I/O), so this gate needs no secrets or network.
# Live paths (the real-Sheets e2e) are #[ignore]+env-gated and excluded unless
# PT_E2E=1 + creds are supplied. The HLD success metric "Verus/Kani pass in CI" is
# met by the Verus/Kani stages when their toolchains are provisioned.
#
# Owning intent: docs/intent/runtime (RUNTIME-SHEETS / -CYCLE — the e2e exercises
# the live Sheets client) + the per-crate verus!{} cores (ledger-core, tax). This
# script is the executable form of the project's verification + integration gate.
set -euo pipefail
cd "$(dirname "$0")/.."

# The crates whose verus!{} core is deductively verified (Verus) and whose bounded
# #[cfg(kani)] harnesses are model-checked (Kani). Each carries a verus/verify.sh
# and a [package.metadata.verus] verify=true marker, mirroring a prior private verified-Rust project.
VERIFIED_CRATES=(ledger-core tax)

# A >5min hang on a verification stage is a FAILURE, not a flake: gtimeout aborts at
# 300s and KILLs at 320s; exit 124 (the timeout signal) is surfaced loudly below.
TIMEOUT_BIN="${PT_CI_TIMEOUT_BIN:-gtimeout}"
TIMEOUT_DUR="${PT_CI_TIMEOUT_DUR:-300s}"
TIMEOUT_KILL="${PT_CI_TIMEOUT_KILL:-20s}"

# ---------------------------------------------------------------------------
# fmt — GATING. The workspace is rustfmt-formatted; rustfmt cannot parse Verus
# syntax and leaves the verus!{} macro bodies byte-untouched, so formatting
# never disturbs the verified cores. clippy stays advisory.
# ---------------------------------------------------------------------------
echo "== fmt check =="
cargo fmt --all --check

echo "== clippy (advisory) =="
cargo clippy --workspace --all-targets 2>&1 | tail -3 || echo "ADVISORY: clippy findings present"

# ---------------------------------------------------------------------------
# GATING: the workspace must build and the (offline, deterministic) test suite must
# pass. Un-||-suppressed under `set -euo pipefail`, so a failure aborts the gate.
# ---------------------------------------------------------------------------
echo "== build --workspace --locked =="
cargo build --workspace --locked

echo "== test --workspace --locked (offline/deterministic) =="
cargo test --workspace --locked

# ---------------------------------------------------------------------------
# GATING when present, SKIP when absent: per verified crate, the Verus deductive
# proof AND the Kani bounded model checking, each under gtimeout (a >5min hang =>
# exit 124 => loud FAIL). Absent toolchain + not required => SKIP; absent +
# PT_CI_REQUIRE_{VERUS,KANI}=1 => hard FAIL.
# ---------------------------------------------------------------------------

# A gtimeout-wrapped stage runner: surfaces a 124 (>5min hang) loudly and re-raises
# any non-zero as a gate failure.
run_guarded() {
  local label="$1"; shift
  set +e
  "$TIMEOUT_BIN" --kill-after="$TIMEOUT_KILL" "$TIMEOUT_DUR" "$@"
  local rc=$?
  set -e
  if [ "$rc" -eq 124 ]; then
    echo "FAIL: $label HUNG (>$TIMEOUT_DUR, killed) — a verification hang is a failure, surfacing loudly" >&2
    exit 124
  elif [ "$rc" -ne 0 ]; then
    echo "FAIL: $label exited $rc" >&2
    exit "$rc"
  fi
  echo "OK: $label"
}

have_verus() { command -v cargo-verus >/dev/null 2>&1 || [ -d tools/verus-arm64-macos ]; }
have_kani()  { command -v cargo-kani  >/dev/null 2>&1; }

echo "== Verus deductive proof (verus!{} cores) =="
if have_verus; then
  for crate in "${VERIFIED_CRATES[@]}"; do
    echo "-- Verus: $crate --"
    run_guarded "verus($crate)" bash "crates/$crate/verus/verify.sh"
  done
else
  msg="Verus toolchain absent (tools/ is .gitignored — provision it in CI)"
  if [ "${PT_CI_REQUIRE_VERUS:-0}" = "1" ]; then echo "FAIL: $msg" >&2; exit 1; fi
  echo "SKIP: $msg"
fi

echo "== Kani bounded model checking (#[cfg(kani)] harnesses) =="
if have_kani; then
  for crate in "${VERIFIED_CRATES[@]}"; do
    echo "-- Kani: $crate --"
    run_guarded "kani($crate)" cargo kani -p "$crate"
  done
else
  msg="Kani toolchain absent (provision it in CI)"
  if [ "${PT_CI_REQUIRE_KANI:-0}" = "1" ]; then echo "FAIL: $msg" >&2; exit 1; fi
  echo "SKIP: $msg"
fi

# ---------------------------------------------------------------------------
# GATING: @spec-coverage across ALL segments. Every behavioral EARS spec defined in
# docs/intent/**/*-specs.md must be cited by at least one test (`// @spec <ID>` in a
# crates/*/tests file). A defined spec with no citing test is an uncovered behavior
# and FAILS the gate; a citation of an ID that no spec defines is a stale/typo
# citation and also FAILS (the arrow stays coherent in both directions).
# ---------------------------------------------------------------------------
echo "== @spec-coverage (every behavioral spec has a citing test) =="
defined="$(mktemp)"; cited="$(mktemp)"
trap 'rm -f "$defined" "$cited"' EXIT
# Defined: the bold EARS IDs in the specs docs (e.g. **LEDGER-EVENT-001**).
grep -rhoE '\*\*[A-Z][A-Z]+-[A-Z0-9-]*[0-9]+\*\*' docs/intent/ \
  | tr -d '*' | sort -u > "$defined"
# Cited: every @spec-annotated ID in a TEST file.
grep -rhoE '@spec [A-Z][A-Z0-9, -]*' crates/*/tests 2>/dev/null \
  | grep -oE '[A-Z][A-Z]+-[A-Z0-9-]*[0-9]+' | sort -u > "$cited"

uncovered="$(comm -23 "$defined" "$cited" || true)"
stale="$(comm -13 "$defined" "$cited" || true)"
ndef=$(wc -l < "$defined" | tr -d ' '); ncit=$(wc -l < "$cited" | tr -d ' ')
echo "   $ndef specs defined, $ncit distinct spec IDs cited by tests"
cov_fail=0
if [ -n "$uncovered" ]; then
  echo "FAIL: behavioral specs with NO citing test:" >&2
  echo "$uncovered" | sed 's/^/   - /' >&2
  cov_fail=1
fi
if [ -n "$stale" ]; then
  echo "FAIL: test @spec citations for IDs no spec defines (stale/typo):" >&2
  echo "$stale" | sed 's/^/   - /' >&2
  cov_fail=1
fi
if [ "$cov_fail" -eq 0 ]; then
  echo "OK: every behavioral spec is cited by a test"
else
  exit 1
fi

# ---------------------------------------------------------------------------
# GATING when enabled: the live real-Sheets END-TO-END round-trip. Runs ONLY when
# PT_E2E=1 AND the workbook id + credentials are supplied — it authenticates and
# hits the network, so it is excluded from the default offline gate. It drives
# runtime's REAL GoogleSheetsApi against the configured workbook, round-trips a
# couple of events -> replay -> Snapshot -> project views -> read marks back ->
# report + summary, asserts reconciliation, and self-cleans its dedicated test tabs.
# ---------------------------------------------------------------------------
echo "== real-Sheets e2e (live round-trip) =="
if [ "${PT_E2E:-0}" = "1" ] \
   && [ -n "${PT_WORKBOOK_ID:-}" ] \
   && [ -n "${GOOGLE_APPLICATION_CREDENTIALS:-}" ]; then
  # The e2e is #[ignore]; --include-ignored opts it in. --nocapture surfaces its
  # progress line. It is a single, self-cleaning, idempotent test.
  cargo test -p runtime --locked --test e2e_real_sheets -- --include-ignored --nocapture
else
  echo "SKIP: real-Sheets e2e (set PT_E2E=1 + PT_WORKBOOK_ID + GOOGLE_APPLICATION_CREDENTIALS)"
fi

echo "== CI OK =="
