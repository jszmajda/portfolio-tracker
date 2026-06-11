---
parent: high-level-design
prefix: STORE
---

# store

## Context and Design Philosophy

`store` is the **single trust seam** between the verified kernels and the outside world. It owns
the workbook's append-only **event-log tabs**, the **row ↔ event serialization** that crosses
into and out of `verus!{}`-verified code, the **append + read-back-verify** write path, and the
**rebuildable local cache**. Everything `store` does is I/O and structural conversion — it
contains **no accounting or tax math**. That is the point: by concentrating serialization here
and proving the kernels pure, the unverified surface is one small, totally-structural conversion,
locked by a drift-guard test exactly as in the prior verified project.

Principles:

- **The workbook is authoritative; the cache carries no truth.** The append-only event-log tabs
  are the source of record. The local cache is a disposable mirror, rebuilt from the workbook
  whenever a content fingerprint shows they diverge.
- **Structural, not semantic.** Row ↔ event is a total field-copy with a `Kind` discriminator;
  any computed value (basis, gain, tax) is produced by the kernels on replay, never stored as
  truth and never trusted on read.
- **Durably recorded or still in your hands.** A write is confirmed by reading it back and
  checking field equality before the TUI closes the activity; an unconfirmed write returns
  control to the owner (no background queue), per the HLD.

## Workbook Event-Log Schema

Two append-only tabs, both human-readable and filterable (the HLD's live-view requirement):

- **`Ledger Events`** — `LedgerEvent`s (Buy / Vest / Sell / Split / Reversal).
- **`Tax Events`** — `TaxEvent`s (Allocate / Move / Pay / AmountOverride / SeedMigration). `SeedMigration` is the combined-migration accrual kind the tax kernel requires for seeding closed prior-year accruals; the serializer handles it like any other kind.

Each tab uses **typed columns**: the universal `Seq`, `EventId`, `Date`, `Kind`, plus the union
of that family's fields (sparse — only the columns a `Kind` uses are populated), plus the
`platform` / `tracking_code` metadata on ledger acquisitions and sales. A blob/JSON column is
rejected — it would make the tabs unreadable and unfilterable.

- **`Seq`** is an explicit integer column, **dense and contiguous (1, 2, 3, …) per tab**, and is
  the **authoritative fold order** — independent of the row's visual position, so a human
  filtering or sorting the view cannot change replay order. The two tabs have **independent**
  `Seq` sequences.
- **`EventId`** is **globally unique across both tabs**, assigned once at event creation and
  **invariant across all retries** of that event. It is the Reversal target and the idempotency
  key; every `EventId` lookup (idempotency, read-back, Reversal target) is global.

## Serialization (the trust boundary)

`row → event` and `event → row` are **total, structural** conversions: an exhaustive match over
every `Kind`, copying typed fields, with no arithmetic. Serde derives and the conversion live
**outside** `verus!{}`. This is the only unverified seam between Sheets and the kernels, locked
by the **drift guard** (below).

## Append & Read-Back-Verify Write Path

On a TUI submit, for the event(s) the kernel has validated and accepted:

1. **Confirm currency.** Run the cheap currency probe (the per-tab fingerprint block — count,
   max `Seq`, checksum; see Local Cache) against the cache. On a change, rebuild from the workbook and have the event re-validated, so
   validation and append share one consistent view (guards the single-writer assumption against a
   stale cache).
2. **Assign Seq from the live workbook.** Read the tab's current max `Seq` **from the live
   workbook** (never the cache) and assign `base = max + 1`. A multi-event submit (e.g. a Vest
   plus its same-day sell-to-cover) is assigned a **contiguous block** locally up front
   (`base, base+1, …`) and appended strictly in order — Seqs are not re-derived per row.
3. **Idempotency check.** If the event's `EventId` already exists, read that row: if it
   deserializes **equal** to the event being written, the write already landed — skip the
   append; if it exists but **differs**, raise `WriteVerifyMismatch` (do not append, do not
   silently accept).
4. **Append** the row via the Sheets append API.
5. **Read back & verify.** Read the appended row by `EventId`, deserialize it, and assert
   **structural equality** with the event just written. Only then update the local cache and let
   the TUI close the activity. A field mismatch raises `WriteVerifyMismatch`; a missing row (or
   unreachable workbook) returns control to the owner to retry.
6. **Retry safety.** Because `EventId` is fixed across retries, a retry after an ambiguous write
   re-runs step 3 and never double-appends; a partial multi-event landing leaves a valid dense
   prefix, and the unlanded events re-append by idempotency.

## Local Cache

A local **SQLite** database mirrors **both event logs** for fast offline reads, replay, and
reporting. It is rebuildable and carries no truth. (Cached *marks* are **not** `store`'s —
`sheets-view` owns the `GOOGLEFINANCE` column and the caching of last-known marks.)

Currency is checked in **two tiers**, so the cache keeps its value (no full read per operation):

- **Cheap currency probe** — a small **fingerprint block of formula cells** maintained in each
  event-log tab (a dedicated metadata range): `COUNTA` (count), `MAX(Seq)`, and a
  `SUMPRODUCT`-weighted **checksum** over the `Seq`/`Date`/money columns (with character-code
  sums over key text columns), all referencing the event columns by whole-column range so they
  auto-extend on append. The probe reads **just these few cells in one call** — not the log — and
  because any covered cell change shifts the checksum, it catches appends (`MAX(Seq)` moves),
  deletions (count moves), **and in-place edits to covered columns** (checksum moves). It is a
  change-detector for *accidental* human edits, not a cryptographic hash; if the fingerprint
  cells are missing or error, the probe treats the tab as changed and triggers a full read.
- **Content hash** — a complete hash over every row's `Seq`, `EventId`, and field cells, computed
  **only during a full read `store` already performs**: startup load, a cache rebuild, or an
  on-demand integrity verification (e.g. before an authoritative quarterly/annual tax report).
  Never a standalone per-op full read; it is the belt-and-suspenders that also covers any field
  outside the checksum's reach.
- Any probe change or hash mismatch → **rebuild the cache from the workbook tabs** (the workbook
  wins).
- **Guarantee:** appends, deletions, and in-place edits to covered columns are caught cheaply at
  any time; an edit outside the checksum's coverage is caught at the next startup or on-demand
  content hash — fail loud, never silently served. Offline, reads serve from the cache; current
  valuation falls back to `sheets-view`'s last cached marks.

**The workbook is authoritative for all persisted data; the cache is a derived projection.**

## Replay Loading & Integrity

`store` reads the event-log tabs (or the cache), deserializes to `Vec<LedgerEvent>` and
`Vec<TaxEvent>`, and hands them to the kernels for replay (`ledger-core` then `tax`). Because the
workbook is human-editable, `store` runs an **integrity check** on load and refuses to feed a
corrupt log to the kernels (failing loud, not folding silently):

- **Dense `Seq`** per tab: values are unique, start at 1, and have no gap. A gap (a deleted row),
  a duplicate, or an out-of-order `Seq` is a hard integrity error surfaced to the TUI — with the
  recovery affordance that the workbook is a Google Sheet, so the deletion/edit can be undone via
  **Sheets version history**.
- Every row's `Kind` is known and its required columns are present (an unknown `Kind` or missing
  field is an integrity error, never a silent skip).
