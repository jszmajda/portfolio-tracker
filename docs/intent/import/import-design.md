---
parent: high-level-design
prefix: IMPORT
---

# import

## Context and Design Philosophy

`import` is the **one-time migration** from the legacy workbook into the new event-log tabs. It
**reconstructs the event stream** (Buy / Vest / Sell / Split) from the legacy `Stock Actions` and
`Stock Sales` tabs, runs every reconstructed event through the **verified kernel validation** (the
same path `entry` uses), and **reconciles** the replayed result against the legacy `Positions`
numbers before anything is committed. The legacy `Positions` tab is a **reconcile target, not a
source** — it is derived, so the importer recomputes it and compares.

It is built around two safety disciplines:

- **Dry-run first.** Reconstruct → validate → reconcile → **report**, writing nothing. The owner
  reviews the reconciliation (and the *intended* divergences) and only then commits.
- **Faithful except where we deliberately changed the model.** The new model fixes two things the
  legacy got wrong — **RSU basis** (FMV-at-vest, not the legacy `$0`) and **splits** (explicit
  events, not implicit) — so migrated realized/unrealized will *not* match legacy for those lots.
  Reconciliation classifies every difference as **matched (within rounding)** or **intended
  divergence (explained + quantified)**; an *unexplained* difference is a migration bug to fix
  before commit.

`import` is one-time tooling — integration-tested, with the reconciliation as its correctness
proof. It is not Verus-verified; the events it emits are validated by the kernel, so the imported
log is valid by construction.

## Source Mapping

| Legacy | New |
|---|---|
| `Stock Actions` Buy row | **Buy** event — lot id = legacy tranche id; basis from `$/share`, fees. |
| `Stock Actions` Vest row | **Vest** event — lot id = legacy tranche id; **FMV/share recovered from the legacy `$/share` column** (which holds the vest FMV even though legacy `Total Cost` = `$0`). |
| `Stock Sales` row | **Sell** event — specific-ID against the referenced tranche (lot id); qty = `Shares Sold`, price = `$/share`, fees, date. The legacy already tracks per-tranche sales, so the lot reference is recoverable directly. |
| `Positions` tab | **reconcile target** — recomputed and compared, never read as input. |
| derived columns (`rm.shs`, `Tax`, `Unr.*`, `#DIV/0!` …) | ignored (recomputed by the kernels). |

A legacy vest's **sell-to-cover** children (the `-a`/`-b` suffixed rows) become ordinary Sell
events against the vest lot at vest-date price (≈ zero gain), per the HLD's RSU model.

If a Vest row's FMV source (`$/share`) is itself `$0`, blank, or `#DIV/0!`, the FMV cannot be
recovered: the importer raises a **hard error requiring an owner-supplied FMV** for that vest — it
never defaults to `$0` (which would re-import the double-tax error).

## Corporate-Action & RSU-Basis Reconstruction

The known-hard case the dry-run must prove out: **AMZN's 20:1 split (2022-06-06)** with an RSU
vest before it and sales after it.

- **Share-frame rule (the load-bearing assumption).** Every legacy quantity is interpreted in the
  **share-frame of its own row date** — a Vest's `qty` is the as-of-vest count, a Sell's `qty` is
  as-of-sale; the legacy *remaining-shares* column is **ignored** (it is a derived, live-frame
  figure). The importer asserts this as a checked precondition; the explicit `Split` then carries
  pre-split lots into the post-split frame so post-split sales consume correctly. If a row offers
  **only** a live-remaining figure and no own-frame quantity, the row's share-frame cannot be
  established: the importer raises a **frame-ambiguous parse error** (a zero-share guard) rather
  than misread the live-frame number as an own-frame quantity.
- A **Split** event is reconstructed for each known corporate action and inserted at its date in
  `Seq` order, so pre-split acquisitions rescale before post-split sales consume them. The known
  actions (the AMZN split to start) are supplied as importer input, not guessed from the data.
- **Vest basis** uses the FMV recovered from the legacy `$/share`; the legacy `$0` basis is
  deliberately *not* carried (it is the double-taxation error the new model fixes).
