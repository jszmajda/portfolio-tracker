---
parent: high-level-design
prefix: TUI
---

# tui

## Context and Approach

`tui` is the terminal application — the **only** way activity is entered and the primary way the
portfolio is read day to day. It is a **sub-HLD**: it owns no EARS of its own and delegates the
specifics to two child leaves, but it holds the shared intent that makes them one tool — the app
shell, navigation, and the conventions for how every screen renders freshness, degradation,
estimates, failures, and confirmations.

The split exists because the two halves are genuinely different intents: **entry** is interactive
*mutation* through the verified write path; **views** is read-only *rendering* of the kernels'
output. What they share — the shell and the display/interaction conventions below — is
substantial enough to live in one parent rather than be duplicated or drift between them.

## Children

| Child | Prefix | Owns |
|---|---|---|
| `entry` | `TUI-ENTRY` | Buy / Vest / Sell / Split / Reversal entry, tax-accrual actions (allocate / move / pay / override), residency & config/platform/alias edits — all through the write-path loop. |
| `views` | `TUI-VIEW` | Composition, value-over-time, tax/reserves, and realized-history screens, with filtering and drill-down. |

## App Shell & Navigation

- A keyboard-driven shell with a top-level switch between **Entry** and **Views**, plus a
  persistent **status line** (workbook connection, last sync / quote-epoch, lock state).
- Screens are pushed/popped on a stack; every screen can be left without committing (entry is
  only durable once read-back-verified — see the write-path loop). Nothing is mutated by
  navigation alone.
- The shell runs **through `runtime`**: `runtime` drives the replay cycle, holds the `Snapshot` +
  cached marks the children render, owns the Sheets-access layer and the advisory lock, and is the
  site interactive submits acquire the lock through. (The headless `summary` and `import` use the
  same `runtime`; the shell is its interactive front.)

## The Input & Update Loop

The shell is a hand-rolled **Model–Update–View** loop over `crossterm` events, sitting outside the
`verus!{}` boundary. It is the **cross-cutting** machinery the two children share; the children own
*what* each context renders and *which keys it binds*, the shell owns *how an event reaches the
active context* and *how a frame is produced*.

- **One event source, one dispatch.** The shell reads `crossterm` events (key, resize, and a
  periodic refresh tick) and folds each into the Model via an `update(Model, Event) → Model`
  transition; after every transition it re-renders the whole frame from the resulting Model
  (immediate-mode). No widget holds mutable state the Model does not.
- **The active context owns the keymap.** The Model carries a screen stack (the parent's
  push/pop model above); its **top frame is the active context**. The shell routes every key
  event to the active context's keybinding table — the **Entry** composer/lot-picker/accrual/
  config contexts (`TUI-ENTRY`) and the **Views** screens + nav (`TUI-VIEW`). A key unbound in the
  active context falls through to the shell's **global** bindings (the top-level Entry⇄Views
  switch, `[/]` filter, `enter`/`esc` stack push/pop, quit, refresh) — never silently swallowed.
- **Modality.** When a context is in a **text-input sub-mode** (a `tui-textarea` field has focus, a
  confirm prompt is up), the shell suppresses global single-key bindings so a typed character is
  never intercepted as a command; the field's own escape (`esc`/`enter`) returns to command mode.
- **Refresh is an event.** A refresh tick (or a completed `entry` append) is folded through the
  same `update` path as a keystroke — it never mutates the Model out of band — so the refresh
  lifecycle ordering the children require (re-read marks → re-resolve stacked anchors → re-anchor
  focus → re-render) is a single deterministic transition, not a race.
- **The shell owns every clock.** The input loop polls on a short timeout (rather than blocking
  on the next event) and raises the ambient refresh tick itself when roughly an hour has passed
  since the last successful refresh; the Model never reads time — elapsed-time decisions reach it
  as data, so the Model stays pure and the cadence decision is unit-testable apart from the loop.
  The trigger's behavior (cadence, suppression while composing, failure/retry) is the `views`
  leaf's requirement, as part of its refresh model.
