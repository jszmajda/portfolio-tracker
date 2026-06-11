# Arrow: runtime

The shared live host: the one Google Sheets client, auth, the cross-process advisory
write-lock, the replay → project → read-marks → cache cycle, bootstrap, and event ids.

## Status

**OK** — last audited 2026-06-11 (git SHA `2e43e4dd8896b05a75325c9a08fd37846cfb172d`). All 26
specs implemented with citing tests; the live e2e is env-gated (`PT_E2E=1`).

## References

### HLD
- docs/high-level-design.md (runtime host / Sheets access)

### LLD
- docs/intent/runtime/runtime-design.md

### EARS
- docs/intent/runtime/runtime-specs.md (26 specs)

### Tests
- crates/runtime/tests/sheets.rs
- crates/runtime/tests/token_refresh.rs
- crates/runtime/tests/lock.rs
- crates/runtime/tests/cycle.rs
- crates/runtime/tests/boot.rs
- crates/runtime/tests/cache.rs
- crates/runtime/tests/revalidate.rs
- crates/runtime/tests/eventid.rs
- crates/runtime/tests/reports.rs
- crates/runtime/tests/adapters.rs
- crates/runtime/tests/e2e_real_sheets.rs (env-gated live round-trip)
- crates/import/tests/lock.rs (lock cascade from import)
- crates/pt/tests/boot_wiring.rs
- crates/pt/tests/commit_lock.rs
- crates/pt/tests/submit_write_path.rs

### Code
- crates/runtime/src/sheets.rs, auth.rs, lock.rs, cycle.rs, boot.rs, cache.rs,
  revalidate.rs, eventid.rs, history.rs, adapters.rs
- crates/pt/src/wiring.rs, crates/pt/src/tui_app.rs (binary wiring)
- crates/import/src/lib.rs (lock-holding commit seam)

## Architecture

**Purpose:** Owns every live seam: the single Sheets client, the advisory write-lock, the
replay cycle that turns the event log into views, and the composition root the binary boots.

**Key Components:**
1. Sheets-access layer — one client, retry/backoff, token refresh
2. Advisory write-lock — reentrant, TTL, lockfile
3. Replay & marks cycle — replay → project → read marks → cache
4. Boot — the composition root
5. Cache detection/rebuild + post-rebuild re-validation
6. Global EventId assignment, history-append retry

## Spec Coverage

| Category | Spec IDs | Implemented | Deferred | Gaps |
|----------|----------|-------------|----------|------|
| Sheets-Access Layer | RUNTIME-SHEETS-001..005 | 5 | 0 | 0 |
| Advisory Write-Lock | RUNTIME-LOCK-001..007 | 7 | 0 | 0 |
| Replay & Marks Cycle | RUNTIME-CYCLE-001..006 | 6 | 0 | 0 |
| Composition Root & Bootstrap | RUNTIME-BOOT-001..003 | 3 | 0 | 0 |
| Cache Detection & Rebuild | RUNTIME-CACHE-001..002 | 2 | 0 | 0 |
| Post-Rebuild Re-validation | RUNTIME-REVALIDATE-001 | 1 | 0 | 0 |
| Global EventId Assignment | RUNTIME-EVENTID-001 | 1 | 0 | 0 |
| History-Append Retry Loop | RUNTIME-REPORTS-001 | 1 | 0 | 0 |

**Summary:** 26 of 26 active specs implemented; 0 deferred.

## Key Findings

1. **Live coverage is env-gated by design** — `e2e_real_sheets.rs` is `#[ignore]` unless
   `PT_E2E=1` + workbook id + credentials; the offline gate stays deterministic.

## Work Required

None — segment is coherent.
