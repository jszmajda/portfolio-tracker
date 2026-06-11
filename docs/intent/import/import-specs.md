# import — EARS Specs

Specs owned by the `import` leaf (prefix `IMPORT`; see `import-design.md`). Status: `[x]`
implemented · `[ ]` active gap · `[D]` deferred. `import` is one-time migration tooling; it writes
through the verified kernels + `store`, and its reconciliation is its correctness proof.

## Source Mapping

- [x] **IMPORT-MAP-001**: The system shall reconstruct a Buy event from each legacy `Stock Actions` Buy row, with lot id = legacy tranche id and basis from `$/share` + fees.
- [x] **IMPORT-MAP-002**: The system shall reconstruct a Vest event from each legacy Vest row, recovering FMV/share from the legacy `$/share` column (not the `$0` `Total Cost`); a vest's sell-to-cover `-a`/`-b` children become ordinary Sells at vest-date price.
- [x] **IMPORT-MAP-003**: The system shall reconstruct a Sell event from each legacy `Stock Sales` row, specific-ID against the referenced tranche (lot id), with qty / price / fees / date from the row.
- [x] **IMPORT-MAP-004**: The system shall treat the legacy `Positions` tab as a reconcile target only — never a source — and ignore derived columns.

## Corporate Actions, Frame & FMV

- [x] **IMPORT-CORP-001**: The system shall interpret every legacy quantity in the share-frame of its own row date, ignore the legacy remaining-shares column, and assert this share-frame rule as a precondition.
- [x] **IMPORT-CORP-002**: The system shall reconstruct a Split event for each owner-supplied known corporate action, inserted at its date in `Seq` order so pre-split lots rescale before post-split sales consume them.
- [x] **IMPORT-CORP-003**: If a Vest row's FMV source (`$/share`) is `$0`, blank, or `#DIV/0!`, then the system shall raise a hard error requiring an owner-supplied FMV rather than defaulting to `$0`.
- [x] **IMPORT-CORP-004**: If a reconstructed event fails kernel validation, then the system shall surface it as an import error resolvable via a per-row correction-override input fed into a re-run, never by editing the canonical log.
- [x] **IMPORT-CORP-005**: If a legacy row offers only a live-remaining-shares figure and no own-frame quantity, then the system shall raise a frame-ambiguous parse error (the zero-share guard) rather than interpret the live-frame figure as an own-frame quantity, since the share-frame of the quantity cannot be established (`IMPORT-CORP-001`).

## Historical Tax & Residency

- [x] **IMPORT-TAX-001**: The system shall stamp each historical Sell's `accrues_to_state` from `residency_on(sale_date)` over an owner-supplied historical residency timeline, allowing an owner-asserted (flagged) founding state when the oldest sale predates the owner's earliest recalled move.
- [x] **IMPORT-TAX-002**: For an owner-confirmed closed year, the system shall seed a single combined migration accrual per `(jurisdiction, tax_year)`, set it to the legacy actual via `AmountOverride`, and drive it through `Allocate → Move → Pay` (outstanding = 0) — superseding that year's per-`RealizedGain` accruals and excluding de-minimis ones; open-year accruals stay live and per-`RealizedGain`.
- [x] **IMPORT-TAX-003**: The system shall validate every reconstructed tax event through the same append-time tax-kernel gates an `entry` tax append uses, presenting the combined-migration key (empty `sale_id`/`lot_id`, no backing `RealizedGain`) as a first-class real key the validator recognizes (per `TAX-ACCRUAL-007`), and shall surface a rejected reconstruction as `ImportError::TaxKernelRejected` rather than committing an unvalidated tax event.

## Dry-Run & Reconciliation

- [x] **IMPORT-RECON-001**: The system shall default to a dry-run that reconstructs, validates, reconciles, and reports without writing.
- [x] **IMPORT-RECON-002**: The system shall classify each symbol's reconciliation as matched (within ±(½¢ × consumed-lot count) per position plus a portfolio-total tolerance), intended-divergence (within an *independently predicted*, **symbol-aggregate** RSU-basis or split delta), or unexplained.
- [x] **IMPORT-RECON-003**: The system shall block commit on any unexplained divergence and shall write only after the owner accepts the reconciliation.
- [x] **IMPORT-RECON-004**: The system shall reconcile reconstructed shares per symbol within a micro-share tolerance, snapping a sub-threshold closed-position residual via a closing adjustment and flagging a larger one.
- [x] **IMPORT-RECON-005**: The system shall require a typed `AcceptedReport` owner-acceptance token — distinct from the auto-computed `commit_allowed` safety gate (`IMPORT-RECON-003`) — before any commit write, so that owner acceptance and the safety gate are independently satisfied (the gate being clear never implies acceptance, and acceptance never bypasses the gate).
- [x] **IMPORT-RECON-006**: The system shall clear the auto-computed `commit_allowed` safety gate only when the reconciliation has no commit-blocking condition, where the complete blocking set is: any Unexplained dollar-divergence verdict (`IMPORT-RECON-003`), any Flagged share residual (`IMPORT-RECON-004`), a non-empty malformed-source-row list (`IMPORT-RUN-011`), a `Positions`-only symbol with no surviving rows (`IMPORT-RUN-004`), and a reconstructed-but-unpositioned symbol (`IMPORT-RUN-006`).
- [x] **IMPORT-RECON-007**: The realized-P&L reconcile target shall be the per-symbol sum of the sales tab's PRE-TAX `Profit` column — the legacy `Positions` "Realized Profit" is net of the sheet's estimated tax and is not a valid pre-tax comparison — while the share-count and unrealized targets remain the `Positions` row; a sale row whose `Profit` cell is unparseable is a malformed source row.
- [x] **IMPORT-RECON-008**: For an owner-declared adjudicated divergence — a symbol plus the owner's stated reason, supplied as owner input — the system shall record an otherwise-Unexplained dollar residual as **owner-adjudicated** (carrying both the residual and the reason in the report and into the commit record) and shall not count it in the commit-blocking set; an adjudication never applies to a share residual, and an undeclared symbol's Unexplained verdict still blocks.