- **Errors and submits ride the Model.** A submit's outcome (confirmed / returned-control-with-
  retry / lock-held) and an integrity block are Model state the next View renders; the loop draws
  them, it does not pop dialogs imperatively.

## Cross-Screen Display Conventions

These conventions are the load-bearing shared intent — applied **identically** in `entry` and
`views`, so the same fact never renders two ways:

- **Freshness.** Every figure derived from marks shows its **quote-epoch / as-of** context; data
  served from the cache while offline is marked **stale** with the last quote-epoch — never
  presented as live.
- **Degradation.** Two distinct, consistently-worded cases: a figure whose **mark** is missing is
  **unpriced** (the `‡`/`—` marker, never `0`); a figure whose required **upstream input** is
  missing (e.g. a `tax` estimate that's degraded or `NoBracketsAvailable`) is **degraded** — both
  use the dim role, but the word matches the cause, so a priced-but-tax-degraded symbol is never
  mislabelled "unpriced." A value depending on either is marked, never silently dropped.
- **Estimates.** Post-tax and unrealized-tax figures are labelled **`[est]`**; under stale
  brackets they read `[est, brackets stale]`, and under cold-start (`NoBracketsAvailable`) they
  read `n/a (no brackets)` — the same wording `summary` uses.
- **Auto-settled & orphan accruals.** A **de-minimis auto-settled** accrual renders as a distinct
  **settled** state (not a stuck mid-lifecycle stepper), excluded from actionable selection. An
  **orphaned** accrual (its sale was reversed after Move/Pay — the tax kernel's orphan-no-op rule) renders a `warn`
  **"undone — needs unwind"** badge, distinct from degraded and integrity.
- **Integrity errors.** If `store`/`reports` raise an integrity error (corrupt/edited event log or
  History) **or** `config` reports a creds/unreadable-cache failure — the full *no-trustworthy-data*
  set, matching `summary`'s exit-2 — the TUI **surfaces it loudly and refuses to render derived
  numbers** rather than show figures from a corrupt or unavailable source.
- **Confirmations.** Consequential or corrective actions — a **Reversal**, a **Pay**, a config
  change to tax rules — require an explicit confirm step; ordinary appends do not.

## Color Conventions

Color is **semantic** — it encodes meaning, never decoration — and is governed by one small,
fixed palette used identically on every screen. It is the visual spine of the conventions above:
each display state has a colour *and* a redundant non-colour signal.

| Role | Where it applies | Color | Always-paired signal (color is never the only cue) |
|---|---|---|---|
| `gain` | positive P&L / delta / return | sage green | leading `+` and `▲` |
| `loss` | negative P&L / delta / return | terracotta red | leading `−` and `▼` |
| `flat` | zero / no change | default fg | `·` |
| `stale` | figures served from cache while offline | warm dim | `⟲ as-of <quote-epoch>` |
| `degraded` | a value with no mark | warm dim | `‡` or `—` |
| `estimate` | post-tax / unrealized-tax figures | verdigris | `[est]` (or `[est, brackets stale]` / `n/a (no brackets)`) |
| `warn` | needs attention — brackets stale, a confirm prompt, lock held | amber | `⚠` |
| `error` | integrity failure / untrustworthy data | escalated red (or inverse) | `✗` + a blocking message |
| `accent` | masthead, headers, rules, the status line | gilt | — |

Accrual **lifecycle** uses a *minting* progress ramp: `Accrued` faint → `Allocated` verdigris →
`Moved` gilt → `Paid` sage — so a glance reads how far each tax dollar has travelled.

Principles:

- **Color is redundant, never sole** (tenet below): `gain`/`loss` always carry `+/−` and `▲/▼`,
  so the finance-standard red/green pair is never the only differentiator — legible in monochrome
  and for colorblind users.
- **Honor the terminal.** `NO_COLOR`, a `--no-color` flag, and a non-tty (piped) destination
  disable colour and fall back to the glyphs/words above. Because every screen keys off role
  tokens, an alternate palette is a drop-in if ever needed; the redundant glyphs are the
  always-on safeguard.
- **Chrome stays quiet so data pops.** Only `accent` colours structure; the data roles carry
  meaning. Color degrades down a fixed ladder by detected terminal depth — **truecolor → 256 → 16 →
  none** — at each rung mapping every role token to the best available representation and never
  losing the redundant glyph/word: a 24-bit role colour maps to its **nearest 256-palette index**,
  the 256 value maps to the nearest of the 16 ANSI roles, and at **none** the roles render with the
  always-paired glyph/word alone (the `NO_COLOR` / non-tty path). The ladder is a testable
  requirement, owned by `views` (`TUI-VIEW-NAV`) as the application of this convention.

## Interface Palette & Motifs

The theme is **Ledger** — a *gilt-edged ledger book rendered as a precision instrument*: warm ink
paper, a single gilt/brass accent, double-entry hairlines, cool sage/terracotta semantics so
nothing is neon. **Dark-only** (the owner runs dark terminals exclusively), truecolor, degrading
truecolor → 256. Screens reference **role tokens**, never raw colours, so an alternate palette is a
drop-in if ever needed.

### Palette (truecolor, "Ledger")

Chrome:

| Token | Hex | Use |
|---|---|---|
| `bg` | `#14120E` | warm ink base |
| `bg-panel` | `#1C1A14` | elevated panels |
| `bg-focus` | `#2A2618` | focused row / field |
| `border` | `#3A352A` | panel borders, hairline / double rules |
| `fg` | `#E8E1D3` | primary text (newsprint) |
| `fg-dim` | `#9A8F7A` | secondary / `stale` / `degraded` |
| `fg-faint` | `#5C5444` | tracked-caps labels, chrome |

Data / semantic:

| Role | Hex | Note |
|---|---|---|
| `accent` | `#D4A82C` | gilt / brass — the signature |
| `gain` | `#6FA86B` | sage |
| `loss` | `#C5594B` | terracotta |
| `estimate` | `#5FA89E` | verdigris |
| `warn` | `#E08A3C` | amber |
| `error` | `#E5544A` on `#2A1411` (or inverse) | escalated |

Accrual-lifecycle ramp (a *minting* progression): Accrued `#5C5444` → Allocated verdigris
`#5FA89E` → Moved gilt `#D4A82C` → Paid sage `#6FA86B`.

### Motifs

Recurring patterns, used identically everywhere:

- **Masthead.** The app identity in tracked uppercase `accent` (gilt) at the top — the ledger's
  spine.
- **Panels & rules.** Major regions sit in **rounded thin panels** (`╭─╮ │ ╰─╯` in `border`); the
  **focused panel's border turns gilt**. Section headers underline with a **double-entry rule**
  (`═`); minor splits use a single hairline (`─`).
- **Tracked-caps labels.** Labels render in `fg-faint` uppercase (`TOTAL VALUE`); the figure beside
  them is bold `fg` — the monospace substitute for type hierarchy.
- **Money columns.** Money is right-aligned with thousands separators and `$`, separated by a dim
  `│` column rule — true ledger columns. View money is **whole-dollar** (cents are not significant
  at portfolio scale; rounded half-to-even): **glance cells compact at magnitude**
  (`$842` → `$123.5k` → `$2.65m`) while **reconciliation figures** — the summary band totals and
  the tax amounts/reserves the owner reconciles against statements and transfers — stay exact
  whole dollars, never abbreviated. **Per-share money keeps cents** (a sub-dollar mark like
  `$0.04` vanishes without them), and `entry`'s editable fields keep cents (a field re-parses
  what it shows). Fractional share counts align on the decimal point.
- **Header band.** `accent` title left, right-aligned **as-of** freshness — always the same spot,
  as a human **calendar date** (`as-of 2026-06-09`, never a raw day-key integer); a book with no
  priced trading day reads `no priced day yet`.
- **Status line.** A `·`-segmented bottom line: connection **dot** (`●` connected / `warn` stale /
  `error`), last sync (the calendar as-of), **`updated HH:MM`** (the wall-clock of the run/refresh
  that produced the view), **lock** state, current screen — chrome in `fg-faint`/`accent`.
- **Focus caret.** The focused row carries a left-gutter **`▎`** in `accent` over `bg-focus`.
- **Lifecycle stepper.** An accrual's state is a four-dot stepper coloured by the ramp —
  `◉◉◉○ Moved`, `◉◉◉◉ Paid` — the signature progress motif. A **de-minimis auto-settled** accrual
  shows a distinct `✓ settled` (not a partial stepper); an **orphaned** one shows a `warn`
  `⚠ undone` badge.
- **Sparklines.** Value-over-time and per-symbol trends as `▁▂▃▄▅▆▇█`, tinted by net direction
  (`gain`/`loss`), with the **latest captured cell brightened** (a "today" tick). A per-row trend
  strip is **fixed-width**, right-padded with dim `·` dots for trading days not yet captured — a
  timeline filling in, never a one-block stub.
- **Glyph set (fixed).** `▲▼` delta · `‡`/`—` degraded · `⚠` warn · `✗` error · `⟲` stale ·
  `●/○` status · `◉/○` stepper · `▎` focus · `→` flow. One glyph, one meaning, everywhere.
- **Density.** Calm and airy — consistent padding, one blank line between panels; never cramped.

A screen in this language (columns elided to page width — the live table also carries
BASIS, GAIN%, and SHARE):

```
  L E D G E R                                                      as-of 2026-06-09
╭─ Positions ─────────────────────────────────────────────────────────────────────╮
│  TOTAL VALUE  $1,284,300   ▲ +$8,420  (+0.66%)                                   │
│  NET (POST-TAX)  $902,160   [est]                                                │
│  REALIZED 2026 YTD  ▲ +$48,210                                                   │
│  ══════════════════════════════════════════════════════════════════════════════ │
│   SYMBOL NAME             SHARES │    PRICE │   VALUE │   UNREAL │           NET       │    DAY Δ │ TREND        │
│ ▎ AMZN   Amazon.com, Inc.   620  │  $261.26 │ $162.0k │ +$131.0k │ net $126,545 [est]  │ ▲ +$3.1k │ ▁▂▃▅▆▇······ │
│   GOOGL  Alphabet Inc.       60  │  $376.37 │  $22.6k │    −$410 │ net  $21,940 [est]  │ ▼  −$120 │ ▇▆▅▃▂▁······ │
│   PLTR   Palantir Tech.       —  │ unpriced │       — │        ‡ │              —      │        ‡ │ ············ │
╰─────────────────────────────────────────────────────────────────────────────────╯
  TAX 2026   RSU-GRNT1-a  AMZN  $2,400  ◉◉◉○ Moved      3  GOOGL  $253  ◉◉◉◉ Paid
  ● connected · sync as-of 2026-06-09 · updated 16:02 · 🔓 · Positions
                          [tab] entry  [1-5] screens  [r] refresh  [?] help  [q] quit
```

Reading the vignette against the motifs: the summary band's `TOTAL VALUE` / `NET (POST-TAX)`
take-home / `REALIZED <year> YTD` figures are reconciliation money — **exact whole dollars,
never abbreviated** — while the glance cells below compact at magnitude (`$162.0k`); every
column carries a tracked-caps **title row** on the same `│` grid; the **NAME** column gives
each symbol its company name (ellipsized only when clamped); per-share PRICE keeps cents;
each symbol row ends in the fixed-width **TREND** strip with dim-dot padding; and the status
line carries the as-of sync, the `updated HH:MM` wall-clock, and the `[1-5] screens` hint
among the context's bound keys.

## The Write-Path UX Loop

Every mutation in `entry` rides one shared loop, so the user's mental model is uniform:

```
 compose  →  kernel validates  →  append (store)  →  read-back-verify  →  confirmed
              │ reject: show               │ unreachable / mismatch:
              │ the LedgerError/           │ return control, keep the
              │ TaxError inline,           │ composed entry, offer retry
              │ nothing written            │ (no background queue)
```

This is the user-facing form of the HLD's *durably recorded or still in your hands*: an entry is
either confirmed durable or still editable in front of the owner — never silently lost, never
optimistically shown as saved. `runtime`'s cross-process advisory **write-lock** is acquired
inside the append (try-acquire; on **held**, the submit fails non-destructively with retry), so an
interactive session and a cron `summary` never append concurrently.

## Tenets

- **No number without its qualifier.** Freshness, degraded, and estimate markers travel with the
  figure; an unqualified number means live-and-exact.
- **A mutation is durable before the screen moves on.** The write loop confirms read-back before
  an entry is treated as saved; otherwise control stays with the owner.
- **Corrections are entered, never hand-edited.** Fixing a mistake is a Reversal + new event in
  `entry`, matching the append-only ledger — the TUI offers no path to edit a stored row.
- **Color is redundant, never the only signal.** Every colour-coded distinction also carries a
  sign, glyph, or word, so the UI reads correctly in monochrome, when piped, under `NO_COLOR`,
  and for colorblind users.

## Key Design Decisions

- **Sub-HLD with `entry` / `views` children, not one leaf.** The two are distinct intents
  (mutation vs rendering) sharing a real parent intent (shell + conventions); one combined LLD
  would outgrow itself and blur them. (Alternative considered: a single `tui` leaf with
  `TUI-ENTRY`/`TUI-VIEW` facets — simpler tree, but the shared conventions had no clean home and
  the doc grew unwieldy.)
- **Conventions live in the parent, applied by both children.** Freshness/degraded/estimate/
  integrity/confirm rendering is defined once here so `entry` and `views` cannot drift.
- **The write-path loop is a parent contract.** Every entry flow conforms to it; children specify
  *what* is composed, not *how* durability is achieved.
- **ratatui + crossterm, immediate-mode.** The maintained standard Rust TUI. Immediate-mode
  rendering matches "views are projections" (each frame renders from the current `Snapshot`/
  `reports`), and it ships the widgets the Ledger motifs need — rounded blocks, `Sparkline` /
  `Chart`, `Table` column constraints, truecolor `Color::Rgb`. App structure is a hand-rolled
  **Model–Update–View** loop; `tui-textarea` (or simple line editors) fills the input-widget gap;
  `crossterm` is the cross-platform backend. Sits outside the `verus!{}` boundary. (Alternatives:
  *tui-realm* — an Elm framework over ratatui, more structure but an extra layer; *cursive* —
  retained-mode, weaker custom-render control. Both rejected for less control over the bespoke
  aesthetic.)
- **Role-token theming, truecolor dark-only "Ledger".** Screens reference semantic/chrome **role
  tokens**, not raw colours, so an alternate palette is a drop-in if ever needed. The single
  shipped theme is the warm gilt **Ledger** (the owner runs dark terminals exclusively — no light
  variant), truecolor degrading 24-bit → 256. (Alternative: hardcode ANSI-16 — rejected as flat
  and un-themeable; a light theme — rejected as unused.)

## References

- Root HLD: `docs/high-level-design.md` (`tui` as a sub-HLD in the design tree; tenets it inherits).
- Children: `docs/intent/tui/entry/entry-design.md`, `docs/intent/tui/views/views-design.md`.
- Conventions mirror: `summary` (stale/est/exit-2 wording), `store` (write discipline, lock),
  `sheets-view` (degraded marks), `config` (bracket state).