- Event **ordering** is by date then a deterministic tie-break. A reconstruction that fails kernel
  validation (e.g. a sale dated before the split but carrying a post-split qty) is surfaced as an
  **import error**; the owner resolves it via a **per-row correction input** (a date/qty/frame
  override fed back into a re-run) — never by editing the canonical log.

## Historical Tax & Residency

- **Residency stamping.** The importer takes a **historical residency timeline** from the owner
  and stamps each historical Sell from `residency_on(sale_date)`. If the oldest sale predates the
  owner's earliest recalled move, an explicit **owner-asserted founding state** (a best guess,
  flagged) is **recorded as the founding residency entry** — satisfying `config`'s founding-entry
  rule, not bypassing it.
- **Prior-year accruals — combined, lifecycle-driven.** A **closed year** (filing deadline passed
  *and* owner-confirmed paid; the closed-year set is owner input, not clock-inferred) is seeded as a
  single **combined migration accrual per `(jurisdiction, tax_year)`** — *not* per-(sale, lot),
  because the legacy tax is a single per-sale figure with no federal/state or per-lot split, and a
  combined grain is where the legacy lump maps cleanly and the `0 ≤ applied ≤ gain` bound holds.
  That accrual is set to the legacy actual via `AmountOverride`, then driven through the **real
  lifecycle** — `Allocate → Move → Pay` — because `tax`'s `Pay` gate requires `Moved` (an
  override-then-Pay alone would be rejected). It leaves `outstanding = 0`; per-lot accruals for that
  closed year are superseded by the combined one and de-minimis auto-settled accruals are excluded.
  Seeding is **annual-granularity** (quarterly report N/A for migrated years). Open-year accruals
  are left live and per-`RealizedGain`. *(The combined migration accrual is `tax`'s construct —
  see `tax-design.md`, "Migration accrual".)*

  Every reconstructed tax event runs through the **same append-time tax-kernel gates** an `entry`
  tax append uses. The combined-migration key (empty `sale_id`/`lot_id`, no backing `RealizedGain`)
  is presented to that validator as a **first-class real key** (as `tax` requires for migration
  accrual keys), so the
  `AmountOverride → Allocate → Move → Pay` sequence against it validates against the seeded
  migration accrual rather than being rejected as an orphan. A reconstruction the tax kernel
  rejects surfaces as `ImportError::TaxKernelRejected` — never committed unvalidated.

## Dry-Run & Reconciliation

The default mode. The importer replays the reconstructed log → `Snapshot` and compares to the
legacy `Positions`, producing a report per symbol:

```
 SYMBOL   shares   realized P&L      unrealized        verdict
 GOOG       80     $0 ≈ $0           match (±$1)        ✓ matched
 AMZN     1600     legacy $48,000    new $8,000         ⚠ intended: RSU FMV-basis (−$40,000)
                                                          + explicit split — review
 PLTR        0     $2,000            $2,000             ✓ matched
```

