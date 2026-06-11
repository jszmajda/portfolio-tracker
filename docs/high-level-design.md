# High-Level Design: portfolio-tracker

## Problem

A single owner-operator tracks a personal portfolio of equities and vested RSUs held across
several trading platforms. Today this lives in a hand-wired Google Sheet with three coupled
tabs — **Positions** (a per-symbol rollup), **Stock Actions** (the real ledger: one row per
Buy/Vest/Sell tranche, with cost basis, remaining shares, and sale-side columns), and
**Stock Sales** (one row per sale, referencing a tranche and computing tax).

The spreadsheet works but is tedious and fragile to maintain over time:

- Every position number is a hand-written `SUMIF`/`QUERY` formula over the ledger; adding or
  correcting activity means editing formulas across tabs.
- There is no clean entry path — recording a purchase or sale is manual cell editing.
- Tax is a derived column. Each sale's rate is **manually chosen** (a `UsedTaxRate` override)
  next to a derived long-term/short-term reference rate, and there is no notion of a tax
  accrual that must be set aside, allocated to an account, moved once the sale clears, and
  finally paid to the IRS year by year.
- State tax is not modeled at all, even though the owner moves between states and what is
  owed depends on residency at the time of each sale.
- A separate shell script (the owner's `portfolio-summary.sh` daily-summary scraper) scrapes
  the Sheet's computed cells to print a daily terminal summary — brittle, coupled to cell
  ranges, and duplicating logic that belongs in one place.

The cost is ongoing maintenance friction and a tax model that has outgrown a spreadsheet.

## Approach

A formally verified accounting kernel sits between a fast terminal entry surface and a Google
Sheets workbook that serves as both the canonical store and the live view. The spreadsheet
stays — the formulas and the manual wiring do not.

### Event-sourced verified kernel

Activity is an **append-only event log** — Buy, Vest, Sell, and corporate actions such as
stock splits — each carrying platform and an optional tracking code for the tranche where
applicable. Deterministic replay of the log, combined with configuration, produces the
current positions, realized/unrealized P&L, tax-lot cost basis, and tax accruals. The pure accounting and tax math is written inside a `verus!{}` module —
proven under Verus, erased under stable `cargo build` — with `#[cfg(kani)]` bounded harnesses
alongside, exactly as in a prior private verified-Rust project. The kernel is storage-agnostic: it replays an event
log regardless of where the log is stored, so serialization and all I/O sit *outside* the
verified boundary as a single trust seam.

### Google Sheets as canonical store and view

The workbook holds two kinds of tabs. **Event-log tabs** are append-only and canonical — the
TUI writes new activity through the kernel, which validates it and appends a row; rows are
never edited in place. **View tabs** are projections the kernel regenerates — clean,
filterable positions and reports, with a `GOOGLEFINANCE` column supplying live marks. Because
Google owns durability and backup, and the owner is the only writer, the spreadsheet can be
the source of truth without the usual remote-store hazards. A **local cache** mirrors the
event log for fast reads, replay, and offline work; the cache carries no truth and is
rebuildable from the workbook at any time.

### TUI as the entry surface

A terminal UI is where activity is entered as it happens — purchases, sales, tax-accrual
actions, and residency/config changes — and where reports are viewed. Entry goes through the
kernel, which appends to the canonical log and refreshes the cache and view tabs.

### Tax as a calculated, first-class lifecycle

Each sale generates a **tax accrual** — a real entity with an explicit lifecycle (accrued →
allocated → moved → paid). Tax is **calculated**, not typed: from configured annual income and
the federal and state tax brackets in effect, long-term vs short-term by holding period, with
the accrual's **state stamped on the transaction** (defaulted from current-residency config,
overridable per transaction so intra-year moves are exact). Bracket tables are configuration,
refreshed periodically as a maintenance task; the system reminds the owner when the tables
have gone stale. Accruals settle into one or more **reserve ledgers** (a federal reserve and a
per-state reserve — each simply a ledger with a balance), roll up into year-by-year totals,
and feed a quarterly sales report for estimated-tax filing.

## Target Users

A single technical owner-operator (the author) tracking a personal multi-platform equity and
RSU portfolio, who relocates between states over time. Their needs: a fast, low-friction entry
loop that does not depend on a broker integration; trustworthy tax-lot accounting they can
stake real money on; a residency-aware view of tax owed and set aside; and a clean live view
to glance at and filter. They are comfortable in a terminal and value correctness guarantees
over feature breadth.

