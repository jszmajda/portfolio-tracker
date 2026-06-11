---
parent: tui
prefix: TUI-VIEW
---

# views

## Context and Design Philosophy

`views` is the **read-only rendering** side of the TUI — it renders `ledger-core`'s `Snapshot`,
`reports`' analytics, and `tax`'s accruals/reserves in the Ledger language, with filtering,
sorting, and drill-down. It **mutates nothing**; where an action is appropriate (act on an
accrual, correct a position), it *launches* the corresponding `entry` flow and returns. It
computes nothing — every number comes from a kernel or `reports`.

Screens (facets): **Positions/Composition** (`TUI-VIEW-POS`), **Open Lots** (`TUI-VIEW-LOT`),
**History / value-over-time** (`TUI-VIEW-HIST`), **Tax & Reserves** (`TUI-VIEW-TAX`), **Realized**
(`TUI-VIEW-REAL`), over a shared **navigation** model — filter/sort/drill-down/group
(`TUI-VIEW-NAV`).

`views` is UI/IO — integration-tested, not Verus. All display conventions (freshness, degraded,
`[est]`, integrity-block, colour, motifs) are the `tui` sub-HLD's; `views` applies them.

**Colour-depth degrade ladder.** `views` is the leaf that makes the parent's colour-degrade
convention a testable requirement (the conventions are shared, but a testable EARS must live in a
leaf, and rendering is the read side's job). It maps the Ledger role tokens down a fixed ladder by
the terminal's detected depth — **truecolor → 256 → 16 → none**: a 24-bit role colour resolves to
its **nearest 256-palette index**, a 256 value to the nearest of the 16 ANSI roles, and at **none**
(the `NO_COLOR` / `--no-color` / non-tty path) the roles render with their always-paired glyph/word
alone. Every rung preserves the redundant non-colour signal, so meaning never depends on a colour
that degraded away.

## Screens

- **Positions / Composition** — the landing screen and live glance: per symbol, ticker ·
  **company name** · shares · price · market value · **total cost basis** · pre-tax unrealized ·
  **gain % of basis** · post-tax `Net` `[est]` · **day change ($ and %)** vs the prior captured
  trading day · trend strip, with each row's **share %** of the priced portfolio. The company name
  comes from `config`'s symbol→display-name map (the owner does not always recognize tickers);
  an unmapped symbol shows the ticker itself, and a platform group row leaves the name blank. A **grouping toggle** re-pivots by
  `platform` (or symbol), showing per-group subtotals; the priced-coverage caveat (degraded count
  + priced-basis fraction) rides the header. When `reports` returns the priced total ≤ 0, the
  share column renders **`n/a`** in `reports`' wording (never a signed or computed share),
  distinct from the degraded `‡` marker.
  - **Basis & gain %.** Basis is reconstructable, so it renders even on a degraded row. The
    gain-% cell is `reports`' pre-tax-unrealized ÷ basis (the hand-wired sheet's unr-% at a
    glance): the `—` dash when the row is degraded, and **`n/a`** (in `reports`' wording) when
    basis ≤ 0 — a closed-out or zero-cost position has no meaningful %-of-basis, consistent with
    `reports`' refusal to emit a nonsensical basis share.
  - **Net.** The row's `Net` is the **post-tax value** — market value less `tax`'s estimated
    unrealized tax (equivalently basis plus `reports`' net-of-tax unrealized): the
    keep-if-sold-today figure, one semantics with the band's `NET (POST-TAX)` and `summary`'s
    headline Net, and the per-position net the legacy sheet carried. The estimate clamps at
    zero, so a position at or below its basis owes nothing and its `Net` **equals its market
    value** — an underwater vest-FMV RSU position never re-taxes value already taxed as W-2
    income at vest. The pre-tax P&L story belongs to the `UNREAL` and `GAIN%` columns, never
    to `Net`. The `—` dash when degraded; a platform group row carries the group's reconciled
    sum. `Net` is reconciliation money — exact whole dollars (see Money formatting).
  - **Trend strip.** Each symbol row carries a fixed **12-cell trend strip**: the symbol's
    captured value series over the most recent twelve captured trading days as the
    `▁▂▃▄▅▆▇█` ramp, scaled between the window's min and max, in the series' net-direction
    tint with the **latest captured cell brightened-bold** (the today tick), and the cells
    for trading days **not yet captured right-padded with dim `·` dots** — a young book reads
    as a timeline deliberately filling in, never a one-block stub that looks broken. Zero
    captures render the full dot strip; a single capture renders one mid-rung tick (no trend
    — flat tint) before its dots; an all-equal window renders the **mid rung** (a flat line
    mid-strip, not a bottom-scraping floor). A platform group row renders no strip — no
    single per-symbol series, the same honesty as its day-change dash.
  - **Day change.** `reports`' per-symbol value delta between the **latest prior captured
    History point** and the **current** values — today's actual move, not the last two captures
    (the latest capture can lag the live marks). Honesty is **per symbol**: a symbol with no
    priced value at either endpoint renders the dash for its row alone, so a partial prior
    (one perpetually-unpriced symbol) degrades only the rows it actually lacks, never the whole
    column. A **zero-key capture is garbage** (a zero trading-day key means *no priced day*)
    and is never selected as the prior — it never enters the series at all. The `—` dash until
    a prior capture exists, on a degraded row, and on a platform group row (no single
    per-symbol series). The % is relative to the prior value and rides the same delta segment.
  - **Summary band.** The glance band above the rows, under the double-entry rule: `TOTAL VALUE`
    (with the whole-book Δ vs the prior complete captured day), `NET (POST-TAX)` `[est]`, and
    `REALIZED <year> YTD` — the current tax year's calendar-year-to-date realized P&L, the
    realized-gain companion to the Tax & Reserves figures that accrue from it. Every band figure
    comes from `reports` (`build_series_point`; the realized history) and renders **exact
    whole dollars, never abbreviated** (the band is reconciliation money — see Money
    formatting). The band's Δ still requires a **complete** prior capture: a partial prior
    *total* cannot source an honest book-level delta, unlike the per-symbol day-change column.
    A fully-unpriced book renders the dash for the mark-dependent lines, never `0`, and a
    partial total carries `‡` — but the realized line is reconstructable-exact: a year with no
    sales renders the flat `· $0`, never a dash.
- **Open Lots** — per open lot: lot id · symbol · **company name** (the same display-name map as
  Positions) · acquire date · source (Buy/Vest) · term · remaining qty · basis · basis/share ·
  platform · tracking code. The drill-down target from a position.
- **History** — value-over-time: a **multi-row block chart** of the trading-day value series with
  a table beneath. Chart columns scale between the series **min and max**, which render as axis
  labels on the chart's bottom and top rows, with a **date-span line** beneath the chart; the
  chart is tinted by net direction with the **latest captured column brightened-bold** (the today
  tick). In-band a **gap is a blank column/cell** and an **incomplete day a `‡`-tinted
  column/cell** (degraded role), axis labels secondary; value (split-neutral) is the axis.
  Degenerate series render explicit states — **0 points** `no history yet — captures begin on the
  first priced run`, **1 point** a single tick (no trend), **all-incomplete** the chart suppressed
  with a flagged table.
- **Tax & Reserves** — accruals with the lifecycle **stepper** (`◉◉◉○`; a de-minimis accrual shows
  `✓ settled`, an orphaned one `⚠ undone`, per the parent conventions), grouped by `(jurisdiction,
  tax_year)`; reserve balances (accrued / moved / paid / outstanding / shortfall); and the next
  estimated-payment period. A provenance line states these are *estimated tax on realized gains, by
  jurisdiction — not the gains themselves (see Realized)*. This screen launches the `entry` accrual
  actions on a selection (passing identity).
  - **Next-period data source (contract).** The next estimated-payment period is **not computed by
    `views`** — it enters the **view bundle from `runtime`**, computed from `tax::quarterly_report`
    + `quarter_of(today)` (the same figure `summary`'s tax line carries). `views` renders the
    bundle field; it does not call `tax` itself. This mirrors `summary`'s next-period feature so the
    interactive and headless surfaces show one number.
  - **Reserve summary and next-period stay whole-book.** Unlike the per-screen filter (`/`, which is
    visibility-only over the **whole priced portfolio** for positions), the
    **reserve-balance summary and the next-period figure are computed over the whole book and do
    *not* narrow with an active tax-year/state filter**. Filtering the accrual rows below changes
    which accruals are *listed*, never the summary or next-period totals — so the reserve a glance
    reads is always the true obligation, not a filtered slice.
- **Realized** — realized-gain history from `reports`, by **calendar** year/range (labelled
  calendar — distinct from `tax`'s estimated-period quarterly report), filterable, as `│`-ruled
  ledger columns: period · proceeds · basis · gain. A provenance line states these are *raw
  realized gains, calendar-grouped — not tax (see Tax & Reserves)*.

```
╭─ History · AMZN+all ──────────────────────────────── as of Fri Jun 5 ─╮
│  $1.29m ┤                                          ▁▂▃▄▆▇█             │
│         ┤                              ▁▂▃▄▅▅▆▇                        │
│  $0.90m ┤      ▁▂▂▃▃▄▅▆▆▇      · gap ·                                 │
│         └────────────────────────────────────────────────────────────│
│          Jan        Mar        May ‡incomplete    Jun                  │
╰────────────────────────────────────────────────────────────────────────╯
```

## Line Rendering

Screen lines render as **spans** — per-segment role and emphasis — so one line can carry the
parent's motifs without flattening them to a single colour: a tracked-caps label renders faint
beside its **bold** figure; the accrual stepper's dots carry the lifecycle ramp colour while the
row text stays fg; a delta segment carries its gain/loss role inside an otherwise-plain row; the
`[est]` qualifier rides in the estimate role; the dim `│` column rules stay chrome; and a
trend strip's **latest captured cell is brightened** (the "today" tick) over its direction
tint, its not-yet-captured dot padding staying faint chrome.

**Column sizing is data-driven.** Each column's width is computed from the rendered cells — the
maximum cell width over the visible rows, under per-column min/max clamps — never hand-padded
constants, so a five-digit share count or a six-figure value widens its column instead of
shearing the table. Numeric columns right-align (per-share money's fixed two decimals align;
fractional share counts align on the decimal point), share counts carry thousands separators, a
long symbol is ellipsized **only when its clamp forces it**, and a degraded figure's `—` dash
sits inside its own column so neighbouring columns never shift.

**Money formatting.** View money is whole-dollar — cents round half-to-even away (they are not
significant at portfolio scale) — and the **glance cells compact at magnitude**: below $1,000
whole dollars (`$842`), below $1m tenths of a thousand (`$123.5k`), at and above $1m hundredths
of a million (`$2.65m`), with the sign and delta glyph cues riding the figure unchanged. The
compact cells are Positions `VALUE` / `BASIS` / `UNREAL` / `DAY Δ`, Open Lots `BASIS`,
Realized `PROCEEDS` / `BASIS` / `GAIN`, and the History axis labels and table values. Two
families never compact: **per-share money keeps cents** (`PRICE`, `$/SH` — a $0.04 OTC mark
vanishes without them), and **reconciliation money stays exact whole dollars** — the summary
band's figures, the Tax & Reserves amounts/reserve balances, and the Positions `NET` column,
the figures the owner reconciles against statements, real transfers, and the legacy sheet's
per-position net. `entry` is the write side and keeps cents throughout (a field re-parses what
it shows).

**Column title rows.** Every columnar screen — Positions, Open Lots, Tax & Reserves, Realized —
opens its data block with a **title row**: tracked-caps column titles in `fg-faint` on the *same*
grid as the rows (the `│` rules at identical positions). A title is a rendered cell of its
column: it participates in the column's data-driven width computation under the same clamps, so
a title never shears the grid and a narrow column widens just enough to carry its name.

**Semantic colour on figure columns.** The gain/loss roles ride the *figures*, not just their
glyphs: a row's pre-tax unrealized, gain-%-of-basis, and day-change cells — and the Realized
screen's gain cells and the summary band's deltas — render in the gain/loss/flat role matching
their sign, with the leading `+`/`−` (and `▲`/`▼`/`·` where the delta treatment applies) as the
always-paired redundant cue. `[est]`-marked figures — the row's post-tax net and the band's
`NET (POST-TAX)` — render in the estimate role beside their qualifier. A degraded cell's `—`
stays in the dim degraded role; it never borrows a gain/loss tint.

**Status-line key hints.** The status line's right edge carries the **active context's key
hints** — bracketed keys in accent, hint words in fg-faint (`[tab] entry  [r] refresh  [?] help
[q] quit`). The hints follow the active context: a drilled frame offers `[esc] back`, the idle
Entry panel its flow-launch keys, an open composer its field keys, a lot picker its row keys.
Only **live** keys are hinted — a key the active context does not bind never appears. While a
field is in text-input mode the hints are suppressed entirely (the field owns every keystroke;
the line must not advertise keys the field is swallowing).

## Help Overlay

`?` — outside text-input mode — opens a modal **help overlay**: a calm centered panel over the
current screen, gilt title, and a two-column key/action listing of the full keymap grouped by
context. The first group is **screens** — each screen named beside the key that reaches it
(`1` Positions … `5` Realized) — so navigation is discoverable from the overlay alone; the
remaining groups are **global / views / entry / lot picker (inside a Sell)** (the picker group
names its host flow, so a reader who has never opened a Sell knows where those keys live).
`esc` or `?` dismisses it. The overlay is chrome: it mutates nothing, launches nothing, and
renders from the keymap alone — the glance-depth companion to the status line's live hints.

## Navigation (`TUI-VIEW-NAV`)

A shared interaction model with two distinct scoping layers — a **contextual scope** set by
drilling and a **user filter** typed on a screen:

- **Screen switch** (`1`–`5`): in Views mode the number keys jump directly to a screen —
  `1` Positions · `2` Open Lots · `3` History · `4` Tax & Reserves · `5` Realized — from any
  Views frame, drilled or not. A switch **replaces the screen stack** with the target's landing
  frame (a drilled scope belongs to the drill that set it and dies with it) while each screen's
  own filter / sort / grouping are **retained across switches** (the same per-screen retention
  ascend honors), so returning to a screen finds it as it was left. Like all navigation, a
  switch mutates nothing durable. The status line hints `[1-5] screens` on every Views screen.
- **Filter** (`/`): a user filter by symbol, platform, term, state, or tax year. It changes
  **visibility only** — share % and totals stay over the **whole priced portfolio**, never the
  filtered subset. A **filter-state header** states the scope so the non-summing column reads
  honestly: `showing 3 of 12 · 18% of book`.
- **Group**: a toggle re-pivots Positions/Composition by symbol vs platform. A group's subtotal
  always reflects the **full group**, even with a filter active (the filter hides detail rows; the
  subtotal stays whole and is annotated) — so a subtotal never rebases.
- **Sort**: by any column, stable tie-break. A row sorts to the **tail only when the *sort key
  itself* is degraded** for that row (in both directions — degraded always at the tail); a row
  degraded in some *other* column sorts normally on a non-degraded key.
- **Drill-down**: `enter` descends, `esc` ascends, on the screen stack — Position → its Lots → a
  lot's realized history; an accrual → its sale → the realized gain. Drilling sets a **contextual
  scope** (a breadcrumb chip, e.g. `AMZN ›`) **cleared on ascend**; the **user filter and sort are
  per-screen and retained** across ascend, composing *on top of* the contextual scope.
- **Focus anchored to identity.** A refresh or re-sort keeps the selection on the same row
  *identity* (symbol / lot id / accrual key), not the row index, with a stable scroll offset — so
  new marks reordering a value-sorted list never move the selection out from under the owner; if
  the focused identity vanished, focus falls to the nearest row.

## Refresh & Staleness

`views` renders from the current `Snapshot` + marks. A **refresh** re-reads marks (via
`sheets-view`) and re-renders; between refreshes, live figures carry their freshness context, and
when offline the screen is **stale-marked** (last quote-epoch) per the conventions. An **integrity
error** from `store`/`reports` blocks the affected screen with the loud error treatment rather
than rendering from a corrupt source.

**Ambient hourly auto-refresh.** While the TUI is open, the shell triggers the same refresh as
the manual `[r]` binding once roughly an hour has passed since the last **successful** refresh —
manual, automatic, and the post-submit republish all advance the clock, and the connect cycle
that produced the opening view seeds it. The cadence is entirely the shell's (the `tui`
sub-HLD's clock rule): the input loop polls on a short tick and compares elapsed wall-time
against the last successful refresh; the Model never reads a clock — the decision reaches it as
data. An **open entry composer suppresses** the trigger (a mid-composition reflow would yank
fields and focus out from under the owner); the overdue refresh runs on the first tick after the
last entry context closes, back on the Views side of the loop. The **help overlay does not
suppress** it — the overlay is chrome rendered over the current screen from the keymap alone, so
a refresh beneath it holds. A **failed auto-refresh degrades exactly like a failed manual one**
— the view stale-marks offline and the `updated HH:MM` stamp stays the last successful run's —
and because a failure does not advance the last-successful clock, the shell retries each tick
until a refresh lands, so recovery is immediate when connectivity returns. Auto-refresh is
ambient: it carries no status-line hint and no help-overlay entry — there is no key to teach —
and the `updated HH:MM` stamp is its visible trace.

**Human freshness stamps.** The masthead's right-aligned as-of renders the priced trading day
as a **calendar date** (`as-of 2026-06-09`) and the status line carries **`updated HH:MM`** —
the wall-clock time of the run/refresh that produced the view. Both are formatted strings
threaded into the view bundle by the binary (the key→calendar conversion and the wall clock are
the binary's; `views` renders, it does not derive). A book with no
priced trading day (no key, or a zero key) reads **`no priced day yet`** — a raw trading-day key
integer never reaches the screen — and a view with no run time omits the `updated` segment
rather than fabricating one.

**Estimate-qualifier seam.** The `[est]`-family qualifier on every estimate figure renders from
the view bundle's **bracket state**, which is `config`'s resolved state for the **current tax
year**, threaded by the binary — `Verified` → `[est]`, `Stale` → `[est, brackets stale]`,
cold-start → `n/a (no brackets)`. `views` renders the bundle field; it never resolves brackets
itself. A book whose current year has verified brackets must therefore never shout
`n/a (no brackets)` beside figures the verified context computed.

On refresh (or a concurrent `entry` append), each stacked screen's **anchor identity is
re-resolved**; if a drilled-into lot or accrual no longer exists or has closed, that frame is
replaced with a calm `this lot was closed by a newer entry — returning to <parent>` notice rather
than a stale or empty render.

**Refresh lifecycle ordering.** A refresh is one deterministic transition through the shell's
`update` loop (the `tui` sub-HLD), and its four steps run in a **fixed order** so the screen never
renders against half-updated state: (1) **re-read marks** via `sheets-view` (or fall to the cached,
stale-marked set when offline); (2) **re-resolve each stacked frame's anchor identity** against the
refreshed `Snapshot` (a dangling frame becomes the calm returning-to-parent notice); (3)
**re-anchor focus** to the surviving row identity, falling to the nearest row if it vanished;
(4) **re-render** the frame. Re-anchoring focus before the render
(step 3 before 4), and re-resolving anchors before re-anchoring focus (step 2 before 3), is what
keeps new marks reordering a value-sorted list from moving the selection out from under the owner.

## Empty States

Six calm, distinct empty states — none using the `✗` integrity treatment:

- **No positions** (fresh / pre-import) — `no positions yet — add activity in Entry, or run import`.
- **No open lots** — `no open lots — every lot has been fully sold` (the Open Lots screen with all
  lots closed; distinct from No positions, which is the whole-book pre-import state).
- **No accruals** — `no tax accruals yet — they appear when you record a sale`.
- **No history** — `no history yet — captures begin on the first priced run`.
- **No realized gains** — `no realized gains yet — they appear when you sell a lot` (the Realized
  screen with nothing closed; distinct from No positions, the whole-book pre-import state).
- **Filter matched zero** — `no rows match — [clear filter]`.

## The views ↔ entry Boundary

`views` is strictly read-only. Selecting an actionable item (an accrual to allocate/move/pay, a
position to correct) **launches the matching `entry` flow**, passing only the selection's
**identity** (e.g. the accrual key) — `entry` **re-resolves it live** at flow start, never the
rendered, possibly-stale values. Launching while **offline or lock-held is allowed**: `entry`'s
write loop is the single rejection path (return-control + retry), and `views` surfaces the
offline/lock state so it is not a surprise. On return, `views` re-renders. The boundary is clean:
`views` shows and navigates; `entry` mutates.

## Decisions & Alternatives

| Decision | Chosen | Alternatives Considered | Rationale |
|----------|--------|------------------------|-----------|
| Filter semantics | Visibility only; % over the whole priced portfolio | Rebase % to the filtered subset | A filter that silently rebases percentages misleads; visibility-only keeps the allocation honest. |
| Grouping vs filtering | Group re-pivots with subtotals; filter only hides | One mechanism for both | They answer different questions ("how is it split" vs "show me these"); conflating them muddies %. |
| Degraded sort | Degraded rows sort last | Sort as zero / hide | A degraded value isn't zero; sorting it as zero or hiding it distorts the ranking. |
| Drill-down state | Filter/sort retained per screen across ascend | Reset on navigation | Returning to a list you'd filtered and finding it reset is hostile; retain it. |
| Mutation | None — launch `entry` flows | Inline edit in views | Keeps the read/write split the sub-HLD draws; one durable write path. |
| Realized periods | Calendar (labelled), distinct from `tax` quarterly | Reuse `tax`'s IRS periods | Two "quarterly" numbers confuse; `reports` already owns the calendar view. |
| Filter-state header | Show `shown/total · % of book` | Leave the non-summing column unexplained | A %-column not summing to 100 reads as a bug; the header makes the subset explicit. |
| Drill scope vs filter | Two layers: contextual scope (breadcrumb, cleared on ascend) + retained user filter | One filter for both | Conflating either strands you in a context or wipes your filter. |
| Refresh identity | Re-resolve stacked anchors; focus anchored to identity; dangle → calm notice | Re-render by index | Index-based refresh dangles drilled frames and jumps the selection mid-read. |
| Launch identity | Pass the selection's identity; `entry` re-resolves live | Pass rendered values | Rendered values can be stale → a wrong confirmed write. |
| Reserve/next-period vs filter | Whole-book — summary + next-period ignore the active tax-year/state filter (only listed accruals narrow) | Narrow the summary with the filter | The reserve a glance reads must be the true obligation, not a filtered slice; narrowing it would understate what is owed. |
| Next-period source | Enters the view bundle from `runtime` (`tax::quarterly_report` + `quarter_of(today)`); `views` only renders | `views` calls `tax` itself | `views` computes nothing; one number across the interactive and headless (`summary`) surfaces. |
| Colour-depth degrade | Fixed ladder truecolor→256→16→none, nearest-index map at each rung, glyph/word always kept | Render only at truecolor; hard-fail lower depths | The owner runs truecolor, but a degrade ladder keeps the UI legible over SSH/CI and is cheap given role-token theming. |
| Refresh ordering | Fixed re-read → re-resolve anchors → re-anchor focus → re-render | Render then fix up focus | Re-anchoring before render is what stops new marks moving the selection mid-read. |
| Line seam | Spans (per-segment role/emphasis) | One role per line | A single-role line cannot render a faint label beside a bold figure or ramp-coloured dots beside fg text — the parent motifs demand per-segment styling. |
| Column widths | Computed from the data (max cell per column, min/max clamps) | Hand-padded fixed widths | Fixed pads shear on a five-digit share count or a six-figure value; data-driven widths keep the ledger columns true. |
| Stepper colour placement | The dot run carries the ramp; the row text stays fg | Colour the whole accrual row | A whole-row wash drowns the key and amount; the dots are the motif, so the colour rides them. |
| Status hints | Only live keys hinted; suppressed in text-input mode | Hint the full keymap always | Advertising an unbound or swallowed key teaches a dead reflex; the help overlay carries the full map. |
| Help surface | A modal overlay (chrome), dismissed by `esc`/`?` | A help screen on the stack | A stack frame implies navigation state to re-resolve on refresh; help is a glance, not a place. |
| Day-change source | `reports`' per-symbol value delta: the **latest prior captured** History point vs the **current** values, honesty per symbol (a symbol unpriced at either endpoint dashes its row alone) | The last two captured points; skip incomplete priors to an older complete one | The latest capture can lag the live marks — prior-vs-current reads today's actual move. Skipping an incomplete prior silently widens the "day" window and one perpetually-unpriced symbol would blank the whole column forever; per-symbol endpoint honesty degrades only the rows actually lacking a value. |
| Zero-key History points | Excluded from the bundled series entirely (never a chart column, never a day-change prior) | Render them; exclude only from the prior selection | A zero trading-day key means *no priced day* — a garbage capture; charting it stretches the axis to day 0 and selecting it as a prior fabricates a delta against nothing. |
| View money formatting | Whole-dollar everywhere; glance cells compact (`$123.5k` / `$2.65m`); band + tax money + the row `NET` exact whole dollars; per-share money keeps cents | Keep two-decimal cents everywhere; compact every cell including totals; compact `NET` with the other row cells | Cents are noise at portfolio scale, but an abbreviated reserve can't be reconciled against a real transfer and a sub-dollar mark vanishes without cents — the three families match how each figure is read. `NET` is the keep-if-sold-today figure the owner scans and reconciles against the legacy sheet; a `$142.7k` blur defeats that read. |
| Row Net semantics | Post-tax **value**: market value − estimated unrealized tax (= basis + `reports`' net-of-tax unrealized) | Post-tax unrealized P&L (`reports`' net-of-tax figure raw) | One word, one meaning: the band's `NET (POST-TAX)` and `summary`'s headline Net are post-tax value, and the owner reads `Net` as keep-if-sold-today. The P&L story already has `UNREAL` and `GAIN%`; a same-named column with different semantics misreads as a bug. An underwater FMV-basis position clamps to zero tax, so `Net` = market value — never the legacy $0-basis re-tax. |
| Trend strip | Fixed 12-cell strip: captured ramp from the left, dim `·` dots for not-yet-captured days, mid rung for a zero-span window, today tick kept | Variable-width sparkline (one cell per capture); suppress the strip below 3 points | One or two ramp cells read as a rendering bug, and suppression makes a young book look unfinished; the dot-padded fixed strip reads as a timeline filling in and keeps every row's strip the same width on the grid. Min/max window scaling matches the History chart's axis. |
| Estimate-qualifier source | The bundle's bracket state = `config`'s resolved state for the current tax year, threaded by the binary | `views` resolves brackets itself; a hardcoded conservative cold-start default | `views` computes nothing, and a conservative default is dishonest in the other direction — verified-bracket figures annotated `n/a (no brackets)` teach the owner to ignore the qualifier. |
| Gain-% denominator | Pre-tax unrealized ÷ basis (`reports` computes); `n/a` on basis ≤ 0, dash when degraded | % of market value; hide the cell on odd rows | %-of-basis is the unr-% the owner reads off the legacy sheet; a ≤ 0 basis has no meaningful %, and a hidden cell would shear the grid. |
| Realized-YTD zero | Exact flat `· $0` when the year has no sales | The `—` dash | The dash means *unknowable* (mark-dependent); realized P&L replays from the log — zero is the true figure. |
| History chart | A fixed-height multi-row block chart on the spans seam | ratatui's `Chart` widget; keep the one-row sparkline | The spans seam is the testable seam and carries the per-column roles (gap / incomplete / today tick); the `Chart` widget draws outside it, and one row cannot show min/max scale. |
| Column titles vs widths | The title is a rendered cell of its column (participates in the data-driven width under the clamps) | Exclude titles and ellipsize them to the data width; hand-pad title rows | An excluded title shears or truncates on narrow data; participation keeps one rule ("widths come from the rendered cells") and the grid true. |
| Auto-refresh cadence & ownership | Hourly, measured from the last successful refresh; decided in the shell over a short input-poll tick; the Model reads no clocks | A background tick thread posting refresh events; the Model tracking timestamps itself | The poll timeout adds no thread or channel to the hand-rolled loop, and a clock-free Model keeps the cadence decision pure and unit-testable; hourly is plenty for a glance instrument whose live prices already ride `GOOGLEFINANCE`. |
| Auto-refresh suppression scope | Only an open entry context suppresses; the help overlay does not | Suppress under the help overlay too; never suppress | A mid-composition reflow yanks fields and focus out from under the owner, so composition wins; the overlay is chrome over the current screen (it anchors to no data row), so a refresh beneath it holds and staleness need not accumulate behind a help glance. The overdue refresh runs on the first tick after the last entry context closes. |
| Failed auto-refresh retry | Degrade exactly as a failed manual refresh; the failure leaves the last-successful clock unadvanced, so the next tick retries | Back off a full hour after a failure; an exponential backoff | An hour-long backoff leaves the book stale for up to an hour after connectivity returns; the offline cycle fails fast, the degrade path is already calm, and tick-retry makes recovery immediate. |
| Auto-refresh visibility | Ambient — no status-line hint, no help-overlay entry; the `updated HH:MM` stamp is the trace | Hint an `auto 1h` segment on the status line | Hints teach keys, and there is no key here; the stamp already shows when the rendered view was produced. |
| Screen-switch keys | Number keys `1`–`5`, Views mode only | Letter mnemonics (`p`/`o`/`h`/…); a screen-cycle key | Letters collide with present and future single-key binds (`r` refresh, the Entry launch set); numbers are collision-free, ordered like the screens, and read as a row in the help overlay. |
| Screen switch vs stack | Replace the stack with the target's landing frame; retain per-screen filter/sort/grouping | Keep drilled frames across a switch; reset nav state on every switch | A drilled scope is meaningless on another screen's stack, but a filter the owner set is theirs — the same retention ascend honors. |
| Name column source | `config`'s symbol→display-name map, ticker fallback, blank on platform rows | `GOOGLEFINANCE` name lookups; hardcode names in the TUI | Names are owner-edited reference data like aliases; a live lookup adds an oracle dependency for static text, and an unmapped symbol degrading to its ticker is honest. |
| Freshness stamp seam | The binary threads formatted `as-of` calendar / `updated HH:MM` strings through the view bundle | `views` derives calendar dates / wall-clock itself | The key→calendar conversion and the clock already live in the binary's wiring; `views` computes nothing, and one formatter keeps the TUI and headline stamps identical. |

## Open Questions & Future Decisions

### Deferred
1. **Multi-series chart overlay.** Overlaying per-symbol value series on the History chart — a
   later UX pass.
2. **Large-table paging.** Scrolling vs paging for long lot/history/realized lists, and any
   virtualization.
3. **Saved views.** Whether frequently-used filter/group combinations can be saved/named.

## References

- Parent: `docs/intent/tui/tui-design.md` (conventions, Ledger aesthetic, nav shell).
- Sibling: `docs/intent/tui/entry/entry-design.md` (the flows `views` launches).
- Sources rendered: `ledger-core` (`Snapshot`), `reports` (composition, value-over-time, realized),
  `tax` (accruals, reserves, unrealized estimate).
