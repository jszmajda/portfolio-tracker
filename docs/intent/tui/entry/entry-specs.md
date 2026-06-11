# entry — EARS Specs

Specs owned by the `entry` leaf (prefix `TUI-ENTRY`; see `entry-design.md`). Status: `[x]`
implemented · `[ ]` active gap · `[D]` deferred. `entry` is the TUI write-path surface; it mutates
only through the verified kernels + `store` and computes nothing.

## Composer & Write Loop

- [x] **TUI-ENTRY-FLOW-001**: The system shall present each flow as a composer with inline advisory validation against the cached snapshot, rendering the specific kernel/config error beside the offending field and writing nothing on rejection.
- [x] **TUI-ENTRY-FLOW-002**: On submit, the system shall re-validate against the live state as the authority; a submit-time disagreement shall re-render in the inline error slot (inline never overrides submit).
- [x] **TUI-ENTRY-FLOW-003**: The system shall submit in the order confirm (if gated) → acquire the advisory write-lock → append + read-back-verify → confirmed, clearing the composer only on confirmed durability.
- [x] **TUI-ENTRY-FLOW-004**: If append or read-back-verify fails or the workbook is unreachable, then the system shall return control with the entry intact and an `[r]etry`; a retry shall reuse the prior confirmation and be idempotent (stable `EventId`, per TUI-ENTRY-FLOW-010).
- [x] **TUI-ENTRY-FLOW-005**: If the advisory write-lock is held, then the system shall fail the submit non-destructively (a `warn` notice, entry preserved, retry) without queuing.
- [x] **TUI-ENTRY-FLOW-006**: The system shall gate Reversal, Pay, Override, and tax-rule edits with a confirm step that restates what will be written, and shall not gate plain appends (Buy / Vest / Sell / Split / residency / platform / alias).
- [x] **TUI-ENTRY-FLOW-007**: The system shall default the date to today, `accrues_to_state` to `config.residency_on(sale_date)`, the platform to `config` suggestions, and resolve symbols via the alias table — all overridable.
- [x] **TUI-ENTRY-FLOW-008**: The system shall be the active keybinding context for its forms: `tab`/`shift-tab` (and `↑`/`↓`) move between fields, `enter` on the last field submits, `esc` cancels and pops the context (confirm-discard if any field is dirty); a focused field enters text-input mode in which the shell's global single-key accelerators are suppressed and `esc` returns to field-navigation mode.
- [x] **TUI-ENTRY-FLOW-009**: The system shall render each flow as a rounded titled panel with the gilt focus caret on the focused field, tracked-caps labels, a single inline error/advisory slot directly beneath the offending field (shared by inline validation TUI-ENTRY-FLOW-001 and a submit-time disagreement TUI-ENTRY-FLOW-002), and a footer hint line of the active bracketed single-key accelerators (`[F]`, `[c]`, `[r]`, `[enter]`, `[space]`) bound only while no field is in text-input mode.
- [x] **TUI-ENTRY-FLOW-010**: The system shall freeze the composed event's content at first submit and re-submit the byte-identical event on `[r]etry` — re-deriving no field between attempts — so the `store`-assigned content-hash `EventId` (STORE-SCHEMA-003/004) is identical across attempts and the retry is recognised as the same event; a composer edit after a returned-control submit is a new entry (new content, new id), not a retry.

## Activity Entry

- [x] **TUI-ENTRY-ACT-001**: The system shall compose a Buy (symbol, qty, unit price, date, fees, platform, tracking code) into a Buy event.
- [x] **TUI-ENTRY-ACT-002**: The system shall compose a Vest (symbol, qty, FMV/share, date, platform, tracking code) into a Vest event (no fee field).
- [x] **TUI-ENTRY-ACT-003**: The system shall compose a Sell (symbol, qty, unit price, date, fees, platform, tracking code, lots) into a Sell event, driving the lot picker for lot selection.
- [x] **TUI-ENTRY-ACT-004**: The system shall compose a Split (symbol, ratio num:den, date) into a Split event.
- [x] **TUI-ENTRY-ACT-005**: The system shall compose a Reversal by picking a target from a recent-events list that greys/omits already-reversed events and prior Reversals, showing any backward-dependency block inline before submit.
- [x] **TUI-ENTRY-ACT-006**: When a Buy or Vest names a symbol matching no existing position and no alias, the system shall warn (not block) and require a confirm-new-position step.

