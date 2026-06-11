---
parent: tui
prefix: TUI-ENTRY
---

# entry

## Context and Design Philosophy

`entry` is the **write-path surface** of the TUI — the only place activity is recorded. Every
flow is a **composer** (a small form) that rides the parent's write-path loop and renders in the
Ledger language; `entry` specifies *what each flow composes and validates*, not *how durability
works* (that is the `tui` sub-HLD's contract). It mutates only through the verified kernels and
`store`; it computes nothing itself.

Flows fall in four families (facets): **activity** (`TUI-ENTRY-ACT`), the **lot picker**
(`TUI-ENTRY-LOT`), **tax-accrual actions** (`TUI-ENTRY-TAX`), and **config edits**
(`TUI-ENTRY-CFG`), all sharing one **composer/write-loop** pattern (`TUI-ENTRY-FLOW`).

`entry` is UI/IO — integration-tested, not Verus-proven. The validation it surfaces *is* the
verified `LedgerError`/`TaxError`/config validation; `entry` only invokes and renders it.

## The Composer & Write Loop

Every flow is the same shape, so muscle memory transfers:

```
 fields ─▶ inline validate (kernel, live) ─▶ submit ─▶ append + read-back-verify ─▶ confirmed
   ▲             │ reject: the LedgerError/TaxError/        │ unreachable / mismatch:
   └─ edit ◀─────┘ config error shown beside the field,     └─ keep the composed entry,
                   nothing written                              return control, offer [r]etry
```

- **Inline validation is advisory; submit is authoritative.** Inline checks run as fields change
  (against the current cached snapshot) and render the specific error (`InsufficientShares`,
  `BadSplitRatio`, a bracket-stacking breach…) **beside the offending field** in `error`, writing
  nothing. Because the cache can lag the workbook, **submit re-validates against the live state**
  and is the authority; a submit-time disagreement re-renders in the same inline slot — inline
  never overrides submit. (The Reversal dependency view and the lot picker are likewise
  point-in-time and re-checked at submit.)
- **Submit** order: *confirm (if gated)* → *acquire the advisory write-lock* → *append through
  `store`* (validate → append → read-back-verify) → *confirmed*, which clears the composer. A
  `WriteVerifyMismatch` or unreachable workbook **returns control** with the entry intact and an
  `[r]etry` — never a silent loss, never an optimistic "saved." If the **lock is held** (a cron
  `summary`), submit fails non-destructively (`⚠ lock held` in `warn`, entry preserved, retry) —
  no queue. `[r]etry` reuses the prior confirmation and is safe to repeat (stable `EventId`
  idempotency).
- **Stable-`EventId` contract (the retry's basis).** The idempotent retry depends on the submit
  carrying the **same `EventId` across every attempt**. `entry` does **not** author the id: the
  `EventId` is a toolchain-stable content hash assigned at event creation (`runtime` owns global
  uniqueness; `store` assigns for a live append). The composer's obligation is to **freeze the
  composed event's content at the first submit and re-submit the byte-identical event on `[r]etry`**
  — never re-deriving fields (e.g. re-reading "today", re-resolving an alias) between attempts —
  so the content hash, and thus the `EventId`, is identical and `store`'s idempotency check
  recognises the retry as the same event. A composer edit
  after a returned-control submit is a *new* entry (new content → new id), not a retry.
- **Defaults reduce typing**: date defaults to today; `accrues_to_state` defaults to
  `config.residency_on(sale_date)`; platform offers `config` suggestions; symbols resolve through
  the alias table. All overridable.
- **Launched with identity.** When `views` launches a flow on a selection, `entry` receives only
  the **identity** (an accrual key `(sale_id, lot_id, jurisdiction, tax_year)`, a lot id, or a
  symbol) and **re-resolves it live** against the current `Snapshot` at flow start — never the
  rendered, possibly-stale values.
- **Confirmation** gates the consequential/corrective flows (Reversal, Pay, tax-rule edits); plain
  appends (Buy/Vest/Sell/Split) do not gate.

## Keybindings & Form Rendering (`TUI-ENTRY-FLOW`)

`entry`'s contexts (the activity composer, the lot picker, the accrual-action forms, the config
forms) are the active context the shell's input loop (the `tui` sub-HLD) dispatches keys to. The
keymap is **arrow/tab-driven with single-key command accelerators**, consistent across every form
so the loop's dispatch is uniform:

- **Field navigation.** `tab` / `shift-tab` (and `↑`/`↓`) move between fields; `enter` on the last
  field is **submit**; `esc` cancels the flow and pops the context (the composed entry is discarded
  only after a confirm-discard if any field is dirty). A focused field enters **text-input mode**
  (the shell suppresses global accelerators) and `esc` leaves it back to field-navigation mode.
- **Command accelerators.** Bracketed single keys shown in the form footer fire the form's actions:
  `[F]` FIFO-fill in the lot picker, `[c]` confirm-new-position on the new-symbol guard, `[r]`
  retry on a returned-control submit, `[enter]` confirm on a gated flow, `[space]` toggle a row in
  a multi-select (Pay covers, batch accrual selection). These are bound **only** while the field is
  not in text-input mode.
- **Form layout (every flow).** A rounded panel titled by the flow, the **focused field carrying
  the gilt focus caret** (`▎` over `bg-focus`), tracked-caps labels beside each input, and a single
  **inline error/advisory slot** directly beneath the offending field — the same slot inline
  validation and a submit-time disagreement both write to. The
  footer is a `·`-segmented hint line of the active accelerators.
- **The lot picker** renders as a `Table` of the symbol's open lots on the sale's platform — one
  row per lot (id · source+date · term · remaining · basis/share · a `take [N]` input cell) — with
  the focus caret on the active row, the running `allocated N / M` and the `[est]` gain/tax preview
  in a footer band, and the empty/insufficient state replacing the table body
  when there are no allocatable lots. The inline error sits in the footer band, never over a data
  row.

The keymap is intentionally arrow/tab-first (not modal-vim) so the muscle memory matches the
read-side `views` nav, and the loop can route a key to the active context without per-flow
special-casing.

## Activity Entry (`TUI-ENTRY-ACT`)

| Flow | Composes | Notes |
|---|---|---|
| **Buy** | symbol, qty, unit price, date, fees, platform, tracking code | opens a lot; basis = `scale(qty×price)+fees`. |
| **Vest** | symbol, qty, FMV/share, date, platform, tracking code | opens a lot at FMV basis; no fee field. |
| **Sell** | symbol, qty, unit price, date, fees, platform, tracking code, **lots** | drives the lot picker below; `accrues_to_state` defaulted from residency. |
| **Split** | symbol, ratio (num : den), date | `ratio ≥ 1:…`; rejects `BadSplitRatio` inline. |
| **Reversal** | target event (picked from a list) | confirm; surfaces backward-dependency rejection (below). |

**New-symbol guard.** The kernel accepts any `Symbol` string, so a typo would silently open a
phantom position. When a Buy/Vest names a symbol matching **no existing position and no alias**,
`entry` **warns** (does not block): `new symbol 'AMZM' — first seen · [c]onfirm new position`, so
creating a position is a deliberate, visible act.

The **Reversal** flow lists recent events; already-reversed events and prior Reversals are
**greyed/omitted** via a `runtime`/`ledger-core` **reversible-targets** read helper (it filters
those from the current `Snapshot` and computes each candidate's dependency block) — since the
kernel's `BadReversal` is an *append-time rejection*, not a queryable list. Selecting a target a
later surviving event depends on shows the block inline rather than failing on submit:

```
╭─ Reverse an event ──────────────────────────────────────── confirm ─╮
│ ▎#142  Sell  AMZN  100 @ $95.50   2023-03-09  Robinhood              │
│  #138  Buy   AMZN  100 @ $93.09   2015-01-01                         │
│  ⚠ Reversing #138 is blocked — Sell #142 consumed its lot.          │
│    Reverse #142 first.                                              │
╰─────────────────────────────────────────────────────────────────────╯
  [enter] reverse (confirm)    [esc] cancel
```

## The Lot Picker (`TUI-ENTRY-LOT`) — the signature interaction

A Sell's specific-identification lot selection. It shows the symbol's open lots **on the sale's
platform**, each with term (LT/ST as of the sale date), remaining qty, and basis/share; the owner
allocates the sale quantity across lots (or `[F]` for FIFO), with a running total and a live
gain/tax preview:

```
╭─ Sell AMZN · 150 sh @ $261.26 · Robinhood ─────────────── 2026-06-05 ─╮
│  Pick lots (specific-ID)            or [F] FIFO                        │
│  ════════════════════════════════════════════════════════════════════ │
│ ▎RSU-GRNT1-a │ vest 2021-11-15 │ LT │ rem 750 │ $9.00/sh │ take [100]  │
│  12          │ buy  2024-03-02 │ ST │ rem 200 │ $172/sh  │ take [ 50]  │
│  5           │ buy  2021-11-23 │ LT │ rem   0 │   —      │             │
│  ──────────────────────────────────────────────────────────────────── │
│  allocated 150 / 150  ✓        est. gain $26,840 · est. tax $8,120 [est]│
╰────────────────────────────────────────────────────────────────────────╯
  [F] FIFO   [enter] confirm sell   [esc] cancel
```

- Allocations must sum to the sale qty (running `allocated N / M`, ✓ only when equal); each lot
  capped at its remaining; cross-platform / duplicate / wrong-symbol picks are refused inline
  (the `LedgerError` set). FIFO fills oldest-first as a preview the owner can still adjust.
- **Editing the sale qty resets the allocation** (✓ clears; FIFO can re-fill), so a stale
  allocation never rides into submit.
- **Empty / insufficient states are explicit.** No open lots for the symbol on the sale's
  platform → `no open lots for AMZN on Robinhood — check platform` (distinct from a normal
  under-allocation); if the platform's total remaining is below the sale qty, ✓ is unreachable and
  the shortfall is named. A `rem 0` lot is **greyed** — listed but unallocatable.
- The **est. gain / est. tax** preview is the kernel's computation on the *proposed* allocation,
  **stacked at the sale's `sale_date` position** in the year's YTD, so it matches the accrual the
  Sell will record (modulo a later back-dated entry). `[est]`-flagged, degraded if the mark is
  unavailable; per-lot figures carry the kernel's ±½¢ allocation rounding (only the totals are
  exact). It never blocks the sell.

## Tax-Accrual Actions (`TUI-ENTRY-TAX`)

Selected from the Tax view; each appends a `TaxEvent` through the write loop. Multi-select allows
batch actions:

- **Allocate** — assign an accrual to a reserve account (`account_label`); re-allocatable until Paid.
- **Move** — record the **actual** amount moved + date (may differ from the estimate; the
  shortfall surfaces, not a rejection). The **shortfall is surfaced on the Move form itself**, beside
  the amount field: as the actual is entered, a live `warn` advisory `⚠ short $N vs accrued $M`
  compares the entered actual against the selected accrual's computed amount (or, for a batch, the
  selected set's summed accrual), and an exact/over move clears it. This is the per-Move surfacing
  the aggregate `ReserveLine.shortfall` (the Tax view) rolls up; it is advisory only and never
  blocks the Move.
- **Pay** — record a remittance for a `(jurisdiction, tax_year, period)` and check off the
  **Moved** accruals it covers (covers must match the Pay's jurisdiction/year); **confirm**.
  Amount and covered set are **independent** (per `tax`): `Pay.amount` is the authoritative actual,
  `covers` marks accruals Paid. The `covered $X / paid $Y ✓` line is **advisory**; a partial or
  over-payment (`amount ≠ Σ covered`) is allowed, surfacing the delta — never a submit gate.
  A **pre-submit jurisdiction/year advisory** runs inline as covers are toggled: if a selected
  accrual's `(jurisdiction, tax_year)` differs from the Pay's, the form shows a `warn`
  `⚠ AMZN RSU-GRNT1-c is State CA 2025 — not Federal 2026` beside that row **before** submit, so the
  owner sees the mismatch the kernel would reject as `PayCoverMismatch` at submit rather than
  discovering it on rejection. The advisory is the inline pre-empt; submit re-validates and remains
  the authority.
- **Override** — replace an accrual's computed amount with an absolute `applied_amount_cents` +
  reason (a logged exception); **confirm**.

**Batch & moving targets.** Accrual amounts are computed-live, so a batch action snapshots the
selected set at confirm and **re-validates at submit**; if a selected accrual vanished (its sale
was reversed) or repriced (a config edit), `entry` returns to the form with the delta flagged
rather than committing a stale `covers` list.

```
╭─ Pay — Federal 2026 ───────────────────────────────────── confirm ─╮
│  Period [Q2 ▾]   Amount $[12,100]   Date [2026-06-15]               │
│  Cover Moved accruals:                                              │
│ ▎[✓] RSU-GRNT1-a  $2,400  ◉◉◉○      [✓] RSU-GRNT1-b  $960  ◉◉◉○     │
│  covered $12,100 / paid $12,100  ✓                                  │
╰─────────────────────────────────────────────────────────────────────╯
  [enter] record payment (confirm)   [esc] cancel
```

## Config Edits (`TUI-ENTRY-CFG`)

Forms over `config`, each running `config`'s validation and writing the config tabs / local file:

- **Residency** — add a move `(effective_date, state)`; future-dated allowed; consecutive
  same-state rejected; a founding-entry violation (none at/before the earliest event, blocking
  import) renders as an **inline config error**. The form notes a residency change affects only
  **future-dated `accrues_to_state` defaults**, not already-stamped events.
- **Tax brackets / income / NIIT / de-minimis** — the data-entry side of the refresh runbook;
  rejects a non-monotonic set or a stacked-rate breach inline. The **confirm** restates the
  retroactive blast radius — *"this reprices N unpaid accruals' estimates"* — since unpaid
  accruals are computed-live.
- **Platforms & ticker aliases** — manage the suggestion list and `symbol → GOOGLEFINANCE ticker`
  aliases.

The bracket-**staleness reminder** surfaces here and in the status line (per `config`), linking
straight into the bracket form.

## Confirmations (`TUI-ENTRY-FLOW`)

Confirm gates exactly: **Reversal**, **Pay**, **Override**, and **tax-rule edits** (brackets /
income / NIIT). Plain appends (Buy / Vest / Sell / Split, residency, platform/alias) do not gate.
A confirm step restates what will be written before it is.

## Decisions & Alternatives

| Decision | Chosen | Alternatives Considered | Rationale |
|----------|--------|------------------------|-----------|
| One composer pattern | All flows share fields → inline-validate → write-loop | Bespoke screen per flow | Uniform muscle memory; one place to get durability/error rendering right. |
| Lot picker | Inline allocation table with running total + live gain/tax preview, FIFO shortcut | A separate confirm dialog; FIFO-only | Specific-ID is the owner's tax lever; previewing gain/tax at pick time is where the decision is actually made. |
| Reversal target | Pick from a list; block reasons shown inline | Type an event id; fail on submit | The backward-dependency rule is easier to honor when the block is visible before committing. |
| Move amount | Free actual (shortfall surfaced) | Force = computed accrual | Matches reality (round transfers); the reserve tracks real money (`tax` decision). |
| Override | Absolute amount + reason, confirmed | A rate | There is no single rate under stacking+NIIT (`tax` decision); an amount is unambiguous. |
| Confirm scope | Reversal / Pay / Override / tax-rule edits only | Confirm everything; confirm nothing | Gate the consequential/irreversible; keep frequent appends friction-free. |
| Inline vs submit | Inline advisory (cached); submit authoritative (live), error re-rendered inline | Treat inline as binding | The cache can lag; binding inline would green-light a write the kernel then rejects. |
| New-symbol entry | Warn (not block) on an unknown symbol with no alias | Block unknown symbols; accept silently | The kernel accepts any symbol; a silent typo opens a phantom position, but blocking would stop legitimate new positions. |
| Pay amount vs covered | Independent; ✓ advisory; partial/over allowed | Force `amount = Σ covered` | Matches `tax` (amount is the actual, covers marks paid); estimates rarely equal the real remittance. |
| Sale-qty change | Resets the allocation | Rescale; leave stale | A reset is unambiguous; a stale ✓ must never reach submit. |

## Open Questions & Future Decisions

### Deferred
1. **Bulk / CSV-style entry.** Fast multi-row entry beyond one composer at a time (overlaps `import`).
2. **Undo affordance.** Whether to offer a one-step "reverse the thing I just entered" shortcut
   distinct from the full Reversal flow.

## References

- Parent: `docs/intent/tui/tui-design.md` (write-path loop, conventions, Ledger aesthetic).
- Kernels/segments invoked: `ledger-core` (events, `LedgerError`, lot selection), `tax`
  (`TaxEvent`s, accrual lifecycle, override), `config` (residency, brackets, platforms/aliases,
  validation), `store` (write discipline, advisory lock).
