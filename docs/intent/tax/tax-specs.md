# tax — EARS Specs

Specs owned by the `tax` leaf (prefix `TAX`; see `tax-design.md`). Status: `[x]` implemented ·
`[ ]` active gap · `[D]` deferred. Amounts are integer `Cents`. `T_J(income, st, lt)` is a
jurisdiction's total annual tax function; an accrual is the marginal increment a gain adds.
Bracket/income/NIIT/de-minimis values are `config` data; `tax` consumes them.

## Tax Calculation

- [x] **TAX-CALC-001**: The system shall classify a realized gain as long-term iff its `sale_date` is strictly after the first anniversary of its `acquire_date` (the anniversary of a Feb-29 acquisition being March 1), and short-term otherwise.
- [x] **TAX-CALC-002**: The system shall assign each realized gain a `tax_year` equal to the calendar year of its `sale_date`.
- [x] **TAX-CALC-003**: The system shall compute federal tax by stacking short-term gains on ordinary income at the federal ordinary brackets, stacking long-term gains above ordinary income at the preferential long-term brackets, and adding NIIT at the configured rate on investment gains above the configured MAGI threshold.
- [x] **TAX-CALC-004**: For a state that taxes capital gains as ordinary income (DC and NJ at present), the system shall compute state tax by applying the state's ordinary brackets to all gains (long-term and short-term alike), stacked on state income.
- [x] **TAX-CALC-005**: The system shall compute a gain `g`'s accrual for a jurisdiction as `T_J(income, max(0,st_before+st(g)), max(0,lt_before+lt(g))) − T_J(income, max(0,st_before), max(0,lt_before))`, where `st_before`/`lt_before` are that `tax_year`'s gains realized chronologically before `g` in `(sale_date, sale_seq)` order.
- [x] **TAX-CALC-006**: The system shall clamp `T_J`'s gain arguments at zero so that a within-year net loss contributes zero gain-tax and `T_J` is total over negative inputs.
- [x] **TAX-CALC-007**: When an earlier-dated realized gain is inserted, the system shall re-derive the accruals of chronologically later gains in the same `tax_year`.
- [x] **TAX-CALC-008**: Where an `AmountOverride` event applies to an accrual, the system shall substitute its `applied_amount_cents` for the computed increment and retain both the derived and the applied amount.
- [x] **TAX-CALC-009**: The system shall estimate the tax an open position would incur if sold today — using the Snapshot's kernel-computed per-lot unrealized (no re-scaling) and `runtime`'s as-of-today date, classifying each lot LT/ST as of today and applying the `T_J` marginal increment over the year's realized YTD gains — and derive a per-position effective unrealized tax rate in `ppm` (0 when unrealized ≤ 0).
- [x] **TAX-CALC-010**: The system shall treat the unrealized tax estimate as an estimate only — creating no accrual, no lifecycle state, and no event — and shall report it as degraded when the position's mark is unavailable or its state resolves to `NoBracketsAvailable`.
- [x] **TAX-CALC-011**: The system shall map each gain's `accrues_to_state` (and Federal) to configured jurisdictions, validating each against the configured states; a gain stamped with an unconfigured state shall resolve to `NoBracketsAvailable` for the state portion, and a missing stamp shall fall back to the current-residency default, flagged.
- [x] **TAX-CALC-012**: The system shall carry config's per-jurisdiction `BracketState` (`Verified | Stale | NoBracketsAvailable`) on every accrual, reserve figure, and estimate it emits, and when a jurisdiction is `NoBracketsAvailable` shall report that jurisdiction's accrual/estimate as unavailable (degraded), never zero.
- [x] **TAX-CALC-013**: When computing the federal NIIT add-on (TAX-CALC-003), the system shall include ordinary income in the MAGI comparison — taking the investment-gain amount above the threshold as `max(0, income + (st + lt) − magi_threshold)` (gains clamped at zero) — and shall apply the NIIT rate to the lesser of that amount and the total investment gain `st + lt`, so income alone can push gains past the threshold yet NIIT never taxes more than the gains themselves.
- [x] **TAX-CALC-014**: When computing federal tax (TAX-CALC-003), the system shall stack long-term gains above the sum of ordinary income and short-term gains at the preferential long-term brackets — short-term gains being taxed as ordinary income, so they raise the bracket floor the long-term gains begin at.
- [x] **TAX-CALC-015**: When deriving the unrealized tax estimate over a position whose lots are mixed-sign (TAX-CALC-009), the system shall clamp the combined estimated tax into `[0, net_positive_unrealized]`, where `net_positive_unrealized = max(0, Σ per-lot unrealized)`, so the per-position effective unrealized tax rate stays within `[0, 100%]` even when a gross gain in one regime is offset by a loss in another.

