---
parent: high-level-design
prefix: LEDGER
---

# ledger-core

## Context and Design Philosophy

`ledger-core` is the verified accounting spine. It owns the **event taxonomy**, the **tax-lot**
model, **deterministic replay**, and the **realized/unrealized P&L** math — and nothing else.
Tax calculation, the accrual lifecycle, persistence, Sheets, and the TUI all live outside it.

The executable code lives inside a `verus!{}` module that erases under stable `cargo build` and
is proven under the pinned Verus toolchain, with `#[cfg(kani)]` bounded harnesses alongside. Serde and all I/O sit *outside* the verified
boundary — `ledger-core` takes an already-deserialized `Vec<LedgerEvent>` and returns a
`Snapshot`; it never reads a file, a socket, or a spreadsheet.

Guiding principles:

- **Pure and total.** `replay(log, marks) → Snapshot` is a deterministic pure function. The
  same log always produces the same snapshot. No clock, no randomness, no I/O.
- **Integer money and quantities.** All amounts are exact integers (`Cents`, `MicroShares`);
  no floating point survives past the input boundary. This is what makes the math verifiable.
- **Append-only.** Replay never mutates past events. Corrections are new events (see Reversal).
- **Validated in, total out.** An event is validated *before* it is appended to the log, so a
  stored log is valid by construction and `replay` over it is total — it never encounters an
  event it must reject.

## Money & Quantity Types

| Type | Representation | Notes |
|---|---|---|
| `Cents` | `i64` | Exact money, hundredths of a dollar. Prices are cents **per whole share**. Negative allowed (losses, reversals). |
| `MicroShares` | `i64` | Share quantity at 1e-6 granularity — supports fractional shares and split ratios exactly. |
| `Date` | `i32` | Days since the Unix epoch. Acquisition/sale dates and holding-period math. Leap years are exact because the unit is days. |
| `Seq` | `u64` | Monotonic per-log sequence number; the **total order** events fold in. |
| `Symbol` | interned `String` | Ticker, e.g. `AMZN`. Case-normalized at the input boundary. |
| `LotId` | `String` | Stable, unique-per-log identifier for a tax lot (used to designate lots at sale time). |
| `EventId` | `String` | Stable identifier for an event (referenced by a Reversal). |

**The scale rule.** Because a price is cents-per-*whole*-share and a quantity is *micro*-shares,
every `quantity × price` product is in units of `cents × 1e6` and must be divided by `1_000_000`
to land in `Cents`:

```
value_cents = round_half_to_even( qty_micro × price_cents / 1_000_000 )   // computed in i128
```

This single conversion (`SHARE_SCALE = 1_000_000`) is the only place rounding enters monetary
amounts, and it is applied once when a value crosses into `Cents` (lot basis at open, gross
proceeds at sale, unrealized mark value). Fees are already in `Cents` and are added *after* the
scale.

**Overflow contract.** Every stored monetary amount is bounded by `MONEY_CAP` (≈ 2^40 cents ≈
$11B) — far above the project's ~$100M operational ceiling, so the bound is a totality device
for Verus, never hit in practice. Products are computed in `i128` (which cannot overflow at
these magnitudes), scaled down, and then the **final scaled `Cents`/`MicroShares` result** is
range-checked against `MONEY_CAP`; a result outside the bound is a rejection
(`LedgerError::AmountOutOfRange`), never a wrap. The pre-scale `i128` product is not itself
bound-checked.

## Event Taxonomy & Ordering

A `LedgerEvent` carries an `EventId`, a `Seq`, an event `Date`, and one kind. **Replay folds in
ascending `Seq` order; `Date` is data, never a sort key** — so same-date events (a vest and its
same-day sell-to-cover, a split and a same-day sale) have a defined, deterministic order.
`platform` (free text) and `tracking_code` (`Option<String>`) are tranche metadata recorded on
acquisitions and sales; `ledger-core` stores them but interprets only what the math needs.

