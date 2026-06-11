# runtime — EARS Specs

Specs owned by the `runtime` leaf (prefix `RUNTIME`; see `runtime-design.md`). Status: `[x]`
implemented · `[ ]` active gap · `[D]` deferred. `runtime` is the shared host (Sheets-access, the
advisory lock, the replay/marks cycle); it is I/O + orchestration, outside `verus!{}`.

## Sheets-Access Layer

- [x] **RUNTIME-SHEETS-001**: The system shall provide the single low-level Sheets-access client — authentication (via `config` credentials), read, append, `batchUpdate`, clear, and rate-limit/backoff/retry — through which all workbook I/O passes.
- [x] **RUNTIME-SHEETS-002**: Each segment's write discipline (append + read-back-verify, full-tab `batchUpdate`, History last-wins) shall be built on this one primitive; no segment opens its own Sheets client.
- [x] **RUNTIME-SHEETS-003**: The system shall mint the OAuth access token lazily (on the first request that needs it, from the `config` service-account credentials), cache it, and reuse the cached token for subsequent requests until expiry.
- [x] **RUNTIME-SHEETS-004**: The system shall treat a cached token as expired once it is within a 60-second safety margin of its stated expiry (`now + 60s ≥ expiry`), and shall refresh (re-mint) it before issuing the request, so a token never expires mid-flight in transit to the API.
- [x] **RUNTIME-SHEETS-005**: During a backoff/retry storm (`RUNTIME-SHEETS-001`), the system shall re-evaluate token freshness (`RUNTIME-SHEETS-004`) before each retry attempt and refresh once if the margin has lapsed, and shall coalesce concurrent refreshers so a retry storm triggers at most one in-flight re-mint rather than a thundering herd of re-auth requests.

## Advisory Write-Lock

- [x] **RUNTIME-LOCK-001**: The system shall provide a cross-process, machine-local advisory write-lock with a non-blocking `try_acquire() → Acquired | Held{holder, since}`, an explicit release, and a TTL by which a stale (crashed-holder) lock is reclaimable.
- [x] **RUNTIME-LOCK-002**: The system shall acquire the lock inside every Sheets-mutating primitive — store event-append (`STORE-WRITE-007`), reports History-append (`REPORT-HIST-001` via `RUNTIME-REPORTS-001`), `sheets-view::republish` (`SHEET-PUB-001`), config `put_*` (`CONFIG-SETTINGS-006`), and the `import` commit (held across the whole commit, `RUNTIME-LOCK-006`) — so coverage is independent of the caller. This is the canonical acquisition requirement; each consuming segment carries a local "shall acquire the runtime-owned advisory write-lock before mutating" clause that refers here.
- [x] **RUNTIME-LOCK-003**: When the lock is `Held`, the system shall return that to the writer, which applies its policy — entry fails non-destructively, summary runs read-only, import holds for the whole commit, and `reports.append_snapshot` acquires like any write.
- [x] **RUNTIME-LOCK-004**: The system shall acquire the lock by an atomic, exclusive operation (e.g. lockfile open with `O_EXCL`, or atomic create-temp-then-rename) — never a read-then-write check — so that under contention between a cron `summary` process and an interactive TUI process exactly one acquirer wins the lock and the other observes `Held{holder, since}`.
- [x] **RUNTIME-LOCK-005**: The lock shall be reentrant for a same holder: when the current holder identity matches the requesting holder, a nested `try_acquire` shall return `Acquired` without deadlock and shall not release the underlying lock until the holder's outermost release, so `RUNTIME-LOCK-002` (acquire inside every primitive) and `RUNTIME-LOCK-006` (import holds across the whole commit, inner primitives re-acquiring) coexist.
- [x] **RUNTIME-LOCK-006**: When `import` commits, the system shall hold the lock across the entire commit — the legacy-target check plus all event appends — so the check→append window admits no interleaved writer (no TOCTOU); inner mutating primitives re-acquire reentrantly under the held lock (`RUNTIME-LOCK-005`). CONTRACT: `import` carries the local "commit holds the runtime-owned advisory write-lock for the whole commit" clause that refers here.
- [x] **RUNTIME-LOCK-007**: When `try_acquire` encounters a stale lock past its TTL, the system shall reclaim it atomically (`RUNTIME-LOCK-004`) and grant acquisition, so a crashed holder never wedges later writers.
- [x] **RUNTIME-LOCK-008**: When the lockfile is unreadable or its holder/timestamp content is corrupt or unparseable, the system shall treat the lock as not validly held and reclaim it atomically (`RUNTIME-LOCK-004`) rather than refuse forever.
- [x] **RUNTIME-LOCK-009**: When the lockfile is observed mid-initialization (present but not yet bearing a complete holder/timestamp record), the system shall treat it as `Held` (not yet reclaimable), so an in-progress acquisition by another process is not stolen.
- [x] **RUNTIME-LOCK-010**: When the lock path is unwritable (the acquire itself fails for an I/O reason other than contention), the system shall surface an acquisition error to the caller rather than silently proceed as if `Acquired`.

## Replay & Marks Cycle