## Accruals & Lifecycle

- [x] **TAX-ACCRUAL-001**: The system shall derive one accrual per `(sale_id, lot_id, jurisdiction, tax_year)` from each `RealizedGain`, defaulting to the Accrued state with no event.
- [x] **TAX-ACCRUAL-002**: When an `Allocate` event is applied, the system shall record the accrual's reserve `account_label` and advance it to Allocated, permitting re-allocation (last-write-wins on `account_label`) until the accrual is Paid.
- [x] **TAX-ACCRUAL-003**: When a `Move` event is applied to an Allocated accrual, the system shall record the actual `amount_cents` moved and its `date` and advance the accrual to Moved.
- [x] **TAX-ACCRUAL-004**: When a `Pay` event is applied, the system shall mark every covered Moved accrual Paid and record the remittance amount, date, and period.
- [x] **TAX-ACCRUAL-005**: While an accrual's absolute amount is below the configured de-minimis threshold, the system shall auto-settle it — requiring no lifecycle action and excluding it from outstanding-balance prompts.
- [x] **TAX-ACCRUAL-006**: When a sale is reversed in `ledger-core` while its accrual is still Accrued, the system shall omit that accrual on replay.
- [x] **TAX-ACCRUAL-007**: For a closed prior year, the system shall accept a single combined migration accrual keyed by `(jurisdiction, tax_year)` with no backing `RealizedGain`, treating that combined-migration key as a first-class accrual key the append-time validator recognizes (so an `AmountOverride`/`Allocate`/`Move`/`Pay` against it is validated against the seeded migration accrual, not rejected as an orphan), and while one exists shall supersede that year's per-`RealizedGain` accruals (excluding them from outstanding) so the year reconciles to the seeded legacy actual.
- [x] **TAX-ACCRUAL-008**: For a combined migration accrual (no backing `RealizedGain`, keyed by `(jurisdiction, tax_year)`), the system shall leave the accrual's `term` (long-term/short-term) undefined — stamping a fixed `LongTerm` placeholder that no consumer shall read as a regime classification, since a combined legacy-actual figure is regime-agnostic.

## Reserves

- [x] **TAX-RESERVE-001**: The system shall compute each `(jurisdiction, tax_year)` reserve balance as the sum of its `Move` amounts minus the sum of its `Pay` amounts.
- [x] **TAX-RESERVE-002**: The system shall permit a reserve balance to be negative, surfacing it as an over/under-funding signal rather than rejecting the `Pay`.

## Reporting

- [x] **TAX-REPORT-001**: The system shall produce a quarterly sales report grouping realized gains and accruals by `sale_date` into the IRS estimated periods (Jan 1–Mar 31, Apr 1–May 31, Jun 1–Aug 31, Sep 1–Dec 31).
- [x] **TAX-REPORT-002**: For each quarterly period and jurisdiction, the quarterly report shall show realized gain (long-term/short-term split), computed accrual, and the cumulative safe-harbor target (22.5 / 45 / 67.5 / 90%).
- [x] **TAX-REPORT-003**: The system shall produce an annual report per `(jurisdiction, tax_year)` of accrued, moved, paid, outstanding (`accrued − paid`), and shortfall (`accrued − moved`).
- [x] **TAX-REPORT-004**: The annual report shall show the effective rate (`accrual ÷ gain`) only where the absolute gain exceeds the de-minimis threshold, and otherwise report it as not applicable.
- [x] **TAX-REPORT-005**: When aggregating per-`(jurisdiction, tax_year)` figures, the annual report (TAX-REPORT-003) and the quarterly sales report (TAX-REPORT-001/002) shall own applying the de-minimis (TAX-ACCRUAL-005) and migration-supersession (TAX-ACCRUAL-007) exclusions — excluding any accrual marked auto-settled (`|amount|` below de-minimis) or superseded from the accrued/moved/paid/outstanding/shortfall and per-period accrual sums — so a migrated year reconciles to the seeded legacy actual with no double-count and rounding-epsilon accruals add nothing.

