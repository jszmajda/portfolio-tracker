# Arrow: reports

Report composition over the replayed book: portfolio composition points, value-over-time
series, history capture/persistence, and realized-P&L history.

## Status

**OK** — last audited 2026-06-11 (git SHA `2e43e4dd8896b05a75325c9a08fd37846cfb172d`). All 18
specs implemented with citing tests.

## References

### HLD
- docs/high-level-design.md (portfolio & quarterly-tax reports)

### LLD
- docs/intent/reports/reports-design.md

### EARS
- docs/intent/reports/reports-specs.md (18 specs)

### Tests
- crates/reports/tests/composition.rs
- crates/reports/tests/vot.rs
- crates/reports/tests/history.rs
- crates/reports/tests/realized.rs
- crates/runtime/tests/reports.rs
- crates/runtime/tests/cold_start.rs

### Code
- crates/reports/src/lib.rs

## Architecture

**Purpose:** Composes snapshot + marks + tax estimates into report figures (series points,
trends, realized YTD) consumed by the TUI, summary, and history capture.

**Key Components:**
1. Composition — `build_series_point` over snapshot/marks/estimates
2. Value-over-time series and trading-day calendar
3. History capture & persistence (append-once per trading day)
4. Realized-P&L history (mark-independent, replay-exact)

## Spec Coverage

| Category | Spec IDs | Implemented | Deferred | Gaps |
|----------|----------|-------------|----------|------|
| Composition | REPORT-COMP-001..006 | 6 | 0 | 0 |
| Value-Over-Time | REPORT-VOT-001..006 | 6 | 0 | 0 |
| History Capture & Persistence | REPORT-HIST-001..004 | 4 | 0 | 0 |
| Realized-P&L History | REPORT-REAL-001..002 | 2 | 0 | 0 |

**Summary:** 18 of 18 active specs implemented; 0 deferred.

## Key Findings

None.

## Work Required

None — segment is coherent.
