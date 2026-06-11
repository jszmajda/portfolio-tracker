# reports — EARS Specs

Specs owned by the `reports` leaf (prefix `REPORT`; see `reports-design.md`). Status: `[x]`
implemented · `[ ]` active gap · `[D]` deferred. `reports` is pure portfolio analytics over the
`Snapshot`, marks, `tax`'s unrealized estimate, and the durable value History; it does not own
the tax reports (`tax` does).

## Composition

- [x] **REPORT-COMP-001**: The system shall compute portfolio composition per symbol and per platform — market value, share of total (%), total basis, pre-tax unrealized P&L, and net-of-tax unrealized (via `tax`'s estimate) — in both pre- and post-tax breakdowns.
- [x] **REPORT-COMP-002**: A symbol degraded in either its mark or its tax estimate shall be treated as degraded in both the pre- and post-tax views and excluded from the percentage denominator (the sum of priced positions), so shares of the priced portfolio sum to 100%.
- [x] **REPORT-COMP-003**: The system shall surface, alongside composition, the count of degraded symbols and the priced-coverage fraction (priced basis ÷ total basis).
- [x] **REPORT-COMP-004**: If priced market value is ≤ 0, then the system shall report shares as not-applicable, distinguishing "no positions" from "positions exist but unpriced" from "all positions closed (every quantity is zero)" (treated as "no positions"), and shall flag a position with negative basis rather than emit a nonsensical basis share.
- [x] **REPORT-COMP-005**: The system shall aggregate net-of-tax unrealized across a symbol's lots and platforms by distributing the symbol's exact estimated tax (from `tax`'s estimate) over its sub-positions by pre-tax weight using largest-remainder apportionment, so that the per-lot and per-platform net-of-tax sub-totals reconcile exactly to the symbol's estimated tax with no penny drift.
- [x] **REPORT-COMP-006**: The system shall include in each composition row the pre-tax unrealized P&L as a fraction of total basis (in ppm, rounded half-to-even), and shall report it as not-applicable when the row is degraded or its total basis is ≤ 0 — never a fabricated or nonsensical percentage.

## Value-Over-Time

- [x] **REPORT-VOT-001**: The system shall key each value-series point by the marks' quote-epoch trading day (not the run's calendar day); consecutive captures sharing a quote-epoch resolve to one trading-day point (last-wins).
- [x] **REPORT-VOT-002**: The system shall compare per-symbol value (split-neutral) across time and shall never diff share counts across time.
- [x] **REPORT-VOT-003**: The system shall show trading days with no captured point as explicit gaps, without interpolation. CONTRACT: `render_series` does not derive the trading-day calendar; the caller (owned by `runtime`) supplies the full set of trading-day keys spanning the series, and `reports` marks any supplied key lacking a captured point as a gap.
- [x] **REPORT-VOT-004**: When a point is captured with any degraded symbol, the system shall flag it incomplete (total partial), and a day-over-day delta computed across an incomplete point shall be flagged or suppressed, never silently shown.
- [x] **REPORT-VOT-005**: The system shall begin the value series at the first capture and shall not fabricate pre-capture daily values.
- [x] **REPORT-VOT-006**: The per-symbol value delta shall include the percent change of the value change relative to the earlier point's value (in ppm, rounded half-to-even), and shall report it as not-applicable when either endpoint lacks a priced value or the earlier value is ≤ 0 — never a fabricated percentage.

## History Capture & Persistence

- [x] **REPORT-HIST-001**: `append_snapshot` shall write one trading-day point to the durable workbook History tab via an atomic, read-back-verified `batchUpdate`, and on read-back-verify failure shall return `Err(WriteVerifyFailed)` so the failure is flagged (a lost point is non-reconstructable). CONTRACT: `reports` only flags; the retry loop on a flagged failure is the runtime caller's (owned by `runtime`), not `reports`'.
- [x] **REPORT-HIST-002**: When a capture targets a trading day that already has a point, the system shall overwrite it (last-wins), so re-runs never duplicate.
- [x] **REPORT-HIST-003**: On reading History, the system shall validate unique, non-decreasing trading-day keys, parseable rows, and a content checksum, and on failure shall flag loudly rather than fold a corrupt series (recovery via Google Sheets version history). CONTRACT: the runtime Sheets-backed client owns row→`SeriesPoint` deserialization; `reports` receives the parse outcome and propagates a parse failure as a History-read error (it does not itself parse raw Sheets rows).
- [x] **REPORT-HIST-004**: The system shall read the value series from the cache currency-checked against the workbook, treating the workbook as authoritative on mismatch.

## Realized-P&L History

- [x] **REPORT-REAL-001**: The system shall compute realized-P&L history from the event log's realized gains, grouped by calendar year or an arbitrary date range, labelled "calendar" and distinct from `tax`'s estimated-period quarterly report.
- [x] **REPORT-REAL-002**: The system shall compute the calendar-year-to-date realized P&L for a given year from the event log's realized gains — a single calendar-labelled row summing proceeds, basis, and gain over the sales in that year — returning an exact zero row (not a degraded marker) when the year has no realized sales.
