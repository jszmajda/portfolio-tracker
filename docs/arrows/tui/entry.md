# Arrow: tui-entry

The TUI's write side: the composer and shared write loop, activity entry, the lot picker,
tax-accrual actions, and in-TUI config edits. Leaf under the `tui` sub-HLD.

## Status

**OK** — last audited 2026-06-11 (git SHA `2e43e4dd8896b05a75325c9a08fd37846cfb172d`). All 30
specs implemented with citing tests.

## References

### HLD
- docs/high-level-design.md (local terminal UI)
- docs/intent/tui/tui-design.md (the `tui` sub-HLD: shell, design language, write-path loop)

### LLD
- docs/intent/tui/entry/entry-design.md

### EARS
- docs/intent/tui/entry/entry-specs.md (30 specs)

### Tests
- crates/tui/tests/entry_flow.rs
- crates/tui/tests/entry_activity.rs
- crates/tui/tests/entry_lot.rs
- crates/tui/tests/entry_tax.rs
- crates/tui/tests/entry_config.rs
- crates/tui/tests/entry_render_input.rs
- crates/tui/tests/entry_submit_wiring.rs
- crates/tui/tests/conventions.rs
- crates/pt/tests/shell_keymap.rs
- crates/pt/tests/submit_write_path.rs (submit → kernel → store → read-back loop)

### Code
- crates/tui/src/entry.rs
- crates/tui/src/form.rs
- crates/tui/src/lib.rs (shared shell/render)
- crates/pt/src/shell.rs, crates/pt/src/tui_app.rs, crates/pt/src/wiring.rs (binary glue)

## Architecture

**Purpose:** Every mutation rides one write loop — compose → kernel validate → append →
read-back-verify → confirmed — so an entry is durably recorded or still in the owner's hands.

**Key Components:**
1. Composer & write loop — the shared mutation UX
2. Activity entry — Buy/Vest/Sell/Split forms
3. Lot picker — specific-ID selection inside a Sell
4. Tax-accrual actions — allocate/move/pay lifecycle gestures
5. Config edits — the editable reference-data views

## Spec Coverage

| Category | Spec IDs | Implemented | Deferred | Gaps |
|----------|----------|-------------|----------|------|
| Composer & Write Loop | TUI-ENTRY-FLOW-001..010 | 10 | 0 | 0 |
| Activity Entry | TUI-ENTRY-ACT-001..006 | 6 | 0 | 0 |
| Lot Picker | TUI-ENTRY-LOT-001..006 | 6 | 0 | 0 |
| Tax-Accrual Actions | TUI-ENTRY-TAX-001..005 | 5 | 0 | 0 |
| Config Edits | TUI-ENTRY-CFG-001..003 | 3 | 0 | 0 |

**Summary:** 30 of 30 active specs implemented; 0 deferred.

## Key Findings

None.

## Work Required

None — segment is coherent.
