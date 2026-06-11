# sheets-view — EARS Specs

Specs owned by the `sheets-view` leaf (prefix `SHEET`; see `sheets-view-design.md`). Status:
`[x]` implemented · `[ ]` active gap · `[D]` deferred. `sheets-view` projects the kernels' output
to read-only view tabs and owns the `GOOGLEFINANCE` marks read-back; it holds no
accounting/tax math.

## View Tabs & Layout

- [x] **SHEET-TAB-001**: The system shall publish four read-only view tabs — Positions, Open Lots, Realized, Tax — each per-entity data band being one row per entity with typed headers and no among-data summary rows, so native Sheets filtering and sorting work directly.
- [x] **SHEET-TAB-002**: The system shall order the workbook tabs views → config → event logs, with Positions as the leftmost (landing) tab, reasserting the order idempotently on republish.
- [x] **SHEET-TAB-003**: The view tabs shall be read-only and regenerated — only saved Sheets Filter Views survive a republish; a basic filter, a manual sort, or a human-inserted row on a view tab is overwritten by design.
- [x] **SHEET-TAB-004**: The Tax tab shall be published as two separate typed ranges — a per-accrual data band (one row per accrual) and a distinct `Tax · Reserve Summary` band (one row per `(Jurisdiction, Tax Year)`) physically separated from the accrual band, so the reserve summary is never an among-data summary row within the accrual band (SHEET-TAB-001).
- [x] **SHEET-TAB-005**: The system shall populate the `Tax · Reserve Summary` band per `(Jurisdiction, Tax Year)` from `tax::annual_report`'s figures — accrued, moved, paid, outstanding, and shortfall — consumed as pure values, holding no tax math of its own.

## Value / Formula Boundary

- [x] **SHEET-FORMULA-001**: The system shall write live-price-dependent columns (price, market value, unrealized P&L, unrealized %, estimated unrealized tax, net unrealized) as sheet formulas, and all other columns as engine-written values.
- [x] **SHEET-FORMULA-002**: The system shall write each per-row formula as references to that row's own cells, from a fixed anchored start row beneath a never-rewritten frozen header, so a row shift cannot mis-pair them.
- [x] **SHEET-FORMULA-003**: The Positions tab shall show a pre-tax `Unrealized P&L` and a live post-tax `Net Unrealized = Unrealized P&L × (1 − Est. Tax Rate)`, where `Est. Tax Rate` is the per-position effective unrealized tax rate written by `tax`; the post-tax columns shall be labelled estimates.
- [x] **SHEET-FORMULA-004**: The Tax tab shall present kernel-exact (stacked, as-of-last-sync) tax-adjusted figures, distinct from the Positions live estimate.
- [x] **SHEET-FORMULA-005**: The system shall write the `Est. Tax Rate` cell so Sheets parses it as a number (a percent-typed `userEnteredValue`, e.g. a `"%"`-suffixed value), so the live post-tax `Est. Unrealized Tax` / `Net Unrealized` formulas that multiply it evaluate numerically rather than against text.

## Symbol → Ticker Mapping

- [x] **SHEET-MAP-001**: The system shall resolve a ledger `Symbol` to its `GOOGLEFINANCE` ticker via `config`'s alias map (identity when unset) and build the `Price` formula from the resolved ticker.
- [x] **SHEET-MAP-002**: The system shall key mark read-back by row identity (symbol), asserting one row per symbol and erroring loudly if violated.

## Marks Read-Back

- [x] **SHEET-MARK-001**: After writing the price formulas, the system shall read marks on a separate settle pass, polling the `Price` cells until each is numeric or terminally errored, with a bounded timeout — never inline with the formula write.
- [x] **SHEET-MARK-002**: The system shall convert a numeric USD price to `Cents` via `round_half_to_even(price × 100)`; a positive price that rounds to 0 `Cents` shall be recorded as a degraded anomaly, not a zero mark.
- [x] **SHEET-MARK-003**: If a `Price` cell is a transient `#N/A`/`Loading...`, then the system shall keep the prior good cached mark and retry, recording the symbol degraded (no mark) only after it stays non-numeric past the bounded retry window.
- [x] **SHEET-MARK-004**: The system shall stamp each cached mark with `GOOGLEFINANCE`'s quote date, not the wall-clock read time.
- [x] **SHEET-MARK-005**: A degraded symbol mark shall feed `ledger-core`'s per-symbol degradation (`LEDGER-PNL-007`); while offline, the kernels shall use the last cached marks with their quote-epoch stamps.
- [x] **SHEET-MARK-006**: If a transport failure occurs during the marks read-back pass, then the system shall propagate the error and emit no marks for that pass (no partial or degraded marks); `runtime` owns carry-forward, retaining the last cached marks.
- [x] **SHEET-MARK-007**: A non-positive numeric price reading (`0.0` or negative) shall be recorded as a sub-cent degraded anomaly, and a non-finite reading (`NaN`/`±∞`) shall be recorded as a permanent degraded anomaly — never as a mark — so a bad reading never silently understates or corrupts value.
- [x] **SHEET-MARK-008**: The system shall render a companion `Quote Date` column on the Positions tab whose formula encodes `GOOGLEFINANCE`'s trade date as a locale-proof integer count of days since 1970-01-01 (a `TEXT`-formatted serial-date difference, empty while loading/errored), so the settle pass parses the per-symbol quote-epoch stamp from the values read-back without locale-dependent datetime parsing.

## Republish

- [x] **SHEET-PUB-001**: The system shall republish after each successful event append and on demand, running republish and the periodic mark-refresh in one serialized loop, never concurrently.
- [x] **SHEET-PUB-002**: The system shall rewrite each view tab as a single `batchUpdate` that writes the new range and truncates the residual tail to the exact row count, leaving no half-written window and no leftover stale rows.
- [x] **SHEET-PUB-003**: If a republish fails, then the system shall leave the canonical log unaffected, mark the view stale, retry on the next sync, and surface staleness via a best-effort in-tab banner and the TUI.
- [x] **SHEET-PUB-004**: `sheets-view::republish` shall acquire the runtime-owned advisory write-lock before mutating the workbook and release it after, so republish never writes concurrently with another Sheets-mutating primitive.
