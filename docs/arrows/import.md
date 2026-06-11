# Arrow: import

One-time legacy-workbook migration: parse the hand-wired sheet, reconstruct the event log,
dry-run + reconcile against the legacy Positions, and commit through the verified kernels.

## Status

**OK** — last audited 2026-06-11 (git SHA `7e5dd0c921e2570873d6dbc9fd85cab59c7cc5bb`). All 33
specs implemented with citing tests (the migration itself was live-proven; the previously
test-uncovered MAP-007 / RECON-007 / RECON-008 / CORP-007 now have focused citing tests).

## References

### HLD
- docs/high-level-design.md (replaces the hand-wired Google Sheet)

### LLD
- docs/intent/import/import-design.md

### EARS
- docs/intent/import/import-specs.md (33 specs)

### Tests
- crates/import/tests/parse.rs
- crates/import/tests/mapping.rs
- crates/import/tests/corp.rs
- crates/import/tests/tax.rs
- crates/import/tests/recon.rs
- crates/import/tests/run.rs
- crates/import/tests/lock.rs
- crates/pt/tests/commit_lock.rs (real advisory-lock cascade)
- crates/pt/tests/import_owner_mappings.rs (owner-mappings glue: post-split-frame rewrite)

### Code
- crates/import/src/lib.rs (reconstruction, reconciliation, commit)
- crates/import/src/parse.rs (legacy grid → typed rows)
- crates/pt/src/import_app.rs (owner inputs, owner mappings, report rendering, flow)

## Architecture

**Purpose:** Reconstructs years of legacy rows into validated kernel events, classifies
every reconciliation residual, and refuses to write until the gate clears and the owner
accepts.

**Key Components:**
1. Parse — header-by-name grids → typed legacy rows, malformed never dropped
2. Owner inputs & mappings — splits, exclusions, remaps, post-split-frame rewrite, adjudications
3. Reconstruction — Buy/Vest/Sell/Split via the kernels
4. Dry-run reconciliation — matched / intended / owner-adjudicated / unexplained verdicts
5. Commit — typed owner-acceptance token + safety gate, resume-safe

## Spec Coverage

| Category | Spec IDs | Implemented | Deferred | Gaps |
|----------|----------|-------------|----------|------|
| Source Mapping | IMPORT-MAP-001..004 | 4 | 0 | 0 |
| Corporate Actions, Frame & FMV | IMPORT-CORP-001..005 | 5 | 0 | 0 |
| Historical Tax & Residency | IMPORT-TAX-001..003 | 3 | 0 | 0 |
| Dry-Run & Reconciliation | IMPORT-RECON-001..008 | 8 | 0 | 0 |
| Run Mechanics | IMPORT-RUN-001..008 | 8 | 0 | 0 |
| Owner's Activity Vocabulary & Frame Mappings | IMPORT-MAP-005..007, IMPORT-CORP-006..007 | 5 | 0 | 0 |

**Summary:** 33 of 33 active specs implemented; 0 deferred.

## Key Findings

1. **`pt::import_app` is the owner-mappings entry point** — `apply_owner_mappings`
   (IMPORT-CORP-006/007) lives in the binary glue crate's library half so its tests cite it
   directly (crates/pt/src/import_app.rs).

## Work Required

None — segment is coherent.