- [x] **RUNTIME-CYCLE-001**: The system shall be the replay caller — load the event log via `store`, replay `ledger-core` then `tax` with `config` and the prior cycle's cached marks, and hold the resulting `Snapshot`, accruals, and tax estimates for `tui`/`summary`/`reports` to render.
- [x] **RUNTIME-CYCLE-002**: The system shall cache the marks `sheets-view` reads back and inject them into the next replay, closing the produce-after-consume loop.
- [x] **RUNTIME-CYCLE-003**: The system shall reduce the per-symbol GOOGLEFINANCE quote dates to one trading-day key (the most-recent quote-epoch across priced symbols) for `reports`/`summary`/`tui`, preserving each symbol's own stamp and degraded flag.
- [x] **RUNTIME-CYCLE-004**: When building the marks request and the per-symbol quote-date freshness used in `RUNTIME-CYCLE-002`/`RUNTIME-CYCLE-003`, the system shall exclude positions closed to zero quantity (replay retains a qty-0 symbol in the position set, but a closed symbol is neither requested as a mark nor counted toward per-symbol or trading-day freshness).
- [x] **RUNTIME-CYCLE-005**: After a cycle that produced priced marks, the system shall trigger a History capture by calling `reports::append_snapshot` with the cycle's trading-day key (`RUNTIME-CYCLE-003`) and the priced marks projected via `to_priced_marks`, performing the append under the advisory write-lock (`RUNTIME-LOCK-002`); the History append last-wins per trading-day key (`REPORT-HIST-002`), so a TUI-triggered and a cron-triggered capture for the same trading day reconcile to one point rather than duplicate.
- [x] **RUNTIME-CYCLE-006**: After the marks read-back, the cycle shall run the next replay immediately — re-replaying `ledger-core` then `tax` with this run's cached marks — so the held outcome (`Snapshot`, tax estimates) consumers render is valued at this run's marks, and a one-shot process run produces priced output without a persisted marks cache; the prior cache (`RUNTIME-CYCLE-002`) still seeds the publish-half replay and the transient carry-forward.

## Composition Root & Bootstrap

- [x] **RUNTIME-BOOT-001**: The system shall own the composition root — the single place that constructs the concrete `Store`, the `AdvisoryLock`, and the `GoogleSheetsApi` client, and wires them in dependency order (`config` credentials → Sheets client → `store` → the replay/marks cycle) — so no entrypoint constructs or re-wires these collaborators itself.
- [x] **RUNTIME-BOOT-002**: The system shall expose the wired cycle through a `load_and_run_cycle` entry that the entrypoints call: it loads the event log, runs the replay & marks cycle (`RUNTIME-CYCLE-001`..`RUNTIME-CYCLE-005`) over the constructed collaborators, and returns the held `Snapshot` + accruals + cached marks + trading-day key for the caller to render or act on.
- [x] **RUNTIME-BOOT-003**: The `pt` binary (`crates/pt`) shall be the process entrypoint and shall delegate construction to the `runtime` composition root (`RUNTIME-BOOT-001`); the headless argv→stdout→`ExitCode` contract is owned by `summary` and the interactive entrypoint by `tui`, each running *through* the `runtime`-constructed collaborators rather than building their own.

## Cache Detection & Rebuild

- [x] **RUNTIME-CACHE-001**: The system shall own detection of cache-vs-workbook divergence — running the cheap per-tab currency probe / fingerprint that `store` and `config` defer to it (`STORE-WRITE-001`, `STORE-CACHE-002`, `CONFIG-SETTINGS-005`) — and shall treat any probe change, content-hash mismatch, or missing/errored fingerprint as divergence with the workbook authoritative.
- [x] **RUNTIME-CACHE-002**: On detected divergence the system shall rebuild the local cache from the authoritative workbook tabs, choosing full-replace (drop and re-materialize the cache from a complete workbook read) when the cache cannot be trusted incrementally — corruption, fingerprint error, or out-of-band deletion/in-place edit — and reconcile (apply the newly-appended tail) when the divergence is an append-only extension the fingerprint can localize. CONTRACT: `store` and `config` consume the rebuilt cache and do not author the detection/rebuild mechanism (`STORE-CACHE-004`, `CONFIG-SETTINGS-005`).

## Post-Rebuild Re-validation

- [x] **RUNTIME-REVALIDATE-001**: When a `store`-side append triggers a cache rebuild from the workbook (`STORE-WRITE-001`), the system shall re-validate the pending event against the *refreshed* view before `Seq` assignment — re-running the kernel validation that the original event passed, against the post-rebuild log — so an event that conflicts with rows appended out-of-band since it was composed is rejected rather than appended on stale assumptions. CONTRACT: `store` performs the rebuild and surfaces the refreshed log; `runtime` owns the re-validation handshake and hands the refreshed, re-validated view back to the writer.

## Global EventId Assignment

- [x] **RUNTIME-EVENTID-001**: The system shall own global `EventId` assignment and guarantee cross-tab uniqueness — it holds the union of ids across *both* the `Ledger Events` and `Tax Events` tabs (the cross-tab id set a single-tab append cannot see) and is where `assign_event_id` is wired into the append path. CONTRACT: `store` derives each id by the toolchain-stable content hash (`STORE-SCHEMA-004`) and consumes the assigned id (`STORE-SCHEMA-003`); `runtime` enforces that the same content id is treated as the same event (content de-dup, `STORE-WRITE-008`) and that two genuinely distinct events never collide across tabs.

## History-Append Retry Loop

- [x] **RUNTIME-REPORTS-001**: The system shall own the retry-and-flag loop around `reports::append_snapshot`: when the call returns `Err(WriteVerifyFailed)` (`REPORT-HIST-001`), the system shall retry the append under the advisory write-lock (`RUNTIME-LOCK-002`) up to a bounded number of attempts with backoff, and on continued failure shall surface the trading-day point as uncaptured/flagged rather than discard it silently — `reports` only flags the single failure; `runtime` owns the loop.
