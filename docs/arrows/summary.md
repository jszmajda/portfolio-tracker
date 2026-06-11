# Arrow: summary

The headless daily summary (`pt summary`): emitted figures, day-over-day delta, capture
and locking, text/json output modes, and exit codes.

## Status

**OK** — last audited 2026-06-11 (git SHA `2e43e4dd8896b05a75325c9a08fd37846cfb172d`). All 23
specs implemented with citing tests.

## References

### HLD
- docs/high-level-design.md (daily summary — replaces the dailies scraper)

### LLD
- docs/intent/summary/summary-design.md

### EARS
- docs/intent/summary/summary-specs.md (23 specs)

### Tests
- crates/summary/tests/emit.rs
- crates/summary/tests/delta.rs
- crates/summary/tests/capture.rs
- crates/summary/tests/output.rs
- crates/summary/tests/exit.rs
- crates/pt/tests/dispatch.rs (argv → summary dispatch)

### Code
- crates/summary/src/lib.rs

## Architecture

**Purpose:** Runs one replay cycle headlessly and emits the owner's daily portfolio
summary with an honest day-over-day delta and meaningful exit codes.

**Key Components:**
1. Emitted summary figures
2. Day-over-day delta vs the last captured trading day
3. Capture & locking (shares the runtime lock discipline)
4. Output modes — text and `--json`
5. Exit codes — scriptable truthfulness

## Spec Coverage

| Category | Spec IDs | Implemented | Deferred | Gaps |
|----------|----------|-------------|----------|------|
| Emitted Summary | SUMMARY-EMIT-001..008 | 8 | 0 | 0 |
| Day-Over-Day Delta | SUMMARY-DELTA-001..005 | 5 | 0 | 0 |
| Capture & Locking | SUMMARY-CAP-001..003 | 3 | 0 | 0 |
| Output Modes | SUMMARY-OUT-001..004 | 4 | 0 | 0 |
| Exit Codes | SUMMARY-EXIT-001..003 | 3 | 0 | 0 |

**Summary:** 23 of 23 active specs implemented; 0 deferred.

## Key Findings

None.

## Work Required

None — segment is coherent.
