# ledger-core — EARS Specs

Specs owned by the `ledger-core` leaf (prefix `LEDGER`; see `ledger-core-design.md`). Status:
`[x]` implemented · `[ ]` active gap · `[D]` deferred. All amounts are integer `Cents`;
quantities are `MicroShares` (1e-6 share); `SHARE_SCALE = 1_000_000`; `scale(x) =
round_half_to_even(x / 1_000_000)` computed in `i128`.

## Event Taxonomy & Ordering

- [x] **LEDGER-EVENT-001**: The system shall fold ledger events in ascending `Seq` order, treating each event's `Date` as data and never as the fold's sort key.
- [x] **LEDGER-EVENT-002**: When a Buy event is applied, the system shall open a tax lot with `total_basis = scale(qty × unit_price) + fees` and an acquisition date equal to the event date.
- [x] **LEDGER-EVENT-003**: When a Vest event is applied, the system shall open a tax lot with `total_basis = scale(qty × fmv_per_share)` and an acquisition date equal to the vest date (the fair-market value already taxed as ordinary income at vest).
- [x] **LEDGER-EVENT-004**: When a Sell event is applied, the system shall dispose the sale quantity from the selected lots and emit one `RealizedGain` per consumed (lot, quantity) pair, each with stable identity `(sale_id, lot_id)` and the Sell's `sale_seq` for downstream chronological ordering.
- [x] **LEDGER-EVENT-005**: When a Split event is applied, the system shall rescale the open lots of the split's symbol (mechanics in `LEDGER-SPLIT-*`).
- [x] **LEDGER-EVENT-006**: When a Reversal event is applied, the system shall replay as though its target event had never occurred (the target filtered out of the fold).
- [x] **LEDGER-EVENT-007**: On Buy, Vest, and Sell events, the system shall record `platform` and the optional `tracking_code` as tranche metadata, without using them in any accounting computation.

## Tax Lots & Lot Selection

- [x] **LEDGER-LOT-001**: While a lot is open, the system shall track its `remaining_qty` (MicroShares) and `remaining_basis_cents`, holding both non-negative.
- [x] **LEDGER-LOT-002**: When a Sell consumes `c` shares from a lot whose remaining quantity exceeds `c`, the system shall realize `consumed_basis = round_half_to_even(remaining_basis × c / remaining_qty)` and then decrement `remaining_basis` and `remaining_qty` by the consumed amounts.
- [x] **LEDGER-LOT-003**: When a Sell consumes a lot's final shares (`c` equals `remaining_qty`), the system shall realize `consumed_basis` equal to the lot's entire `remaining_basis` (residual swept), leaving `remaining_basis` zero.
- [x] **LEDGER-LOT-004**: When a Sell supplies explicit `lot_refs` (specific-identification), the system shall consume exactly the named `(lot, quantity)` pairs.
- [x] **LEDGER-LOT-005**: When a Sell supplies no `lot_refs`, the system shall consume open lots of the sale's symbol on the sale's platform in ascending `(acquire_date, open_seq)` order (FIFO) until the sale quantity is satisfied.
- [x] **LEDGER-LOT-006**: The system shall assign every lot a stable, log-unique `LotId` by which a later Sell may designate it.

## Splits

- [x] **LEDGER-SPLIT-001**: When a `ratio_num : ratio_den` Split is applied, the system shall set each affected lot's `remaining_qty` to `round_half_to_even(remaining_qty × ratio_num / ratio_den)`.
- [x] **LEDGER-SPLIT-002**: When a Split is applied, the system shall leave every affected lot's `remaining_basis` and acquisition date unchanged (preserving total basis and the holding period).
- [x] **LEDGER-SPLIT-003**: When a Split is applied, the system shall affect only open lots of the split's symbol, leaving other symbols' lots and fully-consumed lots untouched.

## Realized & Unrealized P&L