## Goals

- **Low-friction entry.** Recording a purchase or sale in the TUI is a few keystrokes and
  needs no broker integration or manual formula editing.
- **One source of truth.** All *positions, P&L, and tax* state derives from one append-only event
  log in the workbook; view tabs, reports, and the local cache are projections of it. (`config` is
  the separate, deliberately-independent *rules* input — brackets, income, residency — not a
  projection of the log.)
- **Verified money math.** Share/cost conservation, FIFO realized-P&L, tax-calculation bounds,
  and tax-accrual-lifecycle invariants are proven (Verus) and bounded-checked (Kani), gated in
  CI.
- **Faithful migration.** The full existing history imports such that migrated positions
  reproduce the old Sheet's realized and unrealized figures within rounding.
- **Residency-aware, calculated tax.** Each sale's federal and state tax is computed from
  configured income and brackets, the holding period, and the state stamped on the
  transaction; accruals are tracked through to paid; the owner is reminded when bracket tables
  go stale.
- **Quarterly reporting.** A quarterly sales report sufficient for estimated-tax filing is
  generated on demand, and reconciles against the accruals created that quarter.
- **Replaces the daily script.** A headless summary command supersedes
  `portfolio-summary.sh`, with day-over-day deltas, consumable by the `dailies` framework.

## Non-Goals

- **No broker/platform integrations.** Platform and tracking code are free-text metadata the
  owner enters; the system never connects to a trading platform.
- **Not a trade-execution system.** It records activity that happened elsewhere; it places no
  orders.
- **Not tax-filing software and not tax advice.** It produces a reporting worksheet; it does
  not file returns or assert correctness against tax law.
- **Single user, single machine.** No multi-user access, no concurrent writers, no hosted
  service.
- **No hand-editing of canonical data.** The event-log tabs are append-only; entry and
  correction happen only through the TUI, as new events.
- **Not a real-time market terminal.** Live marks are `GOOGLEFINANCE`-delayed and best-effort.

## Tenets

Ordered by precedence — when two conflict, the higher one wins.

- **The event log is the truth; everything else is a projection.** Positions, reports, view
  tabs, and the local cache all derive from the append-only log; none is ever edited to change
  state. When a projection disagrees with the log, the log wins and the projection is rebuilt.
- **Verify the money math; trust the I/O.** Formal verification covers the pure accounting and
  tax kernel; serialization, Sheets, and the TUI stay outside the proof and are guarded by a
  single structural trust boundary rather than by proofs of their own.
- **Append, never mutate, the ledger.** Corrections are new compensating events, not edits to
  history — so replay stays deterministic and prior-year tax records remain immutable.
- **Calculate tax and show the work.** Tax is derived from configured income and brackets and
  the calculation is always shown; a manual override is a logged exception, not the norm.

## System Design

Design tree: one HLD over leaf LLDs, with `tui` promoted to a **sub-HLD** (children `entry`,
`views`). Components and their EARS prefixes:

| Segment | Prefix | Role |
|---|---|---|
| ledger-core | `LEDGER` | Verified event-sourced accounting kernel: activity events, tax lots, FIFO cost basis, realized/unrealized P&L, share/cost conservation, deterministic replay. Verus + Kani. |
| tax | `TAX` | Verified tax calculation (federal + state, LT/ST by holding period, bracket+income based, per-transaction state) and the accrual lifecycle state machine (accrued → allocated → moved → paid) over reserve ledgers, with year roll-ups. |
| store | `STORE` | Google Sheets append-only event-log persistence, the serde trust boundary (Sheets row ↔ event), and the rebuildable local event-log cache. The single seam outside `verus!{}`. |
| runtime | `RUNTIME` | The shared host: the low-level Sheets-access layer, the cross-process advisory write-lock, and the replay → project → read-marks → cache orchestration cycle (incl. the per-symbol → trading-day quote-epoch reduction). Used by `tui`, `summary`, `import`. |
| config | `CONFIG` | Annual income, federal + per-state tax-bracket tables (per year), bracket-staleness reminder, current-residency default, platform list, workbook/credential settings, and the config-edit interface. |
| sheets-view | `SHEET` | Projector writing the clean read-only filterable view tabs; `GOOGLEFINANCE` price column; reading live marks back for valuation. |
| reports | `REPORT` | Pure portfolio analytics: portfolio composition and value-over-time, plus the daily snapshot history they read. (The quarterly/annual *tax* reports are owned by `tax`.) |
| summary | `SUMMARY` | Headless daily-summary command with day-over-day deltas and snapshot rotation; the `dailies` replacement. |
| tui | `TUI` | Terminal UI: entry flows (buy/vest/sell, accrual actions, residency/config) and rendering of reports. |
| import | `IMPORT` | One-time migration from the legacy workbook into the new event-log tabs. |