- **`Buy { lot_id, symbol, qty, unit_price_cents, fees_cents, platform, tracking_code }`** —
  opens a lot. `total_basis = scale(qty × unit_price) + fees`. Holding clock starts at the
  event date.
- **`Vest { lot_id, symbol, qty, fmv_per_share_cents, platform, tracking_code }`** — opens a lot
  whose `total_basis = scale(qty × fmv_per_share)` (the fair-market value already taxed as
  ordinary income at vest); the holding clock starts at the vest date. A sell-to-cover is just a
  `Sell` at the vest-date price against this lot. (No fee field — any sell-to-cover commission
  is a fee on the `Sell`.)
- **`Sell { sale_id, symbol, qty, unit_price_cents, fees_cents, lot_refs, accrues_to_state,
  platform, tracking_code }`** — disposes `qty` shares. `lot_refs` is either an explicit list of
  `(lot_id, qty)` (specific-identification) or empty (FIFO fallback). `accrues_to_state` is
  stored for `tax`; `ledger-core` does not read it. Produces one `RealizedGain` per consumed
  (lot, qty) pair.
- **`Split { symbol, ratio_num, ratio_den }`** — a `ratio_num : ratio_den` split (`ratio_num ≥ 1`,
  `ratio_den ≥ 1`). Each open lot of `symbol` gets `remaining_qty := round_half_to_even(
  remaining_qty × ratio_num / ratio_den)` (i128); **`remaining_basis` is left untouched**, so
  per-share basis adjusts and total basis is conserved exactly. The holding period is unbroken.
- **`Reversal { target_event_id }`** — the append-only correction mechanism. Replay is defined as
  folding the log **with the target event filtered out** ("as if it never occurred"); the
  corrected value is supplied by a fresh follow-on event. **Append-time validation is
  backward-looking** (see Reversal validation below): a Reversal is rejected unless the target is
  a real, not-yet-reversed event on which *no already-appended, surviving event depends*.

Mergers, spin-offs, dividend income, and wash sales are out of scope for this leaf (Open
Questions / explicitly out of scope).

## Tax Lots & Lot Selection

A **Lot** is the unit of cost basis:

```
Lot { id, symbol, acquire_date, open_seq, source: Buy|Vest,
      remaining_qty: MicroShares, remaining_basis_cents: Cents,
      platform, tracking_code }
```

- `remaining_basis_cents` is the source of truth for basis; it only decreases (via Sell) and is
  **never rescaled by a Split**. `remaining_qty` decreases (Sell) or rescales (Split).
- **Consuming `c` shares** from a lot realizes `consumed_basis = round_half_to_even(
  remaining_basis × c / remaining_qty)` (via `i128`), then `remaining_basis −= consumed_basis`,
  `remaining_qty −= c`. **When a Sell consumes a lot's last share** (`c == remaining_qty`),
  `consumed_basis = remaining_basis` exactly (residual swept) — so a fully-consumed lot
  transfers its entire basis with zero rounding loss.
- **Specific-identification** is the primary mode: a `Sell` names exact `(lot_id, qty)` pairs.
  Designated quantities must sum to the sale `qty`, each named lot must exist for `symbol` with
  sufficient `remaining_qty`, no lot may be named twice in one Sell, and every named lot must
  reside on the sale's `platform`.
- **FIFO** is the fallback when `lot_refs` is empty: consume open lots of `symbol` (on the sale's
  platform) in ascending **`(acquire_date, open_seq)`** order — `open_seq` breaks same-date ties
  so selection is deterministic. If total open shares on that platform are fewer than the sale
  `qty`, the Sell is rejected — there are no short sales.

## Replay, Validation & Error Model

Validation runs at **append time**, against the state replayed so far (the prefix of accepted
events, with any reversed targets filtered out). A candidate event is either accepted (appended)
or rejected with a `LedgerError`; rejection leaves the log and state untouched (no partial
mutation). Because of this, `replay` over a stored log is total.

`LedgerError` variants (one per rejection trigger):