- **Matched** — within tolerance: per position, ±(½¢ × consumed-lot count) rounded up to whole
  cents (the kernel's largest-remainder noise), plus a portfolio-total tolerance.
- **Intended divergence** — the divergence is **predicted independently**, never back-labelled
  from the residual, and **at the symbol-aggregate level** (where the kernel's sums are exact;
  per-lot gain carries ±½¢ noise): the expected RSU-basis delta = Σ (vest FMV value) for the
  symbol's vested shares (legacy charged `$0`), and the expected split delta = **exactly `$0`**
  (splits are basis-neutral). A residual within *predicted ± tolerance* is accepted as intended;
  **anything beyond predicted ± tolerance is Unexplained** — so a real bug cannot hide behind an
  intended label (the central safeguard for AMZN).
- **Unexplained** — any unattributed residual; a migration bug that **blocks commit** until fixed.
- **Share reconciliation** — reconstructed shares must match legacy per symbol within a
  micro-share tolerance; a sub-threshold residual on a closed position snaps via a closing
  adjustment, a larger one is flagged.

**Commit-blocking set.** The auto-computed `commit_allowed` safety gate clears only when the
reconciliation carries **no** blocking condition. The complete blocking set is: any **Unexplained**
dollar-divergence verdict, any **Flagged** share residual, a **non-empty malformed-source-row
list**, a **`Positions`-only symbol** with no surviving rows, and a **reconstructed-but-unpositioned
symbol** (a symbol rebuilt from surviving rows but absent from legacy `Positions`).

**Owner acceptance vs. the gate.** `commit_allowed` is the automatic safety gate above; it is
**distinct** from owner acceptance. **Commit** writes the reconstructed events through `store` only
when **both** are satisfied — a typed `AcceptedReport` owner-acceptance token **and** a clear
`commit_allowed` gate. The gate being clear never implies acceptance, and acceptance never bypasses
the gate.

## Run Mechanics

- **Fresh target, resumable, in order.** Import seeds a **fresh workbook**; it refuses one holding
  *other* events but **permits a workbook containing only this import's own deterministic events** —
  so a commit interrupted partway (a network drop mid-hundreds-of-appends) **resumes idempotently**,
  re-appending the missing tail **in the original reconstructed order** (so `store`'s dense `Seq`
  encodes the same fold order the dry-run reconciled). The commit acquires `runtime`'s advisory
  write-lock **before** the fresh/own-events-only target check and **holds it across that check and
  every append** (releasing only after the last) — so no other writer can interleave between the
  check and the appends (no check→append TOCTOU); other writers go read-only meanwhile. The lock is
  **reentrant for a same holder**, so the inner append primitives re-acquire it under the held lock.
- **Unique-id pre-pass.** Before reconstruction the importer validates **legacy tranche-id
  uniqueness** and **Sell→tranche referential integrity**, and derives `EventId`s from a
  **guaranteed-unique key (row coordinate + id)**, not the id alone — so a reused legacy id can
  never silently collide and drop a real event. A duplicate id or a Sell referencing a missing
  tranche is a surfaced error.
- **Kernel-validated.** Every event passes the same validation as an `entry` append; the imported
  log is valid by construction.
- **Reconstruct only from rows that exist.** A closed position (e.g. PLTR) reconstructs **only if
  its full Buy/Sell rows survive** in `Stock Actions`/`Stock Sales`; a position present *only* as a
  `Positions` aggregate is a **hard error** (the stream cannot be fabricated), surfaced for manual
  entry — never migrated as empty. The **inverse** case is also caught: a symbol reconstructed from
  surviving rows but **absent from legacy `Positions`** is surfaced as a **flagged, commit-blocking**
  reconciliation result, never silently omitted from the report.
- **View-hidden symbols are real holdings.** Symbols the legacy *daily script* hid from its
  view **are imported** — the hiding was a view concern, not a ledger fact — reconciled against
  `Stock Actions`/`Stock Sales`, not the filtered `Positions` view. The legacy workbook's
  non-portfolio tabs (an owner-configured out-of-scope list) stay out of scope.
- **Messy-data handling.** `#DIV/0!`/`#N/A`/blank cells in derived columns are ignored; genuinely
  malformed source rows are listed for manual review, never silently dropped.
- **Read-only legacy fetch, owner-layout parse.** The run begins by reading the three legacy tabs
  through the one runtime Sheets client and parsing the owner's *actual* column layout (headers
  located by name, not fixed position — `Stock Actions` carries a note row above its header) into
  the typed legacy rows. The legacy workbook is **never written** — fetch, parse, and dry-run are
  read-only end to end; only the commit writes, and only to the *new* workbook. A row whose
  **required source cell** is missing or unparseable routes to the malformed list (it is a source
  row, so it blocks commit); derived columns are never read. Owner inputs (the legacy workbook id,
  known corporate actions, closed years, the residency timeline, the platform each tranche lives
  on — facts the legacy sheet does not record) come from a local, gitignored owner-inputs file the
  binary reads alongside its settings.