```
          +-----------------------------------+
          |               TUI                 |
          |   entry  ·  reports  ·  config     |
          +------------------+----------------+
                             | append event / read snapshot
                             v
   +-------------+   +-----------------------------+
   |   config    |-->|   ledger-core + tax kernel  |
   | income, tax |   |   (verified · verus/kani)   |
   | brackets,   |   |   replay(log)+cfg -> snapshot|
   | residency,  |   +--+---------+-------------+---+
   | platforms   |      |         |             |
   +-------------+      |events   |snapshot     |snapshot
                        v         v             v
                +-------------+ +----------+ +-----------------+
                | local cache | | reports  | | sheets-view     |
                | (rebuildable| | comp.,   | | projector       |
                |  mirror)    | | value/t, | | writes view tabs|
                +------+------+ | quarterly| +--------+--------+
                       |        +----+-----+          |
                       |             |                |
                       |             v                |
                       |        summary CLI --> dailies|
                       |                               |
   +-------------------v-------------------------------v-----------+
   |                   Google Sheets workbook (CANONICAL)          |
   |   +------------------------+    +-------------------------+   |
   |   |   event-log tabs       |    |   view tabs (projected) |   |
   |   |   append-only, TRUTH   |    |   positions, filterable,|   |
   |   |   (written via kernel) |    |   GOOGLEFINANCE marks   |   |
   |   +------------------------+    +-----------+-------------+   |
   +-------------------^-------------------------|-----------------+
                       | import (one-time)       | live marks read back
                       |                         | (reports / summary)
              +--------+-------+
              | legacy workbook|
              +----------------+
```

The verified kernel (`ledger-core` + `tax`) is pure: it replays the event log plus config and
produces a snapshot. `store` is the only component that serializes — its Sheets-row ↔ event
conversion is the trust boundary, guarded by a drift test as in the prior verified project. The event-log tabs are
canonical; the local cache mirrors them for fast/offline reads and is rebuilt from the workbook
when in doubt. The Sheet's view tabs are regenerated from the snapshot and read back only for
live marks; the system never persists live prices as truth, only daily snapshots (owned by
`summary`) that give value-over-time its history.

## Key Design Decisions

- **Google Sheets is the source of truth — append-only event-log tabs written through by the
  TUI, plus projected view tabs — with a rebuildable local cache.** Considered a local SQLite
  event log as canonical (the Sheet a pure published view) and keeping the legacy
  formula-driven Sheet. The sole-operator context removes the unverified-concurrent-writer risk
  that normally makes a remote canonical store dangerous; keeping Sheets canonical means Google
  owns backup and durability, `GOOGLEFINANCE` needs no separate price source, and there is no
  store-to-store sync to keep correct. Append-only rows preserve replay determinism and
  immutable history; the cache carries no truth, so losing it costs nothing.
- **Tax is calculated from configured income and bracket tables, not entered per sale.**
  Considered keeping the legacy manual per-sale rate. Calculation from brackets is correct by
  construction and auditable; the cost is maintaining bracket tables, mitigated by treating
  them as versioned config refreshed on a maintenance cadence with a staleness reminder. A
  manual override remains available as a logged exception.
- **Verified pure kernel over an append-only log, with the serde trust boundary outside
  `verus!{}`.** Mirrors the prior verified project directly: the `verus!{}` code is the executable code, erased
  under stable Rust; a total structural conversion (Sheets row ↔ event) is the only unproven
  seam, locked by a drift-guard test. Considered verifying nothing (the money math is the whole
  point) and verifying the whole application (Sheets/TUI are not amenable and not worth it).
- **Federal + state tax with the accrual state stamped on each transaction.** Considered
  federal-only and resolving state purely from a residency timeline at compute time. The owner
  relocates, and intra-year moves must be exact, so the authoritative state lives on the
  transaction (defaulted from current-residency config). DC and NJ brackets are seeded first.
