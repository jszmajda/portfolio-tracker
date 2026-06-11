---
parent: high-level-design
prefix: TAX
---

# tax

## Context and Design Philosophy

`tax` turns realized gains into money set aside and, eventually, paid. It owns three things:
**tax calculation** (federal + state, long-term vs short-term, from configured brackets and
income), the **accrual lifecycle** (accrued → allocated → moved → paid), and **reserve
ledgers** plus the **quarterly/annual reporting** that ride on top. It consumes
`ledger-core`'s `RealizedGain` records and `config`'s bracket/income/residency data; it owns no
share or cost-basis math.

It follows the same discipline as `ledger-core`: the calculation and lifecycle math is a pure,
verified kernel (`verus!{}` + Kani), integer `Cents` throughout, append-only. The accrual
*amounts* are **computed** (a pure function of the gain and the year's config — "calculate and
show the work"); the accrual *lifecycle* is **event-sourced** via a `TaxEvent` log, so the
human's allocate/move/pay actions are durable, ordered, and auditable.

Guiding principles:

- **Calculate, don't type.** Tax is derived from configured income and brackets; a manual
  override is a logged absolute-amount `TaxEvent`, not the default path (HLD tenet 4).
- **Estimates stay live; actuals are recorded.** A pre-payment accrual amount tracks the
  current income/bracket estimate; the amount actually *moved* and *paid* is recorded on its
  event and never recomputed.
- **The reserve is a ledger with a balance.** Each (jurisdiction, year) reserve balance is
  `Σ moved − Σ paid` of real money; adequacy versus the computed estimate is a report line, not
  an enforced constraint.

## Inputs

- **From `ledger-core`:** `RealizedGain { sale_id, sale_seq, lot_id, symbol, sale_date,
  proceeds_cents, basis_cents, gain_cents, acquire_date, holding_days, accrues_to_state }` — one
  per consumed (lot, qty) pair, orderable by `(sale_date, sale_seq)`; `(sale_id, lot_id)` is its
  stable identity. Plus, for the unrealized estimate, the Snapshot's **per-lot unrealized**
  (kernel-computed, so `tax` adds no second rounding site). `tax` aggregates these.
- **From `config`:** annual ordinary income (per year), federal ordinary brackets, federal
  long-term capital-gains brackets, the NIIT rate and MAGI threshold, per-state ordinary brackets
  (DC, NJ to start), the current-residency default, the de-minimis threshold, and each bracket
  set's **`BracketState`** (`Verified | Stale | NoBracketsAvailable`). All per-year, versioned.
- **From `runtime`:** the cached marks and the "as-of today" date for the unrealized estimate
  (the kernel is clockless).
- **The `TaxEvent` log:** the append-only lifecycle stream (persisted in Sheets via `store`,
  like `LedgerEvent`s), folded in `Seq` order.

## Tax Calculation

**Long-term vs short-term** is decided from calendar dates, not `holding_days`: a gain is
**long-term iff `sale_date` is strictly after the first anniversary of `acquire_date`**, where
the anniversary is the same month/day one year later, and a Feb-29 acquisition takes **Mar 1**
as its anniversary (the next existing day). Otherwise short-term. Calendar dates make leap years
exact; `holding_days` is informational. `tax_year` of a gain is the **calendar year of
`sale_date`** (the trade date), which also drives the reserve and report partitions.

A jurisdiction's annual tax is a pure, total function `T_J(ordinary_income, st_gains, lt_gains)
→ Cents`, defined for all integer inputs:

- **Federal.** Short-term gains stack on ordinary income at the federal ordinary brackets (ST is
  taxed as ordinary income). Long-term gains stack *above* `income + st` at the preferential LT
  brackets (0 / 15 / 20%) — short-term gains raise the bracket floor the long-term gains begin at.
  **NIIT** adds 3.8% on the investment gain: the amount above the MAGI threshold is
  `max(0, income + (st + lt) − magi_threshold)` (so ordinary income counts toward MAGI and can
  push gains past the threshold), and the rate applies to the **lesser of** that amount and the
  total gain `st + lt`, so NIIT never taxes more than the gains. (`T_fed` composes the three.)
- **State (DC, NJ).** Both tax capital gains as ordinary income, so `T_state` applies the
  state's ordinary brackets to *all* gains (LT and ST alike), stacked on state income.

The gain arguments enter the bracket stack clamped at zero (`max(0, ·)`), so `T_J` is total over
negative inputs and a year that is net-negative so far contributes zero gain-tax (losses net
against later gains within the year up to zero; the $3,000-ordinary-offset and carryover are
deferred).

**A single gain's accrual is the marginal increment it adds, stacked in chronological
`(sale_date, sale_seq)` order** — not raw `Seq`. For gain `g` with year-to-date gains realized
*chronologically before* it (`st_before`, `lt_before`):

```
accrual(g) = T_J(income, max(0, st_before + st(g)), max(0, lt_before + lt(g)))
           − T_J(income, max(0, st_before),         max(0, lt_before))
```

Chronological stacking makes a back-dated entry reprice consistently (later-dated accruals are
re-derived when an earlier-dated gain is inserted — accruals are computed, not frozen until
Move/Pay), keeps the quarterly buckets (also `sale_date`-based) coherent, and attributes NIIT to
the gains that chronologically cross the threshold. A loss yields a negative increment.

A `TaxEvent::AmountOverride { accrual_key, applied_amount_cents, reason }` substitutes an
**absolute amount** for one accrual's computed increment (the computed value has no single
"rate" under stacking + NIIT). Both the derived increment and the applied amount are retained.

## Jurisdiction Resolution & Bracket State

- **`accrues_to_state` → jurisdiction.** `tax` maps a `RealizedGain`'s `accrues_to_state` (and
  Federal, always) to configured jurisdictions, **validating** each against the configured states.
  A gain stamped with a state that has no configured brackets resolves to `NoBracketsAvailable`
  for the state portion (Federal still computes); a missing/blank stamp falls back to the
  current-residency default and is flagged.
- **Bracket state is carried, not dropped.** Every accrual, reserve figure, and unrealized
  estimate `tax` emits carries the per-jurisdiction `BracketState` (`Verified | Stale |
  NoBracketsAvailable`) from `config`, so `summary`/`tui` render `[est]` / `[est, brackets stale]`
  / `n/a (no brackets)` from one threaded signal.
- **`NoBracketsAvailable` never silently zeroes.** When a jurisdiction's brackets are absent, that
  jurisdiction's accrual/estimate is reported **unavailable** (degraded), not `0` — so a confident
  zero is never computed against missing tax tables.

## Unrealized Tax Estimate

Beyond realized accruals, `tax` exposes an **estimate** of the tax an *open* position would incur
if sold today — for the live post-tax view (`sheets-view`) and reports. Given a symbol's open lots
with their **kernel-computed per-lot unrealized** (from the Snapshot — `tax` does no re-scaling)
and the **as-of-today date** (`runtime` supplies it; the kernel is clockless), it treats the
position as hypothetically sold today: each lot is classified LT/ST by its holding **as of
today**, and the estimated tax is the same `T_J` marginal increment over the year's realized YTD
gains (federal + the gain's resolved state + NIIT). From it `tax` derives a per-position
**effective unrealized tax rate**, expressed in **`ppm`** (matching config's rate units, so no
float crosses to `sheets-view`) = `round(estimated_tax × 1e6 / unrealized_pretax)` (0 when
unrealized ≤ 0).

This is an **estimate, not an accrual**: it creates no accrual, has no lifecycle, emits no event,
and is recomputed each cycle. `sheets-view` uses the effective rate for its live post-tax column;
the Tax tab and TUI may show the exact estimated tax. When the position's mark is degraded, or its
state resolves to `NoBracketsAvailable`, the estimate is **unavailable** (degraded), never zero.

## Accrual Model & Lifecycle

An **accrual** is keyed by `(sale_id, lot_id, jurisdiction, tax_year)` — **one per
`RealizedGain` per jurisdiction**, so each is unambiguously long- or short-term. Its amount is
computed (above); its **state** is derived by folding the `TaxEvent` log:

```
Accrued  ──Allocate──▶  Allocated  ──Move──▶  Moved  ──Pay──▶  Paid
```

- **Accrued** — the default the moment its `RealizedGain` exists; no event needed.
- **Allocated** — `TaxEvent::Allocate { accrual_key, account_label }` records which reserve
  account it belongs to; it may stamp the then-current computed estimate for audit. Re-Allocate
  before Pay is permitted (last-write-wins on `account_label`).
- **Moved** — `TaxEvent::Move { accrual_key, amount_cents, date }` records the **actual money**
  moved into the reserve (after the sale clears). `amount_cents` need not equal the computed
  accrual; the difference is surfaced as funding shortfall/surplus, not rejected.
- **Paid** — `TaxEvent::Pay { jurisdiction, tax_year, period, amount_cents, date, covers:
  [accrual_key] }` records a remittance and marks the covered accruals Paid. One Pay (e.g. a
  quarterly estimated payment) may cover many accruals, but every `covers` key must match the
  Pay's `jurisdiction` and `tax_year`; `amount_cents` is the authoritative actual remitted.

**Migration accrual (seeding only).** For a closed prior year, `tax` also accepts a single
**combined migration accrual** keyed by `(jurisdiction, tax_year)` with no backing `RealizedGain`
— `import` seeds it (overridden to the legacy actual, run `Allocate → Move → Pay`). For a year that
has one, the per-`RealizedGain` accruals are **superseded** (excluded from outstanding) so the
year reconciles to the legacy actual rather than the recomputed-but-uncollectible figure. This is
a migration-only construct; live years stay per-`RealizedGain`.

Transitions are **forward-only** and validated at append time (a `TaxError` mirrors
`LedgerError`'s shape): you cannot Move an unallocated accrual, Pay an unmoved one, or pay an
accrual twice; a Pay covering a mismatched jurisdiction/year is rejected. A `TaxEvent` whose
`accrual_key` has no current `RealizedGain` (its sale was reversed in `ledger-core`) folds as a
**no-op**, surfaced as an orphan warning — so replay stays total. An accrual still `Accrued`
when its gain disappears simply drops; if it was already Moved/Paid, its `TaxEvent`s fold as
no-ops and the now-backless reserve entry is surfaced as an orphan warning for manual unwind —
`ledger-core` cannot see tax state, so it does not block the reversal (the cross-segment
detection rule is deferred to the Phase 4 edge audit).

**De-minimis.** An accrual whose `|amount|` is below the configured de-minimis threshold (e.g. a
sell-to-cover's rounding-epsilon gain) is auto-settled — it needs no lifecycle action and is
excluded from outstanding-balance prompts.

## Reserve Ledgers

A reserve exists per `(jurisdiction, tax_year)` and is a derived balance of real money:

```
reserve(J, year) = Σ Move.amount_cents − Σ Pay.amount_cents   (for that J, year)
```

Reserves carry no independent state — they are a projection of the `TaxEvent` log, rebuilt on
replay, and **may go negative** as an over-/under-funding signal (no enforced floor). The annual
report shows, per (jurisdiction, year): total accrued (computed), total moved, total paid,
outstanding (`accrued − paid`), and funding shortfall (`accrued − moved`).

## Quarterly & Annual Reporting

- **Quarterly sales report** groups realized gains and their accruals into the **IRS estimated
  periods** by `sale_date` — Q1 Jan 1–Mar 31, Q2 Apr 1–May 31, Q3 Jun 1–Aug 31, Q4 Sep 1–Dec 31
  — which partition the year exactly. Per period per jurisdiction: realized gain (LT/ST split),
  computed accrual, and the cumulative safe-harbor target (22.5 / 45 / 67.5 / 90%).
- **Annual report** per (jurisdiction, year): accrued / moved / paid / outstanding / shortfall,
  and an effective rate (accrual ÷ gain) shown only where `|gain|` exceeds the de-minimis
  threshold (otherwise `n/a`), with the derived-vs-applied breakdown.

## Verification Invariants

Verus-proven, Kani bounded-checked (facet `TAX-VERIF-*`).

1. **Monotonic tax.** `accrual(g)` is non-decreasing in `g` for each jurisdiction.
2. **Bounded tax.** For a positive gain over a non-negative YTD base, `0 ≤ accrual(g) ≤ g`; a
   configured rate set that would breach 100% is rejected.
3. **Total calculation.** `T_J` is total over all integer `(income, st, lt)` including negative
   gain arguments (clamped at zero), so no input panics or diverges.
4. **Lifecycle state machine.** Accrual states advance only Accrued → Allocated → Moved → Paid
   (Allocate self-loop excepted); no backward transition, no skip, no double-pay.
5. **Reserve conservation.** Every `reserve(J, year) ≡ Σ Move − Σ Pay` for that key.
6. **Quarterly partition.** The four IRS periods partition the tax year with no gap or overlap,
   so `Σ over periods (gains, accruals) ≡ annual (gains, accruals)`.
7. **Orphan totality.** Folding a `TaxEvent` whose accrual has no backing `RealizedGain` leaves
   state unchanged (no-op), so replay over any `TaxEvent` log is total.

## Trust Boundary & Interfaces

- **Inbound (from `ledger-core`):** `RealizedGain`s incl. `lot_id` (pure values; no I/O).
- **Inbound (from `config`):** versioned per-year brackets, income, NIIT params, de-minimis,
  residency default — data; freshness/staleness reminder is `config`'s job.
- **Inbound (`TaxEvent` log, via `store`):** lifecycle events; serde lives outside `verus!{}`.
- **Outbound (to `reports`, `tui`):** computed accruals, reserve balances, quarterly/annual
  reports.
- **Outbound (to `sheets-view`, `reports`):** the per-position effective unrealized tax rate and
  estimated unrealized tax (an estimate, not an accrual).

## Decisions & Alternatives

| Decision | Chosen | Alternatives Considered | Rationale |
|----------|--------|------------------------|-----------|
| YTD stacking order | Chronological `(sale_date, Seq)` | Raw `Seq` | Reports bucket by `sale_date`; chronological stacking keeps per-period accruals and NIIT attribution reproducible under any entry order. |
| Per-gain accrual basis | Marginal increment, cumulative chronological stacking | Flat marginal-rate-at-income; annual-difference only | Prices each gain at the bracket it lands in; deterministic and monotonic. |
| Accrual key | Per-`RealizedGain` `(sale_id, lot_id, J, year)` | `(sale_id, J, year)` | One Sell can mix LT and ST lots; per-gain keying keeps each accrual single-regime and lets the lifecycle/reversal address lots individually. Requires `lot_id` on `RealizedGain`. |
| Manual override | Absolute `applied_amount_cents` | A "rate" | Under stacking + NIIT there is no single rate; an absolute amount is unambiguous and bounded-checkable. |
| `T_J` over losses | Total via `max(0, ·)` gain clamping | Define only for non-negative gains | Keeps the function total and nets within-year losses against later gains; the $3k cap/carryover sits on top (deferred). |
| Amount lifetime | Computed live until Move/Pay records the actual | Freeze at accrual creation | Estimates track latest figures; actuals are recorded facts that never drift. |
| Move amount | Actual money moved (may differ from estimate) | Force equal to computed accrual | Reserves track real money; adequacy is a report line (shortfall), matching how the owner funds round amounts. |
| Reserve floor | May go negative (signal) | Reject Pay that exceeds reserve | A negative balance is a meaningful over/under-funding signal, not necessarily an error. |
| LT/ST boundary | Calendar "more than one year"; Feb-29 → Mar-1 anniversary | `holding_days > 365` | The IRS rule is calendar-based and leap-year-exact; Feb-29 needs an explicit, total convention. |
| State cap-gains treatment | State ordinary brackets on all gains (DC, NJ) | Preferential state LT rate | Confirmed: DC and NJ tax capital gains as ordinary income. |
| NIIT | Federal add-on, marginal in chronological order | Omit; proportional post-pass | Materially affects the accrual; chronological-marginal keeps the annual total exact and attribution economically defensible. |
| Unrealized tax estimate | A rate/estimate from a hypothetical sell-today (no accrual, no event); rate in `ppm`, per-lot value from the kernel, today-date from `runtime` | Omit; reuse the accrual path; re-scale in tax | The live post-tax view and reports need a forward estimate; modelling it as an accrual would pollute the lifecycle, and re-scaling in tax would duplicate the kernel's one rounding site. |
| Bracket state | Carry config's per-jurisdiction `BracketState` on every output; `NoBracketsAvailable` ⇒ unavailable, not zero | Drop it at tax; compute against empty brackets | summary/tui render from one threaded signal; a silent zero on absent brackets is the staleness the HLD says to surface. |
| Migration override | Bulk `AmountOverride` for closed-year migrated accruals is the sanctioned exception | Treat it as the tenet-4 "norm" it warns against | Legacy actuals are facts; migration seeding is the one bulk-override case, logged (derived + applied retained), distinct from live entry. |
| Quarterly periods | IRS uneven estimated periods | Calendar quarters | The report exists for estimated-tax filing, which uses these periods. |

## Open Questions & Future Decisions

### Resolved
1. ✅ Federal ST=ordinary, LT=preferential + NIIT (chronological-marginal); state=ordinary (DC, NJ).
2. ✅ Chronological-`(sale_date, Seq)` cumulative-stacking accrual; calendar LT/ST with Feb-29 → Mar-1.
3. ✅ Per-`RealizedGain` accrual key; lifecycle accrued → allocated → moved → paid, event-sourced.
4. ✅ Reserve = Σ moved − Σ paid (may be negative); Move records actuals; shortfall reported.
5. ✅ `T_J` total over signed inputs via `max(0,·)`; de-minimis auto-settle; orphan-fold no-op.
6. ✅ IRS uneven estimated-tax quarters; `tax_year = year(sale_date)`.

### Deferred
1. **Capital-loss limits & carryover.** The $3,000/yr ordinary-offset cap and loss carryover to
   later years — currently a within-year loss nets against gains down to zero, no further.
2. **Pay vs accrual granularity.** Whether aggregate quarterly `Pay` coverage needs finer
   per-payment attribution than the `covers` list.
3. **Reversing a Moved/Paid sale.** The exact unwinding flow (currently an orphan warning +
   manual unwind; cross-segment detection across the ledger/tax boundary deferred to Phase 4).
4. **State part-year / source rules.** Beyond residency-at-sale stamping.

### Out of scope
- **Wash sales** and other federal basis adjustments (per `ledger-core`).

## References

- HLD: `docs/high-level-design.md` (calculated tax, residency-stamped accruals, lifecycle).
- Upstream: `docs/intent/ledger-core/ledger-core-design.md` (`RealizedGain` contract — requires
  `lot_id`).
- Tax structure confirmed: IRS estimated-tax periods; DC and NJ capital-gains-as-ordinary-income
  (bracket numbers refreshed as a `config` maintenance task).
