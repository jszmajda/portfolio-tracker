# Arrow: tax

The verified tax kernel: per-sale tax calculation over bracket data, the accrual lifecycle
(accrued → allocated → moved → paid), reserves, and append-time tax validation.

## Status

**OK** — last audited 2026-06-11 (git SHA `7e5dd0c921e2570873d6dbc9fd85cab59c7cc5bb`). All 44
specs implemented with citing tests; Verus/Kani gates run in `scripts/ci.sh`.

## References

### HLD
- docs/high-level-design.md (verified tax core)

### LLD
- docs/intent/tax/tax-design.md

### EARS
- docs/intent/tax/tax-specs.md (44 specs)

### Tests
- crates/tax/tests/calc.rs
- crates/tax/tests/lifecycle.rs
- crates/tax/tests/reserves_reports.rs
- crates/tax/tests/verif.rs

### Code
- crates/tax/src/lib.rs (the `verus!{}` core)
- crates/tax/src/kani_proofs.rs (`#[cfg(kani)]` bounded harnesses)
- crates/pt-core/src/lib.rs (shared integer-money kernel — see ledger-core.md)

## Architecture

**Purpose:** Computes per-sale tax on a tax-lot basis from validated bracket data, manages
per-sale accruals through their lifecycle, and reports reserves and quarterly positions.

**Key Components:**
1. Tax calculation — federal/state/LT/NIIT stacking in integer `ppm`/`Cents`
2. Accrual lifecycle state machine — accrued → allocated → moved → paid
3. Reserves — outstanding-by-jurisdiction rollups
4. Append-time validators — `TaxError` taxonomy (incl. the combined-migration key)
5. Verus invariants (bounded tax) + Kani harnesses

## Spec Coverage

| Category | Spec IDs | Implemented | Deferred | Gaps |
|----------|----------|-------------|----------|------|
| Tax Calculation | TAX-CALC-001..015 | 15 | 0 | 0 |
| Accruals & Lifecycle | TAX-ACCRUAL-001..008 | 8 | 0 | 0 |
| Reserves | TAX-RESERVE-001..002 | 2 | 0 | 0 |
| Reporting | TAX-REPORT-001..005 | 5 | 0 | 0 |
| Validation & Errors | TAX-ERR-001..006 | 6 | 0 | 0 |
| Verification Invariants | TAX-VERIF-001..008 | 8 | 0 | 0 |

**Summary:** 44 of 44 active specs implemented; 0 deferred.

## Key Findings

None.

## Work Required

None — segment is coherent.