- **Tax accrual is a first-class entity with an explicit lifecycle over reserve ledgers.**
  Today tax is a derived column. The owner's real workflow has distinct steps — set aside on
  sale, allocate to an account, move once the sale clears, mark paid by year — so an accrual is
  a state machine, and the reserve it settles into is modeled as a ledger with a balance.
- **RSU vesting establishes a cost-basis lot at fair market value; sell-to-cover is an ordinary
  sale at vest-date price.** A `Vest` event creates a lot whose basis is the FMV at vest (the
  amount already taxed as ordinary income on the W-2), and whose holding-period clock starts at
  vest; a sell-to-cover is a `Sell` against that lot at vest-date price, producing ≈ zero
  capital gain, and later disposals compute gain against the FMV basis. For RSUs this is the
  IRS-mandated treatment, not one option among several — the legacy $0-basis treatment is the
  classic double-taxation error (it re-taxes income already taxed at vest). The genuine open
  choice is *lot selection* on a sale — FIFO vs specific-identification — which is a
  `ledger-core` LLD decision, not a basis question. This decision changes how vested lots
  migrate.
- **Corporate actions are first-class events that transform existing lots.** A stock split
  multiplies share counts and divides per-share basis across the affected symbol's open lots,
  preserving total basis and total economic value; the holding period is unbroken. Considered
  baking split-adjustment into the importer only (insufficient — splits happen on an ongoing
  basis, not just in historical data). Splits are the first corporate action modeled; mergers,
  spin-offs, and dividend income are open questions for the `ledger-core` LLD.
- **Full-history migration via a one-time importer into a new workbook.** Open-positions-only
  and fresh-start were considered; both lose cost-basis and prior-year tax history that current
  sales and reports depend on.

## Success Metrics

- Recording a buy/vest/sell in the TUI is a few keystrokes and needs no broker integration.
- After migration, every open position's realized and unrealized P&L matches the legacy Sheet
  within rounding. **Falsification:** a migrated position diverges materially from the old
  numbers.
- Verus and Kani gates pass in CI for conservation, FIFO realized-P&L, tax-calculation bounds,
  and accrual-lifecycle invariants. **Falsification:** replay is non-deterministic, share/cost
  conservation fails, or an accrual can be paid twice or transition backward.
- The quarterly sales report reconciles: the tax it reports for a quarter equals the sum of
  accruals generated by that quarter's sales.
- The `dailies` framework consumes the new summary command and the legacy
  `portfolio-summary.sh` is retired.

## FAQ

**Why keep Google Sheets as the store?** It is the one surface that is glanceable from
anywhere, trivially filterable, already trusted for live prices via `GOOGLEFINANCE`, and backed
up and durable without any work on the owner's part. With a single writer, it can be canonical
without the hazards a shared remote store would carry — so it earns the role rather than being
demoted to a copy.

**Can I still fix a mistake?** Yes — through the TUI, as a new compensating event. The event-log
tabs are never edited in place, which is what keeps prior-year tax records and replay
trustworthy.

**What happens if I'm offline?** Reads work from the local cache; current valuation falls back
to the last cached marks. Entry is write-through with read-back verification: on submit, the
TUI appends to the workbook and confirms the row by reading it back before closing out the
activity; if the workbook is unreachable, control returns to the owner to retry later. There is
no background sync queue — an activity is either durably recorded or still in the owner's hands.

## References

- Legacy workbook: the owner's hand-wired Google Sheet (id in the gitignored
  `import.local.json` owner-inputs file; see `import.local.example.json`), tabs
  Positions / Stock Actions / Stock Sales. Read via the `sheets` CLI with the service-account
  credentials configured in the gitignored `config.local.toml` (see `config.local.example.toml`).
- Daily summary being replaced: the owner's `portfolio-summary.sh` scraper.
- Verification idioms mirrored from a prior private verified-Rust project (`portfolio-engine-core` Verus kernel,
  `#[cfg(kani)]` harnesses, serde-outside-`verus!{}` trust boundary, drift-guard test).
- Tax-bracket sources refreshed as a maintenance task: IRS publications (federal) and the
  revenue departments of the owner's states.
- RSU basis treatment (FMV at vest = ordinary income = capital-gains basis; holding period from
  vest; $0-basis 1099-B as the double-taxation pitfall): IRS Form 8949 guidance, corroborated by
  Charles Schwab and TurboTax RSU tax explainers.