## Decisions & Alternatives

| Decision | Chosen | Alternatives Considered | Rationale |
|----------|--------|------------------------|-----------|
| Mode | Dry-run → reconcile → explicit commit | Direct write | A one-time, high-stakes migration must be reviewable before it touches the canonical log. |
| Reconcile semantics | Matched / intended-divergence / unexplained-blocks-commit | Exact match required; no reconcile | RSU-basis and split fixes *intentionally* diverge from legacy; exact-match is impossible and no-reconcile is unsafe. |
| Vest basis | FMV recovered from legacy `$/share` | Carry legacy `$0`; ask per vest | The FMV is already in the legacy sheet; carrying `$0` would re-import the double-tax error. |
| Splits | Explicit `Split` events from known actions, inserted by date | Trust legacy's implicit/mixed numbers | Only explicit splits reconcile a pre-split vest with post-split sales correctly. |
| Prior-year tax | Seed closed years to **Paid** (legacy tax as the actual) | Recompute and leave open; skip | Those taxes are paid; leaving them open would clutter outstanding with settled history. |
| Target | Empty/fresh workbook only | Merge into an existing log | Seeding a canonical append-only log into a live one risks duplication/disorder. |
| Reconcile classification | Predict the intended delta independently; residual beyond predicted ± tolerance = Unexplained (blocks) | Back-label the residual as intended | Back-labelling lets a real reconstruction bug hide behind the RSU/split divergence on the hardest symbol. |
| Share-frame | Every quantity in its own row's frame; legacy remaining-shares column ignored; explicit Split bridges frames | Trust the legacy live remaining column | The legacy remaining is a live-frame derived figure; mixing frames is a silent 20× error. |
| EventId key | Row coordinate + legacy id (guaranteed unique) | Legacy id alone | A reused legacy id would silently collide and `store` would dedup-drop a real event. |
| Partial-commit resume | Target may hold *only this import's own* events → resume idempotently | Strict empty-only | Sheets appends aren't transactional; strict empty-only strands a partial commit with no resume. |
| Closed-year tax | `AmountOverride` to the legacy actual, then bulk `Pay` (outstanding = 0) | Recompute and leave open; skip | The new calculated accrual ≠ the legacy manual tax would leave a perpetual residual. |
| View-hidden symbols | Import the daily-script-hidden symbols (real holdings) | Skip per the daily script | The script's hiding is a view concern; skipping silently loses real positions. |
| Idempotency | `EventId`s derived from legacy ids | Random ids | Lets a test re-run be safe (store idempotency dedups). |
| Owner inputs | A local, gitignored owner-inputs file (legacy workbook id, splits, closed years, residency, platforms) read by the binary | CLI flags per run; hardcode in the binary; extend `config`'s settings | The inputs are one-time migration facts, not ongoing settings; a file is reviewable and re-runnable, flags are unwieldy for structured lists, and they don't belong in the long-lived config schema. |
| Legacy parse | Headers located by NAME in the owner's actual layout; only source columns read | Fixed column indexes; parse every column | The legacy sheet has note rows and shifting derived columns; name-anchored source-only parsing survives cosmetic edits and never trusts derived figures. |

## Open Questions & Future Decisions

### Deferred
1. **Mergers / spin-offs in history.** If any past holding had one (beyond splits) — depends on
   the corporate-action model `ledger-core` deferred.
2. **Dividend / non-portfolio legacy tabs.** The legacy workbook's non-portfolio tabs (an
   owner-configured out-of-scope list) are out of scope for this importer.

## References

- HLD: `docs/high-level-design.md` (faithful-migration goal; RSU basis; corporate actions).
- Legacy source: the owner's hand-wired Google Sheet (`Stock Actions`, `Stock Sales`,
  `Positions`) — its id is supplied via the gitignored owner-inputs file `import.local.json`
  (see `import.local.example.json`).
- Targets/segments written through: `ledger-core` (events, validation), `tax` (historical Pay),
  `config` (founding residency), `store` (write discipline, empty-target, idempotency).
