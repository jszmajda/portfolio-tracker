---
parent: high-level-design
prefix: SUMMARY
---

# summary

## Context and Design Philosophy

`summary` is the **headless command** that replaces the legacy `portfolio-summary.sh` scraper: a
non-interactive run that captures the day's value point, computes a **day-over-day delta**, and
prints a terminal portfolio summary to stdout for the `dailies` framework. It is **thin
orchestration + formatting** — the numbers come from `reports`, `tax`, and `ledger-core`; it adds
no analytics of its own.

It subsumes the legacy script's two hacks: scraping computed cells out of the sheet (now it reads
the kernels directly) and the 20-hour `portfolio-history.json` rotation (now it appends to
`reports`' trading-day History, last-wins per trading day).

Principles:

- **Format, don't compute.** Composition, value, and the delta come from `reports`; reserve
  status from `tax`. `summary` arranges them for a terminal.
- **Point-to-point, reconciling deltas.** Both delta operands are History points, so the header
  value, the baseline, and the delta always reconcile arithmetically.
- **Honest deltas.** A delta across an incomplete (degraded) point, or against a live-uncaptured
  value, is rendered distinctly — never shown as a plain real move.
- **Degrade, never lie to dailies.** Offline → a clearly stale (but usable) summary, exit 0. A
  failure that means *no trustworthy summary* (corrupt History, bad creds) exits **non-zero** so
  dailies surfaces it.

## What It Emits

A compact terminal report (ASCII), default to stdout. The **headline date is the trading day** of
the displayed value (the appended point's key); the run time is a secondary "as of" line. The
model carries the trading day as the **opaque trading-day key**; rendering that key as a calendar
date (the headline shown below) happens only through `runtime`, which **owns trading-day-key →
calendar-date formatting**. The `--json` output carries the raw key, not a formatted date:

```
📈 Portfolio — Fri Jun 5 2026               (Δ since Thu Jun 4)
   as of run Sat Jun 6 08:00 ET
──────────────────────────────────────────────────────────────
 Total value      $1,284,300     +$8,420  (+0.66%)
 Net (post-tax)     $902,160     +$5,910            [est]
──────────────────────────────────────────────────────────────
 AMZN   834 @ $261.26    $217,891     +$3,120
 GOOGL   60 @ $376.37     $22,582       −$410
 PLTR     0   (unpriced)         —          —‡
   …
──────────────────────────────────────────────────────────────
 Tax reserve 2026:  accrued $42,100 · moved $30,000 · outstanding $14,600
 Next est. payment:  Q2 (due Jun 15) — see Tax tab
──────────────────────────────────────────────────────────────
 ‡ delta unavailable (degraded point)   ⚠ 1 symbol unpriced (PLTR) — totals partial
```

- **Header**: total market value and post-tax net (`[est]`, from `tax`'s unrealized estimate),
  each with the day delta. **Net uses the same priced/degraded set as Total value** and is flagged
  partial whenever Total value is.
- **Positions**: per symbol — shares, price, market value, delta vs the baseline point.
- **Tax line**: current-year reserve status and the next estimated-payment period, from
  `tax`. The reserve figures are summed across jurisdictions from `tax`'s annual rows and shown
  with `tax`'s canonical definitions — `outstanding = accrued − paid` (what is still owed) and
  `shortfall = accrued − moved` (how under-funded the reserve is). The line surfaces `accrued`,
  `moved`, and `outstanding`; in the example above accrued $42,100, moved $30,000, paid $27,500 →
  outstanding $14,600 (and shortfall $12,100). The next estimated-payment period is computed from
  `tax::quarterly_report` and `quarter_of(today)`.
- **Markers**: `—` first-ever point (no baseline); `—‡` delta unavailable across a degraded
  point; `⚠` degraded-symbols footer.

## Day-Over-Day Delta

Both operands are **History points**, so everything reconciles (`header value − delta =
baseline value`):

- **Current operand** = today's just-appended point. **Baseline** = the most recent History
  point whose trading-day key is **strictly less than** today's (not "the second row" — that
  breaks if today's append failed). A gap widens the baseline; the header annotates the actual
  span (`Δ since Thu Jun 4`).
- **Incomplete points** (current or baseline degraded): the affected delta renders `—‡`, never a
  fabricated number (honoring `reports`' incomplete-point flag discipline). Per symbol, a symbol
  degraded in either
  point shows `—‡`.
- **First ever run** (no strictly-prior point): the delta column shows `—`.
- **No appended point this run** (offline, or the append failed): the current operand is the
  **live composition value, explicitly labelled uncaptured**, and the baseline is the latest
  stored point — a **distinct, flagged** "uncaptured-vs-stored" delta, not the normal
  point-to-point one.

## Daily Capture

`summary` triggers `reports.append_snapshot` for the current trading day before computing the
delta, so the day's value point is durably recorded (last-wins per trading day) — replacing the
legacy rotation entirely.

The capture goes through `runtime`'s **cross-process advisory write-lock** (owned by `runtime`,
acquired inside the write primitive), so a cron `summary` and an open interactive TUI never append
concurrently. If `runtime` reports the lock **held**, `summary` runs **read-only** — it skips the
capture and prints from the current state via the uncaptured-delta path, noting it.

The uncaptured-run note **distinguishes its reason**: a *lock-held* read-only run (a concurrent
writer holds the advisory lock) is noted distinctly from an *offline / append-failed* run (no
point could be durably appended). Both produce the same flagged uncaptured delta, but the note
tells the operator which case occurred rather than conflating them.

## Output Modes & Integration

- **Text** (default): the report above, to stdout.
- **`--json`**: a versioned object — `{ schema_version, trading_day, baseline_day, run_at,
  total_value_cents, net_post_tax_cents, total_delta_cents, positions: [{symbol, shares,
  price_cents, value_cents, delta_cents, degraded}], tax: {year, accrued, moved, outstanding,
  next_period, brackets_state}, stale, degraded_symbols }`. `schema_version` lets consumers gate
  on shape changes (no recreating the legacy scrape fragility).
- **`dailies`**: the framework calls `summary` (text) in place of `portfolio-summary.sh`; the old
  script and its `portfolio-history.json` are retired once this lands.

## Tax Line & Net Under Stale / Cold-Start Brackets

The header Net `[est]` and the tax line follow `config`'s bracket state (`Verified | Stale |
NoBracketsAvailable`):

- **Verified** → numbers shown plainly.
- **Stale** → numbers shown, marked `[est, brackets stale]`, nudging a refresh.
- **NoBracketsAvailable** (cold-start) → Net shows `n/a (no brackets)` and the tax line says
  "set up tax brackets" — never a fabricated reserve or net.

## Exit Codes & Failure Behavior

- **0** — a summary was produced: fresh, **or** stale (offline / lock-held), with staleness
  marked in-band (text marker / `stale: true` in JSON). A routine offline run is not an alert.
- **2** — no trustworthy summary could be produced: `reports`' History integrity flag (corrupt /
  edited non-reconstructable series), missing/invalid credentials, or an unreadable cache.
- A capture append that fails mid-run is **not** fatal: `summary` prints the uncaptured-delta
  summary, notes the failed append (retried next run; last-wins → no duplicate), and exits 0.

## Verification / Invariants

`summary` is orchestration and formatting — **unit-tested**, not Verus-proven. Worth asserting:
the baseline is the most recent strictly-prior trading-day point (never today itself); the normal
delta reconciles (`header − delta = baseline`); an incomplete or uncaptured operand yields a
flagged delta; offline degrades to a stale exit-0 print; integrity/creds failures exit 2.

## Interfaces

- **Inbound:** via `runtime` (which drives replay, holds the `Snapshot`/marks, and owns the lock):
  `reports` (snapshot append, value series, composition), `tax` (reserve status, the quarterly
  report's next estimated-payment period, unrealized estimate, bracket state), `ledger-core`
  `Snapshot`.
- **Outbound:** stdout (text or `--json`) for `dailies`; exit code per the contract.
- No writes except via `reports.append_snapshot` (which carries `store`'s write discipline).
- **Entrypoint ownership:** the `pt` binary (`crates/pt`) is the process entrypoint; `summary`
  owns the **headless** `pt summary` contract — argv (including `--json`) → rendered stdout →
  `ExitCode` per the exit-code contract — while `runtime` owns the **composition root** that
  constructs and wires `summary`'s inputs (Store / AdvisoryLock / GoogleSheetsApi, creds → client
  → store → cycle). `tui` owns the separate interactive entrypoint.

## Decisions & Alternatives

| Decision | Chosen | Alternatives Considered | Rationale |
|----------|--------|------------------------|-----------|
| Delta operands | Both History points (current = today's appended, baseline = most recent strictly-prior) | Live-current vs prior; "second row" | Point-to-point reconciles (`header − delta = baseline`) and "strictly-prior" survives a failed append; live-vs-history wouldn't reconcile. |
| Headline date | The displayed value's **trading day** (run time secondary) | Calendar run date | A run-date headline with a trading-day delta mislabels which day a move belongs to (weekends). |
| Exit codes | 0 = produced (fresh/stale, marked); 2 = no trustworthy summary | All exit 0; many codes | dailies must see real failures (corrupt non-reconstructable History) but not alert on routine offline. |
| Suppressed delta | Render `—‡` with a footer reason; distinct from first-run `—` | Print the raw number; blank | The policy forbids a fabricated move; an undefined rendering would print one anyway. |
| `--json` | Explicit, `schema_version`-gated object | Ad-hoc "same figures" JSON | An unversioned shape recreates the legacy scraping fragility. |
| Concurrency | Advisory write-lock; read-only if held | Assume single writer; tolerate races | A cron run plus an open TUI is a real concurrent-writer case despite the single-user non-goal. |
| Tax line under stale/cold-start | Mark stale; `n/a (no brackets)` on cold-start | Silently use last/zero | A confident net on expired or absent brackets is exactly the staleness the HLD says to surface. |

## Open Questions & Future Decisions

### Deferred
1. **Focus / short-term section.** The legacy "focus positions" block — whether to carry it
   forward and how to designate focus (a tranche tag? a `config` watchlist) — is not modelled.
2. **Positions ordering/truncation.** All vs top-N by value/Δ, and sort order.
3. **Alert thresholds.** Surfacing positions crossing a threshold (ties to `sheets-view`'s
   goal/alert Open Question).

## References

- Replaces: `../dailies/lib/portfolio-summary.sh` (and its `portfolio-history.json`).
- Inputs: `docs/intent/reports/reports-design.md` (History append, value series, incomplete-point flags),
  `docs/intent/tax/tax-design.md` (reserves, bracket state), `docs/intent/config/config-design.md`
  (bracket `Verified | Stale | NoBracketsAvailable`), `docs/intent/ledger-core/ledger-core-design.md`.
