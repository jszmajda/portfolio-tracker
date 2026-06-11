# Arrow: ledger-core

The verified accounting kernel: event taxonomy, tax lots, splits, realized/unrealized P&L,
append-time validation — pure `verus!{}` code with Kani bounded harnesses.

## Status

**OK** — last audited 2026-06-11 (git SHA `7e5dd0c921e2570873d6dbc9fd85cab59c7cc5bb`). All 45
specs implemented with citing tests; Verus/Kani gates run in `scripts/ci.sh`.

## References

### HLD
- docs/high-level-design.md (verified accounting core)

### LLD
- docs/intent/ledger-core/ledger-core-design.md

### EARS
- docs/intent/ledger-core/ledger-core-specs.md (45 specs)

### Tests
- crates/ledger-core/tests/event.rs
- crates/ledger-core/tests/lot.rs
- crates/ledger-core/tests/split.rs
- crates/ledger-core/tests/pnl.rs
- crates/ledger-core/tests/err.rs
- crates/ledger-core/tests/verif.rs

### Code
- crates/ledger-core/src/lib.rs (the `verus!{}` core)
- crates/ledger-core/src/kani_proofs.rs (`#[cfg(kani)]` bounded harnesses)
- crates/pt-core/src/lib.rs (shared integer-money kernel: `Cents`, `MicroShares`, `Date`, rounding — no segment of its own)

## Architecture

**Purpose:** The deductively verified ledger kernel — replays the append-only event log into
lots and P&L; rejects invalid events at append time.

**Key Components:**
1. Event taxonomy + `Seq` ordering — Buy/Vest/Sell/Split replay
2. Tax-lot ledger — specific-ID selection with FIFO fallback
3. P&L — realized on consumption, unrealized from marks
4. Append-time validators — `LedgerError` taxonomy
5. Verus invariants + Kani harnesses over the arithmetic kernel

## Spec Coverage

| Category | Spec IDs | Implemented | Deferred | Gaps |
|----------|----------|-------------|----------|------|
| Event Taxonomy & Ordering | LEDGER-EVENT-001..007 | 7 | 0 | 0 |
| Tax Lots & Lot Selection | LEDGER-LOT-001..006 | 6 | 0 | 0 |
| Splits | LEDGER-SPLIT-001..003 | 3 | 0 | 0 |
| Realized & Unrealized P&L | LEDGER-PNL-001..010 | 10 | 0 | 0 |
| Validation & Errors | LEDGER-ERR-001..011 | 11 | 0 | 0 |
| Verification Invariants | LEDGER-VERIF-001..008 | 8 | 0 | 0 |

**Summary:** 45 of 45 active specs implemented; 0 deferred.

## Key Findings

1. **`crates/pt-core` is shared-kernel code without its own segment** — its arithmetic is
   specified through `LEDGER-VERIF-*`/`TAX-VERIF-*` and the Kani harnesses target it
   directly; tracked here rather than as an orphan.

## Work Required

None — segment is coherent.
