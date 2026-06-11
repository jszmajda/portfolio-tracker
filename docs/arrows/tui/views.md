# Arrow: tui-views

The TUI's read side: Positions, Open Lots, History, Tax & Reserves, and Realized screens,
plus navigation, chrome, hints, and autorefresh. Leaf under the `tui` sub-HLD.

## Status

**OK** — last audited 2026-06-11 (git SHA `2e43e4dd8896b05a75325c9a08fd37846cfb172d`). All 48
specs implemented with citing tests (including the autorefresh NAV-017..019 set).

## References

### HLD
- docs/high-level-design.md (local terminal UI)
- docs/intent/tui/tui-design.md (the `tui` sub-HLD: shell, design language, motifs)

### LLD
- docs/intent/tui/views/views-design.md

### EARS
- docs/intent/tui/views/views-specs.md (48 specs)

### Tests
- crates/tui/tests/views_screens.rs
- crates/tui/tests/views_columns.rs
- crates/tui/tests/views_nav.rs
- crates/tui/tests/chrome_hints_help.rs
- crates/tui/tests/conventions.rs
- crates/pt/tests/shell_keymap.rs
- crates/pt/tests/shell_autorefresh.rs
- crates/pt/tests/view_history_wiring.rs
- crates/pt/tests/wiring.rs

### Code
- crates/tui/src/views.rs
- crates/tui/src/lib.rs (render layer, summary band, status line, hints)
- crates/tui/src/theme.rs (palette, money forms, glyphs)
- crates/pt/src/shell.rs, crates/pt/src/tui_app.rs, crates/pt/src/wiring.rs (binary glue)

## Architecture

**Purpose:** Read-only projections of the replayed book in the TUI's design language —
exact reconciliation money in the summary bands, compact glance cells, honest degradation
for unpriced/partial books.

**Key Components:**
1. Positions — summary band (TOTAL VALUE / NET (POST-TAX) / REALIZED YTD) + titled table
2. Open Lots, History, Tax & Reserves, Realized screens
3. Navigation — screen stack, `1`–`5` switching, filters/sort/grouping, autorefresh
4. Chrome — masthead, as-of, status line, hints, help overlay

## Spec Coverage

| Category | Spec IDs | Implemented | Deferred | Gaps |
|----------|----------|-------------|----------|------|
| Positions / Composition | TUI-VIEW-POS-001..013 | 13 | 0 | 0 |
| Open Lots | TUI-VIEW-LOT-001..004 | 4 | 0 | 0 |
| History | TUI-VIEW-HIST-001..004 | 4 | 0 | 0 |
| Tax & Reserves | TUI-VIEW-TAX-001..004 | 4 | 0 | 0 |
| Realized | TUI-VIEW-REAL-001..004 | 4 | 0 | 0 |
| Navigation | TUI-VIEW-NAV-001..019 | 19 | 0 | 0 |

**Summary:** 48 of 48 active specs implemented; 0 deferred.

## Key Findings

None.

## Work Required

None — segment is coherent.