## Lot Picker

- [x] **TUI-ENTRY-LOT-001**: For a Sell, the system shall show the symbol's open lots on the sale's platform with term (LT/ST as of the sale date), remaining qty, and basis/share, and accept a per-lot allocation or a FIFO shortcut.
- [x] **TUI-ENTRY-LOT-002**: The system shall require allocations to sum to the sale qty (✓ only when equal), cap each lot at its remaining, and refuse cross-platform, duplicate, or wrong-symbol picks inline.
- [x] **TUI-ENTRY-LOT-003**: When the sale qty changes, the system shall reset the allocation.
- [x] **TUI-ENTRY-LOT-004**: The system shall show an explicit empty/insufficient state when the sale's platform has no open lots for the symbol or insufficient remaining, and shall grey a `rem 0` lot as unallocatable.
- [x] **TUI-ENTRY-LOT-005**: The system shall show a live estimated gain/tax preview on the proposed allocation, stacked at the sale's `sale_date` position, `[est]`-flagged and degraded on a missing mark, with per-lot figures non-exact (±½¢) — never blocking the sell.
- [x] **TUI-ENTRY-LOT-006**: The system shall render the lot picker as a table — one row per open lot (id, source+date, term, remaining, basis/share, a `take [N]` input cell) with the focus caret on the active row — and place the running `allocated N / M`, the `[est]` gain/tax preview, the empty/insufficient state (TUI-ENTRY-LOT-004), and the inline error in a footer band rather than over any data row.

## Tax-Accrual Actions

- [x] **TUI-ENTRY-TAX-001**: The system shall allocate an accrual to a reserve account (re-allocatable until Paid) via an Allocate `TaxEvent`.
- [x] **TUI-ENTRY-TAX-002**: The system shall move an accrual, recording the actual amount and date (which may differ from the estimate) via a Move `TaxEvent`, and shall surface any shortfall on the Move form beside the amount field as a live `warn` advisory (`⚠ short $N vs accrued $M`) comparing the entered actual against the selected accrual's computed amount (or, for a batch, the selected set's summed accrual), cleared by an exact or over move — advisory only, never blocking the Move.
- [x] **TUI-ENTRY-TAX-003**: The system shall record a Pay for a `(jurisdiction, tax_year, period)`, checking off covered Moved accruals (which must match the Pay's jurisdiction/year), with `amount` and the covered set independent (the ✓ advisory; a partial or over-payment allowed), under a confirm; as covers are toggled it shall show a pre-submit inline `warn` advisory beside any selected accrual whose `(jurisdiction, tax_year)` differs from the Pay's (the mismatch submit would reject as `PayCoverMismatch`), submit re-validating as the authority.
- [x] **TUI-ENTRY-TAX-004**: The system shall override an accrual with an absolute amount and a reason under a confirm, retaining both the derived and applied amounts.
- [x] **TUI-ENTRY-TAX-005**: For a batch action, the system shall snapshot the selected set at confirm and re-validate at submit; if a selected accrual vanished or repriced, it shall return to the form with the delta flagged rather than commit a stale set.

## Config Edits

- [x] **TUI-ENTRY-CFG-001**: The system shall edit the residency timeline (add a move; future-dated allowed; consecutive same-state rejected; a founding-entry violation shown inline), noting the change affects only future-dated `accrues_to_state` defaults, not already-stamped events.
- [x] **TUI-ENTRY-CFG-002**: The system shall edit tax brackets / income / NIIT / de-minimis with `config` validation inline (non-monotonic or stacked-rate breach), confirming with the retroactive blast radius ("reprices N unpaid accruals' estimates").
- [x] **TUI-ENTRY-CFG-003**: The system shall manage the platform suggestion list and the symbol→ticker alias map.
