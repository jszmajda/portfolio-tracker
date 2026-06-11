---
parent: high-level-design
prefix: CONFIG
---

# config

## Context and Design Philosophy

`config` holds the **reference data** the rest of the system reads but the event log does not
produce: the tax-rule tables, the annual income estimate, the residency timeline, the platform
list, the de-minimis threshold, and the machine/secret settings. It is the one input that is
*not* a projection of the append-only log — so it is **mutable, versioned reference data**, not
an event stream. Where the log answers "what happened," `config` answers "under what rules and
where."

Two persistence homes, by sensitivity:

- **Domain config** (tax rules, income, residency, platforms, de-minimis) lives in dedicated
  **workbook tabs** — visible, filterable, backed up like everything else — mirrored into the
  local cache.
- **Machine/secret settings** (workbook id, service-account credentials path, cache location)
  live in a **local file** (TOML, à la the prior project's secrets file) and never touch the shared
  workbook.

`config` is mostly data and I/O and sits outside the `verus!{}` boundary. Its few pure helpers —
residency resolution, the staleness check, bracket validation — are unit- (and optionally Kani-)
tested. Its *validation* is the first gate that makes `tax`'s bounded-tax invariant hold: a malformed
or rate-excessive bracket set is rejected here before `tax` ever sees it.

## What config holds

**Money is `Cents` (i64); rates are fixed-point parts-per-million (`ppm`, i64 — actual rate =
`ppm / 1_000_000`, so `370000` = 37%, `38000` = 3.8%, `55250` = 5.525%)** — integer-exact and
fine enough to represent every published federal/DC/NJ rate (including NJ's 5.525%) without a
float.

### Tax-rule tables (per `tax_year`)

- **Filing status** — `Single | MarriedFilingJointly | …` — selects which bracket/threshold set
  applies; recorded per year (it can change between years). A *mid-year* status change is out of
  scope: a year carries one status (the elected/year-end one).
- **Federal ordinary brackets** — ordered `(lower_threshold_cents, rate_ppm)` rows.
- **Federal long-term brackets** — ordered `(lower_threshold_cents, rate_ppm)` rows (0/15/20%).
- **NIIT** — `{ rate_ppm, magi_threshold_cents }`.
- **State ordinary brackets** — per state code (`DC`, `NJ`, …), ordered `(threshold, rate_ppm)`.
- **Annual ordinary income** — the year's `ordinary_income_cents` estimate (drives bracket
  position; stays live for the open year).
- Each row-set carries `last_verified: Date` and a `source_note` (the IRS/state publication).

### De-minimis threshold

A single `de_minimis_cents` (e.g. `100` = $1). It is compared to **`|accrual amount|`** — the
quantity `tax` auto-settles. The annual report reuses the *same* threshold as a display floor on
`|gain|` (to suppress a meaningless effective rate); it is one configured value, two uses.

### Residency timeline

Effective-dated entries `(effective_date, state_code)`, kept sorted, unique-dated, and without
consecutive same-state entries (such a no-op is rejected on edit). `residency_on(date)` returns
the state of the entry with the greatest `effective_date ≤ date` — so **a sale on the exact
effective date resolves to the new state** (the move is inclusive of its effective date).
Future-dated entries are **allowed** (to record a known upcoming move) and resolve normally for
dates on/after their effective date. This defaults a new sale's `accrues_to_state` (overridable
per transaction) and backfills historical sales during import. When the default
`residency_on(sale_date)` is None (the date falls in the timeline's undefined pre-history), a
**manually-entered** sale must carry an explicit `accrues_to_state` — the entry path blocks
submit otherwise. (Import never reaches this case: `config` requires a founding residency entry
before import runs.)

### Platform list

Platform names. Platforms are **free text on events** (no integration); the list is a TUI
suggestion source, not a constraint — an event may name a platform absent from the list, and the
TUI may offer to add it.

### Symbol → ticker aliases

A small map from a ledger `Symbol` to the ticker form `GOOGLEFINANCE` expects (an
exchange-prefixed or class-share form, e.g. `BRK.B`). Identity is assumed when a symbol has no
alias entry. Consumed by `sheets-view` to build its price formula. (Stored in the workbook's
`Platforms & Aliases` config tab.)

### Symbol → display names

A sibling map from a ledger `Symbol` to its **company display name** (`AMZN` → `Amazon.com`),
because the owner does not always recognize tickers. The ticker itself is assumed when a symbol
has no entry, so an unmapped symbol degrades honestly to what the ledger already shows. Consumed
by the TUI's name columns (Positions, Open Lots); persisted and round-tripped with the rest of
the domain config, alongside the alias map.

### Machine/secret settings (local file)

`workbook_id`, `credentials_path`, `cache_path`, and `reporting_timezone` (default `US/Eastern`;
DC and NJ are both Eastern) — the locale `reports` uses for capture-date metadata.

## Resolution helpers (degradation & staleness)

**Per-jurisdiction bracket resolution.** A request for jurisdiction `J` (federal or a state) and
`tax_year` `Y` returns, independently per jurisdiction:

- `J`'s set verified for `Y`, if present; else
- `J`'s **most recent prior-year** set (walking down `J`'s own year series), flagged `Stale`; else
- (no set in any year `≤ Y` for `J`) the distinct signal **`NoBracketsAvailable`** — so `tax`
  can surface "cannot estimate `J`, enter brackets" rather than crash or silently zero. This is
  the **cold-start** contract (first-ever use, or a state newly added with no history).