## Validation & Errors (append-time)

- [x] **TAX-ERR-001**: The system shall validate each `TaxEvent` against the replayed-so-far state before appending it, and shall leave state byte-identical when it rejects.
- [x] **TAX-ERR-002**: If a `Move` targets an accrual not in the Allocated state, then the system shall reject it.
- [x] **TAX-ERR-003**: If a `Pay` targets an accrual not in the Moved state, or one already Paid, then the system shall reject it.
- [x] **TAX-ERR-004**: If a `Pay`'s `covers` list includes an accrual whose jurisdiction or `tax_year` differs from the `Pay`'s, then the system shall reject it.
- [x] **TAX-ERR-005**: If an `AmountOverride`'s `applied_amount_cents` is negative for a positive gain, or exceeds that gain, then the system shall reject it.
- [x] **TAX-ERR-006**: For an `AmountOverride` whose backing gain is non-positive or absent, the system shall bound `applied_amount_cents` to the closed interval `[min(0, gain), max(0, gain)]` — a loss to `[gain, 0]`, a zero gain or an orphan key (no backing `RealizedGain`) to `{0}` only — rejecting any amount outside it; except where the key is a combined-migration key (TAX-ACCRUAL-007), in which case the override shall equal the seeded legacy actual and any other amount shall be rejected.

## Verification Invariants (Verus-proven, Kani bounded-checked)

- [x] **TAX-VERIF-001**: The system shall make `accrual(g)` non-decreasing in `g` for each jurisdiction (monotonic tax).
- [x] **TAX-VERIF-002**: For a positive gain over a non-negative year-to-date base, the system shall keep `0 ≤ accrual(g) ≤ g`, and shall reject a configured rate set that would breach 100% of gains (bounded tax).
- [x] **TAX-VERIF-003**: The system shall keep `T_J` total over all integer `(income, st, lt)` inputs, including negative gain arguments (total calculation).
- [x] **TAX-VERIF-004**: The system shall advance accrual states only Accrued → Allocated → Moved → Paid (Allocate self-loop permitted), with no backward transition, no skip, and no double-pay (lifecycle state machine).
- [x] **TAX-VERIF-005**: The system shall maintain every `reserve(J, year) ≡ Σ Move − Σ Pay` for that key (reserve conservation).
- [x] **TAX-VERIF-006**: The system shall keep the four IRS estimated periods partitioning the tax year with no gap or overlap, so `Σ over periods (gains, accruals) ≡ annual (gains, accruals)` (quarterly partition).
- [x] **TAX-VERIF-007**: When a `TaxEvent`'s `accrual_key` has no backing `RealizedGain`, the system shall fold it as a no-op and surface an orphan warning, keeping replay over any `TaxEvent` log total (orphan totality).
- [x] **TAX-VERIF-008**: The system shall surface the orphan warnings of TAX-VERIF-007 as an explicit, deterministically ordered (by `Seq`) list of typed warnings — each naming the orphan `accrual_key` and which lifecycle kind (`Allocate | Move | Pay | AmountOverride`) folded as a no-op — alongside the computed accruals on the same aggregation pass, and shall additionally flag any reserve carrying a backless `Move`/`Pay` so a now-backless reserve balance is presented for manual unwind rather than as an ordinary, indistinguishable balance.
