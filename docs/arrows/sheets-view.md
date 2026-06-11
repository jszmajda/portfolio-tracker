# Arrow: sheets-view

The published read-only Google Sheets view: engine-projected view tabs, the value/formula
boundary with the `GOOGLEFINANCE` live-price column, marks read-back, and republish.

## Status

**OK** — last audited 2026-06-11 (git SHA `7e5dd0c921e2570873d6dbc9fd85cab59c7cc5bb`). All 24
specs implemented with citing tests.

## References

### HLD
- docs/high-level-design.md (published Sheets view / price oracle)

### LLD
- docs/intent/sheets-view/sheets-view-design.md

### EARS
- docs/intent/sheets-view/sheets-view-specs.md (24 specs)

### Tests
- crates/sheets-view/tests/tabs.rs
- crates/sheets-view/tests/formula.rs
- crates/sheets-view/tests/mapping.rs
- crates/sheets-view/tests/marks.rs
- crates/sheets-view/tests/republish.rs
- crates/runtime/tests/publish_cycle.rs
- crates/runtime/tests/cold_start.rs

### Code
- crates/sheets-view/src/lib.rs
- crates/runtime/src/cycle.rs (publish step of the replay cycle)
- crates/pt/src/wiring.rs (live wiring)

## Architecture

**Purpose:** Projects the replayed book into clean, filterable view tabs with a
`GOOGLEFINANCE` column as the live-price oracle, and reads the computed marks back.

**Key Components:**
1. View tabs & layout projection
2. Value / formula boundary — engine values vs sheet formulas
3. Symbol → ticker alias mapping
4. Marks read-back — the price oracle's return path
5. Republish — full-overwrite idempotence

## Spec Coverage

| Category | Spec IDs | Implemented | Deferred | Gaps |
|----------|----------|-------------|----------|------|
| View Tabs & Layout | SHEET-TAB-001..005 | 5 | 0 | 0 |
| Value / Formula Boundary | SHEET-FORMULA-001..005 | 5 | 0 | 0 |
| Symbol → Ticker Mapping | SHEET-MAP-001..002 | 2 | 0 | 0 |
| Marks Read-Back | SHEET-MARK-001..008 | 8 | 0 | 0 |
| Republish | SHEET-PUB-001..004 | 4 | 0 | 0 |

**Summary:** 24 of 24 active specs implemented; 0 deferred.

## Key Findings

None.

## Work Required

None — segment is coherent.