- `NonPositiveQty` — a `Buy`/`Vest`/`Sell` with `qty ≤ 0`.
- `DuplicateLotId` — a `Buy`/`Vest` reusing an existing `LotId`.
- `UnknownLot` / `WrongSymbolLot` — a `Sell` naming a missing lot, or one of another symbol.
- `LotRefsMismatch` — designated quantities don't sum to the sale `qty`.
- `DuplicateLotRef` — the same `lot_id` named twice within one Sell.
- `WrongPlatform` — a designated/FIFO lot not on the sale's platform.
- `InsufficientShares` — specific-ID or FIFO can't cover the sale `qty`.
- `BadSplitRatio` — `Split` with `ratio_num < 1` or `ratio_den < 1`.
- `BadReversal` — target unknown, already reversed, or a surviving already-appended event
  depends on it (see below).
- `AmountOutOfRange` — a scaled `Cents`/`MicroShares` amount exceeds `MONEY_CAP`.

**Reversal validation (backward-looking).** A Reversal is the newest event when appended, so
there are no *later* events to consult; the dependency check looks **backward** at the
already-appended, surviving prefix. A Reversal of target `T` is rejected (`BadReversal`) if any
surviving event with lower `Seq` depends on `T`'s effect — concretely, a `Sell` that consumed a
lot `T` opened (whether named explicitly or selected by FIFO), or a `Split` that rescaled such a
lot. To reverse `T`, those dependents must be reversed first (their Reversals carry higher
`Seq`, LIFO). Given this rule, filtering `T` out on re-fold cannot present `replay` with an
event whose precondition is gone — which is exactly what makes replay total (invariant 8).

`replay(events, marks) → Snapshot` folds surviving events in `Seq` order. The output `Snapshot`:

```
Snapshot {
  positions: Map<Symbol, Position>,     // qty, total_basis, realized_pnl, unrealized (if marks)
  open_lots: Vec<Lot>,
  realized_gains: Vec<RealizedGain>,    // the interface to `tax`
}
```

