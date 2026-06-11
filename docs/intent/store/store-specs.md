# store — EARS Specs

Specs owned by the `store` leaf (prefix `STORE`; see `store-design.md`). Status: `[x]`
implemented · `[ ]` active gap · `[D]` deferred. `store` is the single Sheets-row↔event trust
seam; it holds no accounting/tax math.

## Schema

- [x] **STORE-SCHEMA-001**: The system shall persist `LedgerEvent`s and `TaxEvent`s in two append-only workbook tabs (`Ledger Events`, `Tax Events`), each with typed columns including `Seq`, `EventId`, `Date`, `Kind`, and the union of that family's fields.
- [x] **STORE-SCHEMA-002**: The system shall maintain a dense, contiguous per-tab `Seq` (starting at 1, no gaps) as the authoritative fold order, independent of a row's visual position; the two tabs have independent sequences.
- [x] **STORE-SCHEMA-003**: The system shall assign each event a globally-unique `EventId` at creation, invariant across retries, used as both the idempotency key and the Reversal target. CONTRACT: `runtime` owns global `EventId` assignment and cross-tab uniqueness enforcement (it holds the cross-tab id set a single-tab append cannot see); `store` consumes the assigned id and does not author the global-uniqueness mechanism here.
- [x] **STORE-SCHEMA-004**: The system shall derive the `EventId` with a deterministic, toolchain-stable content hash (golden-pinned FNV-1a over the event's canonical field encoding) — never `DefaultHasher` or any hasher whose output may vary across Rust versions or runs — so the same event content yields the same id on any toolchain and across processes.
- [x] **STORE-SCHEMA-005**: The `Tax Events` tab shall serialize all five `TaxEvent` kinds — `Allocate`, `Move`, `Pay`, `AmountOverride`, and `SeedMigration` — each via the total, wildcard-free `Kind` match (`STORE-GUARD-001`); `SeedMigration` is the combined-migration accrual kind the tax kernel requires (`TAX-ACCRUAL-007`), and the serializer shall handle it like any other kind.

## Write Path

- [x] **STORE-WRITE-001**: When appending an event, the system shall first confirm cache currency via the cheap per-tab fingerprint probe (count, max `Seq`, checksum — `STORE-CACHE-002`) and rebuild from the workbook (re-validating the event) on any change, before assigning `Seq`. CONTRACT: `runtime` owns post-rebuild re-validation — who re-runs the kernel validator against the refreshed view and how the revalidated view is handed back; `store` performs the rebuild and surfaces the refreshed log, deferring the re-validation handshake to `runtime`.
- [x] **STORE-WRITE-002**: When appending, the system shall assign `Seq` from the live workbook's current max `+ 1` (never from the cache), allocating a multi-event submit a contiguous block up front and appending in `Seq` order.
- [x] **STORE-WRITE-003**: When an event's `EventId` already exists in the workbook, the system shall skip the append if the stored row deserializes equal to the event, and otherwise raise `WriteVerifyMismatch`.
- [x] **STORE-WRITE-004**: After appending, the system shall read the row back by `EventId` and assert structural equality with the event written before confirming the write; a field mismatch shall raise `WriteVerifyMismatch`.
- [x] **STORE-WRITE-005**: If the workbook is unreachable or read-back finds no row, then the system shall return control to the owner to retry later, leaving no partial in-memory state and no background queue.
- [x] **STORE-WRITE-006**: The system shall update the local cache only after a write has been read-back-verified.
- [x] **STORE-WRITE-007**: Before mutating a workbook tab, the event-append primitive shall acquire the runtime-owned advisory write-lock (taking a `Lock`) and release it after, so lock coverage holds independent of the caller. The lock is reentrant for a same holder, so a caller already holding it (e.g. `import` holding it across an entire commit) re-acquires without deadlock. CONTRACT: `runtime` owns the lock type and the canonical acquisition requirement; `store` carries this local acquire-before-mutate obligation.
- [x] **STORE-WRITE-008**: Because the `EventId` is a content hash (`STORE-SCHEMA-004`), two byte-identical `TaxEvent`s are the same event, not two: the second carries the same `EventId`, so the idempotency check (`STORE-WRITE-003`) finds the row present and equal and skips the append. Tax-event de-duplication is therefore by content — distinct tax events must differ in at least one persisted field to receive distinct ids and both land.
- [x] **STORE-WRITE-009**: When appending to an event-log tab that does not exist (the missing-tab classification of `STORE-LOAD-007`), the system shall create the tab with the frozen header and the fingerprint block (`STORE-CACHE-002`) before the first append, so the first write to a fresh workbook bootstraps the schema. CONTRACT: `runtime`'s Sheets client performs the actual tab creation; `store`'s append primitive owns invoking it on the missing-tab signal and retrying the append once.

## Cache

- [x] **STORE-CACHE-001**: The system shall maintain a rebuildable local SQLite mirror of both event logs that carries no authoritative state.
- [x] **STORE-CACHE-002**: The system shall detect out-of-band changes via a cheap per-tab fingerprint of formula cells (`COUNTA`, `MAX(Seq)`, and a `SUMPRODUCT` checksum over the Seq/Date/money columns) read in one call — catching appends, deletions, and in-place edits to covered columns without re-reading the full log; missing or errored fingerprint cells are treated as a change.
- [x] **STORE-CACHE-003**: The system shall compute the content hash (over each row's `Seq`, `EventId`, and field cells) only during a full read it already performs — startup load, cache rebuild, or on-demand integrity verification — never as a standalone per-operation read.
- [x] **STORE-CACHE-004**: When the currency probe shows a change or the content hash mismatches, the system shall rebuild the cache from the workbook tabs (the workbook wins).
- [x] **STORE-CACHE-005**: While offline, the system shall serve event-log reads from the cache.

## Replay Loading & Integrity

- [x] **STORE-LOAD-001**: The system shall deserialize the event-log tabs (or cache) into `Vec<LedgerEvent>` and `Vec<TaxEvent>` and provide them to the kernels for replay.
- [x] **STORE-LOAD-002**: If a tab's `Seq` values are not dense (a gap, duplicate, or out-of-order value), then the system shall raise an integrity error and refuse to load rather than fold a corrupt log (recoverable via Google Sheets version history).
- [x] **STORE-LOAD-003**: If a row has an unknown `Kind` or a missing required field, then the system shall raise an integrity error rather than silently skip it.
- [x] **STORE-LOAD-004**: On load, for each Reversal row the system shall verify that its target `EventId` exists, has a lower `Seq`, and is not itself a Reversal; otherwise it shall raise an integrity error.
- [x] **STORE-LOAD-005**: The system shall not validate cross-tab references (a `Tax Event`'s `sale_id`/`lot_id`); a Tax Event with no backing `RealizedGain` is handled by the tax kernel's orphan rule (`TAX-VERIF-007`).
- [x] **STORE-LOAD-006**: The system shall expose an on-demand integrity verification that performs a full read of both event-log tabs, computes the content hash (`STORE-CACHE-003`), and compares it to the cache; on a mismatch it shall rebuild the cache from the workbook tabs (the workbook wins, per `STORE-CACHE-004`) and surface the refreshed log to the caller, returning success once cache and workbook agree. The caller invokes it at a trust point (e.g. before an authoritative quarterly/annual tax report), not per operation.
- [x] **STORE-LOAD-007**: When a read of an event-log tab fails because the tab does not exist (the Sheets client's non-retryable missing-tab classification, distinguished from a transport failure), the system shall treat that tab as an empty event log — a cold start, mirroring `CONFIG-SETTINGS-003` — rather than an unreachable workbook, so a fresh workbook with neither tab loads as an empty book; a genuine transport failure shall continue to surface as unreachable (`STORE-WRITE-005`).

## Trust-Boundary Drift Guard

- [x] **STORE-GUARD-001**: The system shall convert between rows and events by a total, wildcard-free structural match over every `Kind`, containing no accounting arithmetic.
- [x] **STORE-GUARD-002**: The system shall verify that `event → row → event` is the identity for every event kind and field (the drift guard), failing the build/test if a kind or field mapping is missing.
- [x] **STORE-GUARD-003**: The system shall keep the serde conversion outside the `verus!{}` boundary.
- [x] **STORE-GUARD-004**: For any field serialized into a list/structured cell whose encoding reserves the delimiters `;`, `:`, `~`, or the `State:` marker, the conversion shall either escape every occurrence of a reserved delimiter in `EventId`s, labels, and other free-text values so it round-trips unambiguously, or constrain those values to a delimiter-free charset; the `event → row → event` identity (`STORE-GUARD-002`) shall hold for values that contain reserved delimiters.
