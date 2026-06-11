# Arrow: store

The canonical persistence seam: append-only event-log tabs in the workbook, the serde
row ↔ event trust boundary, the rebuildable local cache, and replay loading.

## Status

**OK** — last audited 2026-06-11 (git SHA `2e43e4dd8896b05a75325c9a08fd37846cfb172d`). All 30
specs implemented with citing tests.

## References

### HLD
- docs/high-level-design.md (canonical store / trust seam)

### LLD
- docs/intent/store/store-design.md

### EARS
- docs/intent/store/store-specs.md (30 specs)

### Tests
- crates/store/tests/schema.rs
- crates/store/tests/write.rs
- crates/store/tests/cache.rs
- crates/store/tests/load.rs
- crates/store/tests/guard.rs
- crates/store/tests/cold_start.rs
- crates/runtime/tests/cold_start.rs
- crates/runtime/tests/eventid.rs
- crates/pt/tests/submit_write_path.rs (TUI submit → store write path)

### Code
- crates/store/src/lib.rs
- crates/store/src/serde_rows.rs (the single trust seam: Sheets row ↔ event)
- crates/store/src/sheets.rs
- crates/store/src/cache.rs

## Architecture

**Purpose:** Writes validated events through to the append-only workbook tabs and mirrors
them in a truth-free local cache; loads and integrity-checks the log for replay.

**Key Components:**
1. Tab schema — append-only event-log columns
2. Write path — append + read-back-verify
3. Cache — rebuildable mirror, never authoritative
4. Replay loading & integrity checks
5. Trust-boundary drift guard

## Spec Coverage

| Category | Spec IDs | Implemented | Deferred | Gaps |
|----------|----------|-------------|----------|------|
| Schema | STORE-SCHEMA-001..005 | 5 | 0 | 0 |
| Write Path | STORE-WRITE-001..009 | 9 | 0 | 0 |
| Cache | STORE-CACHE-001..005 | 5 | 0 | 0 |
| Replay Loading & Integrity | STORE-LOAD-001..007 | 7 | 0 | 0 |
| Trust-Boundary Drift Guard | STORE-GUARD-001..004 | 4 | 0 | 0 |

**Summary:** 30 of 30 active specs implemented; 0 deferred.

## Key Findings

None.

## Work Required

None — segment is coherent.