**Staleness signal.** A jurisdiction's set is **stale** for the current tax year when no
verified set exists for it that year, or the newest set's `last_verified` is older than the
threshold (default 12 months). The check runs over **active jurisdictions = federal + every
state appearing in the residency timeline within the lookback window** (default: current and
prior tax year; future-dated entries are excluded until effective). The set is computed entirely
from `config`'s own data — it does **not** consult `tax` accrual state. The window may
over-include a state recently moved away from; that is intentional (a left-but-recent state can
still carry an unpaid prior-year liability worth a reminder). The signal names each
(jurisdiction, year) needing attention; the *refresh procedure* is a runbook, not code.

## Config-edit interface

Edits flow through the TUI (`tui` renders the forms; `config` validates and writes the workbook
tabs / local file): add a year's brackets, update income, record a residency change, manage
platforms, set the de-minimis. On every write `config` runs the validation below and refuses an
invalid set. The refresh runbook is the human procedure that feeds this interface.

**Seeding.** Initial values come from a seeding harness that reads the owner's facts — filing
status, income estimate, residency state, de-minimis, platforms, display names — from a
gitignored owner seed file and pairs them with the matching transcribed federal ordinary + LT
brackets, NIIT MAGI threshold, and the owner's state brackets. Filing status is recorded per
year and may change in a future year.

## Validation

- **Each bracket set** must be **non-empty**, with **strictly ascending, unique**
  `lower_threshold_cents`, a **first row at `0`**, and every `rate_ppm ≥ 0`. (A single flat-rate
  `[(0, r)]` is valid.)
- **Stacked-rate ceiling.** For every `(tax_year, state)`, the worst-case stacked top marginal
  rate — `max(federal-ordinary top, federal-LT top) + that state's top + NIIT` (the most a
  single gain dollar can face) — must be `< 1_000_000 ppm` (100%). This is the real gate behind
  `tax`'s bounded-tax invariant; a per-table check alone would not catch stacking. The check also
  runs on the **state-independent federal baseline** — `max(federal-ordinary top, federal-LT top)
  + NIIT` — so a stateless year whose federal-plus-NIIT alone reaches 100% is gated even when no
  `(tax_year, state)` pair exists.
- **Residency timeline** must have unique, sorted `effective_date`s, no consecutive same-state
  entry, and — *before an import runs* — at least one entry at or before the earliest event date
  to be classified. That founding entry **may be an owner-asserted (flagged) best-guess state**
  recorded as the entry; import is blocked only until such a founding entry exists, so
  `residency_on` is never queried in its undefined pre-history region.
- **NIIT rate / MAGI threshold / de-minimis / income** must be non-negative.

## Read contract & failure modes

- **Missing config tabs** (a fresh workbook) → treated as cold-start: `NoBracketsAvailable`, and
  the TUI prompts to seed config.
- **Missing or invalid credentials / unreadable secrets file** → a hard error surfaced to the
  TUI; `config` does not silently return empty data.
- **Cache vs workbook divergence** → the workbook tabs win and `config` rebuilds **its own**
  cache. (`config` owns its cache; the shared Sheets-access layer and the advisory lock are
  `runtime`'s — not `store`'s.)

## Trust Boundary & Interfaces

- **Outbound (to `tax`):** federal/state/LT brackets, NIIT, income, de-minimis — validated
  `Cents`/`ppm` data, each set tagged `Verified | Stale | NoBracketsAvailable`; plus
  `residency_on(date)` for defaulting. (Filing status is **config-internal** — it selects which
  bracket set to emit — not a field `tax` reads.) Income is a per-year column and de-minimis a
  single cell on the **Tax Rules** tab; neither needs its own tab.
- **Outbound (to `ledger-core`):** nothing structural — FIFO fallback is intrinsic to
  `ledger-core`; `config` does not drive lot selection.