## Run Mechanics

- [x] **IMPORT-RUN-001**: The system shall seed only a fresh workbook, but shall permit one holding only this import's own deterministic events and resume idempotently, writing only the missing `EventId`s.
- [x] **IMPORT-RUN-002**: The system shall derive each `EventId` from a guaranteed-unique key (row coordinate + legacy id) and shall validate legacy-id uniqueness and Sell→tranche referential integrity in a pre-pass, surfacing a duplicate id or a missing referenced tranche as an error.
- [x] **IMPORT-RUN-003**: The system shall validate every reconstructed event — ledger-core events and reconstructed tax events alike (`IMPORT-TAX-003`) — through the kernel (the same path as an `entry` append), so the imported log is valid by construction.
- [x] **IMPORT-RUN-004**: The system shall reconstruct a closed position only from its surviving Buy/Sell rows; a position present only as a `Positions` aggregate is a hard error surfaced for manual entry, never migrated as empty.
- [x] **IMPORT-RUN-005**: The system shall import the legacy daily-script-hidden symbols as real holdings, reconciling them against `Stock Actions`/`Stock Sales` like any other symbol (the script's hiding is a view concern, not a ledger fact).
- [x] **IMPORT-RUN-006**: The system shall surface a symbol reconstructed from surviving Buy/Sell rows but absent from the legacy `Positions` tab as a flagged, commit-blocking reconciliation result (the inverse of `IMPORT-RUN-004`), never silently omitting it from the report.
- [x] **IMPORT-RUN-007**: The system shall acquire `runtime`'s advisory write-lock before the commit's fresh/own-events-only target check and hold it across that check and all of the commit's appends — so no other writer can interleave between the check and the appends (no check→append TOCTOU) — releasing it only after the final append; because the lock is reentrant for a same holder, the inner append primitives re-acquire it under the held lock.
- [x] **IMPORT-RUN-008**: When fetching the legacy workbook for a run, the system shall read the three legacy tabs **read-only** — the import never writes to the legacy workbook; fetch, parse, and dry-run are read-only end to end and only the commit writes, only to the new workbook — and shall parse the owner's actual column layout into the typed legacy rows by locating headers by name (tolerating note rows above the header), reading only source columns (never derived figures), and routing a row whose required source cell is missing or unparseable to the malformed list (`IMPORT-RUN-011`) rather than dropping or guessing it.
- [x] **IMPORT-RUN-009**: The system shall keep the legacy workbook's non-portfolio tabs (an owner-configured out-of-scope list) out of scope.
- [x] **IMPORT-RUN-010**: The system shall honor owner-declared symbol exclusions — e.g. unvested grants (not holdings until they vest; pending the grant-tracking feature) and non-security holdings — by reporting every excluded row, never a silent omission.
- [x] **IMPORT-RUN-011**: The system shall list genuinely malformed source rows for manual review rather than dropping them.

## Owner's Activity Vocabulary & Frame Mappings

- [x] **IMPORT-MAP-005**: The system shall reconstruct a legacy `Exercise` row as a `Buy` — the cash paid at the strike is the basis (`$/share` × shares + fees, at the exercise date) — carrying an `exercise` provenance note on the reconstructed event's tracking code, so the acquisition kind survives in the canonical log.
- [x] **IMPORT-MAP-006**: When a whole-share Buy/Exercise row's `Total Cost` parses and differs from `shares × $/share + fees` (a display-rounded or strike-aggregated `$/share`), the system shall preserve the TRUE cash basis: derive the unit price as `⌊Total Cost / shares⌋` and fold the exact non-negative remainder into fees, so the reconstructed basis equals the legacy cash paid to the cent.
- [x] **IMPORT-MAP-007**: When a whole-share sale row's `$/share` cell is zero or blank but its `Sales - Fees` cell records positive proceeds (a price cell that was never filled), the system shall derive the unit price from the recorded proceeds — `(Sales-Fees + fees) / shares` when exact in cents, otherwise the ceiling with the non-negative remainder folded into fees — so reconstructed proceeds equal the recorded proceeds to the cent; a PRESENT `$/share` always governs (the row's price is authoritative over a derived `Sales - Fees` cell), and a zero-price zero-proceeds sale of a Vest tranche remains the sell-to-cover case (`IMPORT-MAP-002`).
- [x] **IMPORT-CORP-006**: The system shall never reconstruct a legacy `Split` activity row (the sheet's delta-share bookkeeping; the owner-input `Split` events carry the split), shall report each skipped one, and shall remap a sale referencing a split delta-row id to the owner-declared ORIGINAL tranche the delta shares came from, so the sale consumes the rescaled original lot.
- [x] **IMPORT-CORP-007**: For an owner-declared post-split-framed row (a row retroactively recorded in post-split units despite a pre-split date), the system shall convert it back to its pre-split frame at the parse boundary — qty ÷ ratio and `$/share` × ratio, by exact integer arithmetic with any inexact division a loud error, the true date untouched — so the replayed `Split` rescales it exactly once and acquisition dates (and so long/short-term classification) stay truthful.