- [x] **LEDGER-PNL-001**: When a Sell is applied, the system shall compute gross proceeds `= scale(qty × unit_price)` and net proceeds `= gross − fees`.
- [x] **LEDGER-PNL-002**: When a Sell consumes more than one lot, the system shall allocate net proceeds across the consumed lots by largest-remainder (floor share plus the signed residual distributed one unit per lot in descending fractional-remainder order, ties broken by ascending `(acquire_date, open_seq)`), so per-lot proceeds sum exactly to net proceeds.
- [x] **LEDGER-PNL-003**: For each consumed (lot, quantity) pair, the system shall report `gain_cents = allocated_proceeds − consumed_basis`, which may be negative.
- [x] **LEDGER-PNL-004**: For each `RealizedGain`, the system shall report `holding_days = sale_date − acquire_date` (in days), which is zero for a same-day sale.
- [x] **LEDGER-PNL-005**: For each `RealizedGain`, the system shall report `holding_days` and `accrues_to_state` but shall not classify long-term versus short-term and shall not compute tax (those are the `tax` leaf's responsibility).
- [x] **LEDGER-PNL-006**: Where a current per-whole-share mark is supplied for a symbol, the system shall compute that symbol's unrealized P&L as `Σ over open lots (scale(mark × remaining_qty) − remaining_basis)`.
- [x] **LEDGER-PNL-007**: If no mark is supplied for a symbol that has open lots, then the system shall report that symbol's unrealized P&L as absent (degraded), never as zero.
- [x] **LEDGER-PNL-008**: If a mark is supplied for a symbol with no open lots, then the system shall ignore it without raising an error.
- [x] **LEDGER-PNL-009**: The system shall produce, per symbol, a Position aggregating total open quantity, total open basis, cumulative realized P&L (the sum of that symbol's `RealizedGain.gain`), and the unrealized P&L of `LEDGER-PNL-006` when a mark is available.
- [x] **LEDGER-PNL-010**: The system shall expose per-open-lot unrealized P&L on the Snapshot (`scale(mark × remaining_qty) − remaining_basis`, or absent when the mark is degraded), so downstream tax estimation requires no second rounding site.

## Validation & Errors (append-time)

- [x] **LEDGER-ERR-001**: The system shall validate each candidate event against the replayed-so-far state (with reversed targets filtered out) before appending it to the log.
- [x] **LEDGER-ERR-002**: If a Buy, Vest, or Sell has a quantity `≤ 0`, then the system shall reject it (`NonPositiveQty`).
- [x] **LEDGER-ERR-003**: If a Buy or Vest reuses an existing `LotId`, then the system shall reject it (`DuplicateLotId`).
- [x] **LEDGER-ERR-004**: If a Sell names a lot that does not exist, or one belonging to another symbol, then the system shall reject it (`UnknownLot` / `WrongSymbolLot`).
- [x] **LEDGER-ERR-005**: If a Sell's designated quantities do not sum to its sale quantity, then the system shall reject it (`LotRefsMismatch`).
- [x] **LEDGER-ERR-006**: If a Sell names the same lot more than once, then the system shall reject it (`DuplicateLotRef`).
- [x] **LEDGER-ERR-007**: If a Sell's selected lot does not reside on the sale's platform, then the system shall reject it (`WrongPlatform`).
- [x] **LEDGER-ERR-008**: If a Sell's selected lots (specific-ID or FIFO) cannot cover its sale quantity, then the system shall reject it (`InsufficientShares`) and shall never produce a negative `remaining_qty` (no short sales).
- [x] **LEDGER-ERR-009**: If a Split has `ratio_num < 1` or `ratio_den < 1`, then the system shall reject it (`BadSplitRatio`).
- [x] **LEDGER-ERR-010**: If a Reversal targets an unknown event, an already-reversed event, or an event on which a surviving lower-`Seq` event depends, then the system shall reject it (`BadReversal`).
- [x] **LEDGER-ERR-011**: If applying an event would make a scaled `Cents` or `MicroShares` result exceed `MONEY_CAP`, then the system shall reject the event (`AmountOutOfRange`).

## Verification Invariants (Verus-proven, Kani bounded-checked)

- [x] **LEDGER-VERIF-001**: For each symbol, the system shall maintain `Σ remaining_basis + Σ realized_basis ≡ Σ acquired total_basis` (basis conservation — invariant across splits).
- [x] **LEDGER-VERIF-002**: Within a split epoch for a symbol, the system shall maintain `Σ remaining_qty + Σ disposed_qty ≡ Σ acquired_qty`, with a Split rescaling open quantities so the aggregate becomes `round(Σ × ratio_num/ratio_den)` (share accounting).
- [x] **LEDGER-VERIF-003**: The system shall keep every `remaining_qty ≥ 0` and every `remaining_basis_cents ≥ 0` at all times (non-negativity).
- [x] **LEDGER-VERIF-004**: When the system rejects a candidate event, it shall leave the log and state byte-identical to before the event (no partial mutation).
- [x] **LEDGER-VERIF-005**: The system shall make `replay(log, marks)` a pure, deterministic function of `(log, marks)` under the `Seq` total order and the `(acquire_date, open_seq)` FIFO order (replay determinism).
- [x] **LEDGER-VERIF-006**: For each Sell, the system shall ensure `Σ RealizedGain.gain ≡ net_proceeds − Σ consumed_basis`, that consumed quantities sum to the sale quantity, and that per-lot proceeds sum exactly to net proceeds (realized-gain correctness).
- [x] **LEDGER-VERIF-007**: When a Split is applied, the system shall change no affected lot's `remaining_basis` and shall change total symbol basis by zero (split basis-neutrality).
- [x] **LEDGER-VERIF-008**: When a Reversal is accepted, the system shall preserve every surviving event's precondition under the filtered re-fold, keeping `replay` total and shares and basis conserved (Reversal totality).