- **Outbound (to `tui`):** the staleness signal, the platform suggestions, the editable views.
- **Persistence:** domain tabs via the shared Sheets access (alongside `store`); secrets via the
  local file. Serde lives outside any verified boundary. Every config `put_*` primitive is a
  Sheets-*mutating* operation and so acquires the **runtime-owned advisory write-lock** before
  mutating and releases it after; the lock is reentrant for a same holder. `runtime` owns the
  lock type and the canonical acquisition requirement.

## Decisions & Alternatives

| Decision | Chosen | Alternatives Considered | Rationale |
|----------|--------|------------------------|-----------|
| Config nature | Mutable, versioned reference data (not event-sourced) | A `ConfigEvent` append-log | Brackets/income/residency are reference tables keyed by year/effective-date; `last_verified` gives the audit a full event stream would, without the machinery. The append-only tenet governs the *truth log*, not reference data. |
| Two homes | Domain config in workbook tabs; secrets in a local file | All in workbook; all local | Income/brackets benefit from visibility and backup; credentials must never live in the shared sheet. |
| Rate units | Fixed-point `ppm` (i64, 1e-6) | Basis points; float percent | bps cannot represent NJ's 5.525% (552.5 bps); ppm represents every published rate exactly and stays integer for the verified kernel. |
| Stacked-rate gate | Validate `max(fed-ord, fed-LT) + state + NIIT < 100%` per (year, state) | Per-table `<100%` only | A single gain dollar faces the stacked sum; per-table checks miss it, leaving `tax`'s bounded-tax invariant ungated. |
| Missing-year brackets | Per-jurisdiction degrade to most recent prior set (flagged `Stale`); none ≤ year → `NoBracketsAvailable` | One global series; hard-fail; assume zero | Federal and each state have independent sparse year series; cold-start needs a distinct signal so `tax` prompts rather than zeros. |
| Staleness active set | Federal + residency-timeline states within a lookback window | Include "states with unpaid accruals" | `config` cannot see accrual state (that's `tax`); residency + lookback keeps it self-contained, over-including safely. |
| Residency model | Effective-dated timeline, `residency_on` inclusive of effective date, future-dated allowed | Single current value; exclusive boundary | Intra-year moves, known upcoming moves, and historical import all need date-resolved residency; the per-event stamp stays authoritative. |
| Import precondition | Block import until a founding residency entry exists | Per-sale manual stamping for pre-history | Makes `residency_on`'s undefined region unreachable instead of patched row-by-row. |
| Filing status | Per-year field (year-end/elected) | Fixed once; mid-year split | It changes between years; a mid-year split is a rare edge taken out of scope rather than silently mis-modeled. |
| Platform list | Suggestions, not a constraint | Enforced enum | Platforms are free-text by HLD non-goal; the list only aids entry. |
| Symbol→ticker aliases | A small config map (identity default), consumed by `sheets-view` | Hardcode in `sheets-view`; per-event ticker | Reference data the owner edits, alongside platforms; keeps `sheets-view` free of stored config. |
| Symbol→display names | A sibling config map (ticker fallback), consumed by the TUI's name columns | Live `GOOGLEFINANCE` name lookups; hardcode in the TUI | Names are static owner-edited reference data exactly like aliases; a live lookup adds an oracle dependency, and the ticker fallback degrades honestly. |
| Refresh procedure | A human-run runbook + a coded staleness reminder | Build a bracket scraper | Brackets change yearly and need human judgment from primary sources; only the *nudge* is worth coding. |
| Tab persistence shape | The whole domain config persists as one validated JSON cell (`'Tax Rules'!A1`) through the runtime adapter, for now | Per-tab human-readable schemas across the three named tabs (Tax Rules / Residency / Platforms & Aliases) | One serde round-trip at a single seam keeps validation whole-config and atomic; the human-readable per-tab schema is deferred until the owner actually edits config in-sheet. |

## Open Questions & Future Decisions

### Deferred
1. **Staleness threshold & cadence.** Confirm 12 months and the lookback width; whether the
   reminder also fires on a new tax year regardless of age.
2. **Multi-state / multi-status mid-year.** A residency or filing-status change mid-year (both
   currently one-per-year) — revisit with `tax` if it matters.

## References

- HLD: `docs/high-level-design.md` (config holds brackets/income/residency/platforms/settings).
- Downstream consumer: `docs/intent/tax/tax-design.md` (`T_J` inputs, de-minimis, residency).
- Refresh procedure: `docs/runbooks/refresh-tax-brackets.md` (the human-run maintenance task).