- A Reversal's target `EventId` **exists**, has a **lower `Seq`** than the Reversal, and is **not
  itself a Reversal** — the structural guarantees `ledger-core`'s totality proof assumes were
  established at append time. (Deeper dependency re-validation remains the kernel's job.)
- **Cross-tab references are not `store`'s gate.** A `Tax Event` referencing a `sale_id`/`lot_id`
  with no backing `RealizedGain` is handled by the tax kernel's orphan-no-op rule, not rejected
  here.
- **A fresh workbook is a cold start, not an outage.** A brand-new workbook has neither event-log
  tab; the Sheets API reports a read of a nonexistent tab as a non-retryable missing-tab error,
  which the client classifies distinctly from a transport failure. `store` treats a missing tab as
  an **empty event log** — the same cold-start posture `config` takes for its missing tabs — so
  the TUI and summary launch against a fresh workbook with an empty book. Only a genuine transport
  failure surfaces as unreachable. The write side completes the bootstrap: the first append to a
  missing tab **creates it** — frozen header plus the fingerprint block — then retries the append
  once; the actual tab creation is `runtime`'s Sheets client's job, invoked on `store`'s
  missing-tab signal.

## Trust Boundary & Drift Guard

The row ↔ event conversion is the project's one unverified seam. A **drift-guard test** (the
prior project's trust-boundary drift-guard idiom) locks it:

- a wildcard-free exhaustive match over every `LedgerEvent` and `TaxEvent` `Kind` (a new kind
  fails compilation until its mapping is added), and
- a round-trip property: `event → row → event` is the identity for every kind and field.

So a schema or kind change cannot silently drop or corrupt data; the guard fails first.

## Interfaces

- **Inbound (from `runtime`):** kernel-validated events to append (the TUI/import reach `store`
  through `runtime`).
- **Outbound (to `ledger-core`, `tax`):** deserialized `Vec<LedgerEvent>` / `Vec<TaxEvent>` for
  replay; integrity errors surfaced to the caller.
- **Via `runtime`:** all workbook I/O uses `runtime`'s **Sheets-access layer**, and the
  event-append primitive acquires `runtime`'s **advisory write-lock** before mutating a tab and
  releases it after (taking a `Lock`), so coverage holds independent of the caller. The lock is
  **reentrant for a same holder**, so `import` — which holds it across an entire commit
  (target-check + all appends) — re-acquires through the inner append without deadlock. `runtime`
  owns the lock type and the canonical acquisition requirement; `store` carries the local
  acquire-before-mutate obligation. `store` owns the event-log tabs and the
  **event-log cache** only; `config`, `sheets-view`, and `reports` each own their own tab/cache (no
  "belongs to store" delegation). Mark caching is `sheets-view`'s.
- **EventId assignment:** `store` derives the `EventId` for a **live** append (the entry path) via
  the toolchain-stable content hash and so guarantees retry-stability; `import` supplies a
  **deterministic** `EventId` (row-coordinate + legacy id) in the same id-space. `runtime` owns
  global `EventId` assignment and cross-tab uniqueness enforcement, holding the cross-tab id set a
  single-tab append cannot see.
  History uses its own trading-day key, not an `EventId` (an intentional, separate idempotency
  model).

## Decisions & Alternatives

| Decision | Chosen | Alternatives Considered | Rationale |
|----------|--------|------------------------|-----------|
| Event-log layout | Two typed-column tabs (`Ledger Events`, `Tax Events`), independent dense `Seq` | One unified tab; per-kind tabs; JSON-blob column | Typed columns keep the canonical log human-readable and filterable (HLD live-view); per-family split keeps each schema coherent. |
| Seq source | Read max from the **live workbook** at append; assign batches as a contiguous block | Read max from the cache; per-row re-derivation | The cache can be stale → a cache-sourced Seq collides; a contiguous block avoids intra-batch collisions. |
| Seq density | Dense/contiguous; a gap is a hard load error | Strictly-increasing, gaps tolerated | Contiguity *detects* a human deletion (fail loud) rather than silently dropping an event; recovery via Sheets version history. |
| EventId | Globally unique across tabs, fixed at creation, invariant across retries | Per-tab unique; regenerated per attempt | A stable global id is what makes idempotency, read-back, and Reversal-target lookups sound. |
| Write confirmation | Read back and assert field **equality**; mismatch → `WriteVerifyMismatch` | "Row exists" = success | Existence ≠ correctness on a human-editable tab; equality is what makes a write durable. |
| Idempotency | `EventId` exists **and row equals** the event → skip; exists-but-differs → error | Skip on existence alone | A pre-existing/edited row with the same id must not be accepted as the intended write. |
| Cache currency | Two-tier: a sheet-side formula fingerprint (`COUNTA` + `MAX(Seq)` + `SUMPRODUCT` checksum) read in one call on the hot path; full content hash only during full reads | Per-op content hash (a full read); `(count, max Seq)` only; Apps Script `SHA256` custom function | A per-op content hash re-reads the whole log, defeating the cache; a formula checksum catches in-place edits cheaply too. Sheets has no native crypto hash (so it's a checksum — fine for accidental edits), and an Apps Script hash risks stale custom-function caching. |
| Cache | Rebuildable SQLite mirror, workbook-authoritative, event-logs-only | NDJSON; cache-as-truth; cache marks here | SQLite gives fast queryable reads; marks belong to `sheets-view`, not `store`. |
| Corrupt-log handling | Integrity check on load, refuse to fold; structural Reversal checks | Best-effort skip; trust the sheet | A tampered log would silently corrupt every downstream number, and would break the kernel's totality assumption. |
| Fresh-workbook cold start | Missing event-log tab reads as an empty log; first append creates the tab (header + fingerprint block) | A manual init command; treating a missing tab as `Unreachable` | An owner-operator tool must launch against a brand-new workbook with zero setup; conflating "tab not created yet" with an outage made a fresh workbook present as an auth failure. A separate init step is one more thing to forget; lazy creation on first write keeps the read path pure. |

## Open Questions & Future Decisions

### Deferred
1. **Sheets row limits / archival.** Behavior as a log grows large (tab row caps, year-partitioned
   tabs, archival) — not a near-term concern at personal scale.
2. **Sheets API resilience.** Retry/backoff and rate-limit handling for the append/read-back calls
   (mechanics, not contract).
3. **Cache schema migration.** Versioning the local SQLite schema across releases (rebuildable, so
   a migration can always fall back to a full rebuild).

## References

- HLD: `docs/high-level-design.md` (event-log tabs vs view tabs; write-through with read-back).
- Kernels fed by `store`: `docs/intent/ledger-core/ledger-core-design.md`,
  `docs/intent/tax/tax-design.md` (orphan-no-op rule).
- Mark caching: `docs/intent/sheets-view/sheets-view-design.md` (owns `GOOGLEFINANCE` marks).
- Mirrored idiom: the prior project's trust-boundary drift guard.