`RealizedGain { sale_id, sale_seq, lot_id, symbol, sale_date, proceeds_cents, basis_cents,
gain_cents, acquire_date, holding_days, accrues_to_state }` — **one per consumed (lot, qty) pair**,
with stable identity `(sale_id, lot_id)`, so each carries its own acquisition date and holding
period (different lots → different tax treatment, keyed individually downstream by `tax`).
`sale_seq` is the Sell event's `Seq`, so `tax` can order gains chronologically by `(sale_date,
sale_seq)` (its accrual stacking depends on it).

## Realized & Unrealized P&L

- **Realized.** Gross proceeds for a Sell are `scale(qty × unit_price)`; net `proceeds = gross −
  fees`. A multi-lot Sell allocates net proceeds across consumed lots by **largest-remainder**:
  give each lot the floor of its share, then hand the signed residual units out one per lot in
  descending fractional-remainder order, ties broken by ascending `(acquire_date, open_seq)`.
  This is defined for negative totals (floor convention, signed residual) and guarantees `Σ
  per-lot proceeds ≡ net proceeds` exactly. Per lot, `gain_cents = proceeds − basis_cents`.
  Proceeds and gain **may be negative** (losses, or fees exceeding gross) — only `remaining_qty`
  and `remaining_basis_cents` carry non-negativity.
- **Sums are exact; per-lot is not proportional.** Per-lot proceeds (largest-remainder) and
  per-lot consumed basis (rounded, swept at closure) each sum exactly to their sale totals, but a
  single lot's `gain_cents` is not the exact proportional gain — it carries up to ±½ cent of
  allocation noise. Only `Σ gain` is exact; `tax` must aggregate, not assume per-lot
  proportionality. A sell-to-cover therefore yields `|gain| ≤` a rounding epsilon, **not exactly
  zero** — no invariant assumes a zero.
- **`holding_days = sale_date − acquire_date`** (both `Date` in days, so leap years are exact);
  same-day = 0; negative is impossible because a sale ordered before its lot opens is rejected.
  `ledger-core` reports `holding_days`; **the long-term/short-term threshold is a tax rule the
  `tax` leaf applies.**
- **Unrealized** requires a current mark. `replay` accepts an optional `marks: Map<Symbol,
  Cents>` (the per-whole-share mark, supplied by the caller — `runtime` — from `sheets-view`'s
  cached `GOOGLEFINANCE` marks). For a symbol with a mark, unrealized = `Σ over open lots
  (scale(mark × remaining_qty) − remaining_basis)`. Degradation is **per-symbol**: a symbol with
  no mark reports unrealized = `None` (never zero); a mark for a symbol with no open shares is
  ignored, not an error.
- **Per-lot unrealized is exposed on the Snapshot's `open_lots`** (each lot's `scale(mark ×
  remaining_qty) − remaining_basis`, or `None` when degraded), so `tax`'s unrealized-tax estimate
  can value and classify each lot **without re-implementing `scale()`** — the one rounding site
  stays inside the verified kernel.

## Conservation & Verification Invariants

Properties Verus proves and Kani bounded-checks.
**Basis is the cross-split-invariant monetary quantity** (splits never touch basis); share counts are
conserved only within a split epoch and rescaled explicitly by splits.

1. **Basis conservation (headline).** For each symbol, `Σ remaining_basis` (open lots) + `Σ
   realized basis` (consumed) ≡ `Σ acquired total_basis`. Consumption moves an exact integer
   amount open→realized; Split leaves `remaining_basis` untouched. Holds at every step, with zero
   residual at lot closure — invariant across splits.
2. **Share accounting.** Each lot's `remaining_qty` changes only by Sell (decrease by consumed
   `c`) or Split (rescale). *Within a split epoch* for a symbol, `Σ remaining_qty` + `Σ disposed
   qty` ≡ `Σ acquired qty`. A `Split` multiplies every open lot's `remaining_qty` by the ratio;
   at the aggregate, post-split `Σ remaining_qty = round(pre-split Σ × ratio_num/ratio_den)` (per
   lot rounded half-to-even). Already-disposed quantities are recorded in the frame they
   occurred and are not retroactively rescaled.
3. **Non-negativity.** Every `remaining_qty ≥ 0` and `remaining_basis_cents ≥ 0` at all times.
4. **No partial mutation on reject.** A rejected candidate event leaves log and state
   byte-identical.
5. **Replay determinism.** `replay(log, marks)` is a pure function of `(log, marks)` under the
   `Seq` total order and the `(acquire_date, open_seq)` FIFO order; identical input yields
   identical output.
6. **Realized-gain correctness.** For a Sell, `Σ RealizedGain.gain ≡ net proceeds − Σ consumed
   basis`, consumed quantities sum to the sale qty, and per-lot proceeds sum exactly to net
   proceeds.
7. **Split basis-neutrality.** A `Split` changes no lot's `remaining_basis` and changes total
   symbol basis by zero; `Σ remaining_qty` scales per the rounding rule in invariant 2.
8. **Reversal totality.** Because a Reversal is rejected when a surviving event depends on its
   target, re-folding with the target filtered out preserves every surviving event's
   precondition; `replay` therefore stays total, and shares/basis remain conserved. Kani
   obligation: Reversal-of-Split and Reversal-of-Buy with intervening Sell/Buy events.

## Trust Boundary & Interfaces

- **Inbound (from `store`):** a `Vec<LedgerEvent>` from deserializing Sheets rows. The Sheets-row
  ↔ `LedgerEvent` conversion is the single unverified seam, locked by a drift-guard test (see
  `store-design.md`, Trust Boundary & Drift Guard). Serde derives live *outside* `verus!{}`.
- **Inbound (from caller):** the optional per-whole-share `marks` map for valuation.
- **Outbound (to `tax`):** `Snapshot.realized_gains` (with `holding_days` and `accrues_to_state`);
  `tax` classifies LT/ST and computes tax, aggregating per-lot gains.
- **Outbound (to `sheets-view`, `reports`):** `Snapshot.positions` and `open_lots`.

## Decisions & Alternatives

| Decision | Chosen | Alternatives Considered | Rationale |
|----------|--------|------------------------|-----------|
| Price/quantity scale | `value_cents = scale(qty_micro × price / 1e6)`, round half-even, once at the `Cents` boundary | price-per-micro-share; rational basis | Prices are naturally cents-per-whole-share and qty is micro-shares; a single documented divisor keeps one rounding site and exact-integer storage. |
| Event total order | Monotonic `Seq`; `Date` is data | Sort by `Date`; insertion-order-only | Determinism must be provable against a defined order; same-date events need an unambiguous sequence. |
| Basis storage | Per-lot `remaining_basis_cents`, swept exact at closure; **basis is the cross-split invariant** | `basis_per_share` (lossy on split); `total_basis`+`original_qty` (lossy on partial consume) | Conserves basis exactly across consumption *and* splits; share counts are frame-relative under splits, basis is not. |
| Overflow | `i128` products, scale down, range-check the **final** result vs `MONEY_CAP` | bound the raw product; `i128` storage; wrapping | Storage stays `i64` and Verus totality is provable; the pre-scale product is 1e6× larger and must not be bound-checked. |
| Corrections | `Reversal` (re-fold with target filtered); **backward-looking** dependency check | In-place edit; cascade-reverse dependents | Append-only preserves determinism; rejecting a Reversal when a surviving earlier event depends on the target keeps the log always-valid and makes filter-re-fold provably total. |
| Validation timing | At append, against replayed-so-far (filtered) prefix | Validate during replay | A stored log is valid by construction → `replay` is total and proof-friendly. |
| Lot selection | Specific-ID primary, FIFO `(acquire_date, open_seq)` fallback, no short sales | FIFO only; average-cost | Owner designates lots for tax efficiency; `open_seq` makes same-date FIFO deterministic; under-supply is a rejection. |
| Multi-lot Sell allocation | Largest-remainder (floor + signed residual, tie-break `(acquire_date, open_seq)`) | naive integer division | Defined for negative totals; guarantees per-lot proceeds sum exactly to the sale total. |
| Split fractional results | Per-lot `round_half_to_even(qty × num/den)`; aggregate stated as `round(Σ × num/den)` | restrict to `ratio_den == 1` only | Handles real fractional-ratio splits (e.g. 3-for-2) at micro-share granularity; true cash-in-lieu of a whole fractional share stays deferred. |
| LT/ST classification owner | `tax` leaf (ledger-core reports `holding_days`) | ledger-core computes is_long_term | Keeps the >1-year threshold (a tax rule) out of the money-conservation core. |
| Platform binding | Lots are platform-bound; a Sell is same-platform | Cross-platform sells; a `Transfer` event | The owner never moves shares between platforms; a mismatch is a rejection. |
| RSU basis | FMV at vest; sell-to-cover is an ordinary Sell | Legacy $0 basis | IRS-mandated; $0 basis double-taxes already-taxed vest income (see HLD). |

## Open Questions & Future Decisions

### Deferred
1. **Cash-in-lieu / fractional-share remainder.** When a split (or merger) leaves a whole
   fractional *share* the broker pays cash for, and the exact rounding at that boundary.
2. **Mergers & spin-offs.** A future corporate-action family alongside Split (basis allocation).

### Out of scope
- **Dividend income** (qualified vs ordinary) — tracked elsewhere, not in this portfolio kernel.
- **Wash sales** — disallowed-loss adjustment is not modeled.

## References

- HLD: `docs/high-level-design.md` (event taxonomy, RSU basis, corporate-actions decisions).
- Mirrored idioms: the prior private project's `portfolio-engine-core` (Verus kernel,
  `#[cfg(kani)]` harnesses) and its trust-boundary drift guard.
- Legacy model: Stock Actions / Stock Sales tabs (tranche IDs, vest `-a`/`-b` children).
