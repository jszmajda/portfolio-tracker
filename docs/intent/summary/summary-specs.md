# summary — EARS Specs

Specs owned by the `summary` leaf (prefix `SUMMARY`; see `summary-design.md`). Status: `[x]`
implemented · `[ ]` active gap · `[D]` deferred. `summary` is a headless, formatting-only command
(the `dailies` replacement) over `reports`/`tax`/`ledger-core`.

## Emitted Summary

- [x] **SUMMARY-EMIT-001**: The system shall emit a terminal portfolio summary — header (total market value, post-tax net), per-symbol positions, and a current-year tax-reserve line — headlined by the **trading day** of the displayed value, with the run timestamp as a secondary line.
- [x] **SUMMARY-EMIT-002**: The header post-tax Net shall use the same priced/degraded position set as Total value, and shall be flagged partial whenever Total value is.
- [x] **SUMMARY-EMIT-003**: When any symbol is unpriced, the system shall name the affected symbols and mark the totals partial.
- [x] **SUMMARY-EMIT-004**: The system shall render the Net and tax line per `config`'s bracket state — plainly when Verified, marked `[est, brackets stale]` when Stale, and `n/a (no brackets)` on `NoBracketsAvailable` — never a fabricated reserve or net.
- [x] **SUMMARY-EMIT-005**: The current-year tax-reserve line shall show, summed across jurisdictions from `tax`'s annual rows, exactly `accrued`, `moved`, and `outstanding`, using `tax`'s canonical definitions (`outstanding = accrued − paid`; `shortfall = accrued − moved`) — never a locally re-derived figure.
- [x] **SUMMARY-EMIT-006**: The system shall compute the next estimated-payment period from `tax::quarterly_report` and `quarter_of(today)`, and render it on the tax line; on cold-start (`NoBracketsAvailable`) it shall show no fabricated period.
- [x] **SUMMARY-EMIT-007**: The headline date is the displayed value's **trading day**, which the model carries as the opaque trading-day key; the system shall render that key as a calendar date only through `runtime`, which owns trading-day-key→calendar-date formatting (the model and the JSON carry the raw key). *(CONTRACT — `runtime` owns calendar formatting.)*
- [x] **SUMMARY-EMIT-008**: When a priced symbol lacks a folded tax estimate, the system shall mark the totals partial (`Header.partial`, via `reports`' incomplete point) even though that symbol is **not** named in `degraded_symbols` — so `[partial]` can show with no named symbol, never a fabricated net.

## Day-Over-Day Delta

- [x] **SUMMARY-DELTA-001**: The system shall compute the delta with both operands as History points — current = today's just-appended point, baseline = the most recent point whose trading-day key is strictly less than today's — so that header value − delta = baseline value.
- [x] **SUMMARY-DELTA-002**: When the baseline point is more than one trading day prior, the system shall annotate the actual span in the header.
- [x] **SUMMARY-DELTA-003**: If the current or baseline point is incomplete (degraded), then the system shall render the affected delta as `—‡` with a footer reason, never a fabricated number.
- [x] **SUMMARY-DELTA-004**: On the first ever run (no strictly-prior point), the system shall render the delta as `—`.
- [x] **SUMMARY-DELTA-005**: When no point is appended this run (offline, append failed, or lock held), the system shall compute a distinct, flagged delta from the live uncaptured value against the latest stored point — never the plain point-to-point form.

## Capture & Locking

- [x] **SUMMARY-CAP-001**: The system shall trigger `reports.append_snapshot` for the current trading day before computing the delta (last-wins per trading day).
- [x] **SUMMARY-CAP-002**: The system shall acquire the advisory write-lock before capture; if the lock is held, it shall run read-only (skip the capture, use the uncaptured-delta path) and note it.
- [x] **SUMMARY-CAP-003**: When noting an uncaptured run, the system shall distinguish the reason — **lock held** (a concurrent writer held the advisory write-lock) versus **offline / append failed** (no point could be durably appended) — rather than conflating them in one note.

## Output Modes

- [x] **SUMMARY-OUT-001**: The system shall default to a text report on stdout and shall offer `--json` as a `schema_version`-tagged object.
- [x] **SUMMARY-OUT-002**: When printing from the cache (offline/lock-held), the system shall mark staleness in-band (a text marker, or `stale: true` in JSON).
- [x] **SUMMARY-OUT-003**: For `schema_version` = 1, the `--json` object shall carry exactly the top-level keys `schema_version`, `trading_day`, `baseline_day`, `run_at`, `total_value_cents`, `net_post_tax_cents`, `total_delta_cents`, `positions`, `tax`, `stale`, `degraded_symbols`; each `positions` element shall carry exactly `symbol`, `shares`, `price_cents`, `value_cents`, `delta_cents`, `degraded`; and `tax` shall carry exactly `year`, `accrued`, `moved`, `outstanding`, `next_period`, `brackets_state` — integer cents only, with `null` (never a fabricated zero) for a degraded or cold-start figure.
- [x] **SUMMARY-OUT-004**: The `pt` binary (`crates/pt`) shall own the **headless** entrypoint for `pt summary` — mapping argv (including `--json`) to the rendered stdout output and to the process `ExitCode` per the exit-code contract — while `runtime` owns the composition root that constructs and wires its inputs.

## Exit Codes

- [x] **SUMMARY-EXIT-001**: When a summary is produced (fresh or stale), the system shall exit 0.
- [x] **SUMMARY-EXIT-002**: If no trustworthy summary can be produced — a `reports` History integrity flag, missing/invalid credentials, or an unreadable cache — then the system shall exit non-zero (2).
- [x] **SUMMARY-EXIT-003**: If a capture append fails mid-run, then the system shall treat it as non-fatal — print the uncaptured-delta summary, note the failure, and exit 0 (retried next run).
