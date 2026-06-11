# portfolio-tracker

A personal stock-portfolio and tax tracker for a single owner-operator — and a working,
end-to-end demonstration of **Linked-Intent Development (LID)**, a methodology where every
line of code traces back through tests and structured requirements to a design document.

## What this is

It tracks equity and RSU positions held across multiple trading platforms, computes realized
and unrealized profit on a tax-lot basis, calculates per-sale federal + state tax from
configured income and bracket tables, and manages each sale's tax accrual through a real
lifecycle (accrued → allocated → moved → paid). It replaces a hand-wired Google Sheet and the
shell script that scraped it.

The architecture in one breath:

- **Google Sheets is the canonical store.** Activity lives in append-only event-log tabs
  (rows are never edited — corrections are new compensating events). The app also publishes
  clean, filterable read-only view tabs with a `GOOGLEFINANCE` column as the live-price
  oracle. A local SQLite cache mirrors the log for fast/offline reads and carries no truth.
- **A ratatui terminal UI** is the entry surface: buy/vest/sell, accrual actions, residency
  and config changes, plus five report screens.
- **A headless `pt summary` command** prints the daily portfolio summary with day-over-day
  deltas (text or JSON), for cron and scripts.
- **Tax is calculated, not typed**: long-term vs short-term by holding period, per-year
  federal and state bracket tables, the state stamped on each transaction so interstate
  moves are exact.
- **The money math is formally verified.** The pure accounting/tax kernels live in
  `verus!{}` modules proven with [Verus](https://github.com/verus-lang/verus) (share/cost
  conservation, FIFO realized-P&L, rounding and tax-calculation bounds, accrual-lifecycle
  invariants) with [Kani](https://github.com/model-checking/kani) bounded model checks on the
  integer arithmetic kernel. Honestly: only the pure kernels are verified — serialization,
  the Sheets I/O, and the TUI are ordinary tested Rust outside the proof, crossing into the
  kernels through a single drift-guard-tested trust seam.

The TUI's Positions screen, in its gilt "Ledger" visual language:

```
  L E D G E R                                                     as-of 2026-06-09
╭─ Positions ──────────────────────────────────────────────────────────────────────╮
│  TOTAL VALUE  $1,284,300   ▲ +$8,420  (+0.66%)                                   │
│  NET (POST-TAX)  $902,160  [est]                                                 │
│  REALIZED 2026 YTD  ▲ +$48,210                                                   │
│  ════════════════════════════════════════════════════════════════════════════   │
│   SYMBOL NAME          SHARES │   PRICE │   VALUE │   UNREAL │            NET │   │
│ ▎ AMZN  Amazon.com       620  │ $261.26 │ $162.0k │ +$131.0k │ $126,545 [est] │ ▁▂▃▅▆▇······
│   GOOGL Alphabet          60  │ $376.37 │  $22.6k │    −$410 │  $21,940 [est] │ ▇▆▅▃▂▁······
│   PLTR  Palantir           —  │ unpriced│       — │        ‡ │              — │ ············
╰──────────────────────────────────────────────────────────────────────────────────╯
  TAX 2026   RSU-GRNT1-a  AMZN  $2,400  ◉◉◉○ Moved      GOOGL  $253  ◉◉◉◉ Paid
  ● connected · sync as-of 2026-06-09 · updated 16:02 · 🔓 · Positions
                         [tab] entry  [1-5] screens  [r] refresh  [?] help  [q] quit
```

## Why this repo is interesting — LID in practice

LID's core idea is a one-directional **arrow of intent**:

```
HLD → LLDs → EARS → Tests → Code
```

- The **HLD** (high-level design, `docs/high-level-design.md`) states the problem, the
  approach, goals/non-goals, and ordered tenets.
- Each component gets an **LLD** (low-level design) under `docs/intent/` — eleven segments,
  from the verified `ledger-core` kernel to the one-time legacy `import`er.
- Each LLD's behavior is pinned as **EARS** specs — one-line structured requirements,
  numbered and checkbox-tracked, beside the design doc. A real one, from
  `docs/intent/store/store-specs.md`:

  > **STORE-WRITE-004**: After appending, the system shall read the row back by `EventId`
  > and assert structural equality with the event written before confirming the write; a
  > field mismatch shall raise `WriteVerifyMismatch`.

- **Tests cite specs** with `// @spec STORE-WRITE-004` comments, and code annotates the
  entry point of each behavior the same way. The coverage gate in `scripts/ci.sh` makes the
  binding mechanical: every one of the **350 defined specs** must be cited by at least one
  test, and every citation must resolve to a defined spec — 350/350, in both directions, or
  CI fails. Renaming a spec without updating its tests breaks the build.
- A navigation overlay in `docs/arrows/` (`index.yaml` + one orientation page per segment)
  tracks each segment's audit status and dependency graph, so a session can orient in
  seconds without re-reading the whole tree.

Changes flow one way: a bug fix or feature starts by finding where *intent* diverged, not by
patching code. The payoff is that the docs are never archaeology — they are the contract the
tests enforce.

**A guided tour, five files in order:**

1. `docs/high-level-design.md` — the problem, tenets, and system shape.
2. `docs/intent/store/store-design.md` — one LLD: the Sheets↔event trust seam, the
   read-back-verify write path, the two-tier cache fingerprint.
3. `docs/intent/store/store-specs.md` — that design pinned as 30 EARS lines.
4. `crates/store/tests/write.rs` — the `@spec`-annotated tests that enforce them.
5. `scripts/ci.sh` — the gate that binds it all: build, test, Verus, Kani, spec coverage,
   live e2e.

The full methodology story — the whole intent tree authored collaboratively *before any
code*, then implemented by a 34-agent TDD workflow with zero-context adversarial review —
is in `docs/notes/build-process.md`. Two bugs the discipline caught are worth retelling:

- **The duration that ate a lot reference.** The Sheets API's default `USER_ENTERED` write
  mode interprets a lot-reference cell like `3:1000000` as a *duration* and stores it as
  `16669:40:00`. The spec-mandated read-back-verify step (STORE-WRITE-004 above) refused to
  confirm the write because the row no longer equaled the event — surfacing a silent
  corruption a fire-and-forget append would have shipped. The fix: event rows ride a `RAW`
  append path; view tabs keep `USER_ENTERED` for their formulas.
- **What does "Net" mean?** The Positions row's `Net` was first implemented as post-tax
  unrealized P&L — defensible, but not what the owner reads `Net` as: keep-if-sold-today
  money. Because the meaning lives in a spec, the fix was an intent change cascaded down the
  arrow — EARS redefined (`TUI-VIEW-POS-012`: market value minus estimated unrealized tax,
  clamped at zero so an underwater RSU lot is never re-taxed), then tests, then code — one
  semantics now shared by the row, the summary band, and the headless summary.

## Quickstart

The test suite is fully offline and deterministic — the kernels are pure, with no clock,
randomness, or I/O — so this works with zero configuration:

```sh
git clone https://github.com/jszmajda/portfolio-tracker
cd portfolio-tracker
cargo test --workspace --locked
```

To run it for real you need a workbook of your own:

1. Create a Google Cloud **service account**, download its JSON key, and enable the Sheets
   API. Create a fresh Google Sheets workbook and **share it with the service account's
   email** (editor). The app bootstraps its tabs on first write — no manual setup inside
   the sheet.
2. Copy each `*.local.example.*` file to its real (gitignored) name and fill it in:
   - `config.local.example.toml` → `config.local.toml` — workbook id, credentials path,
     cache path, reporting timezone.
   - `owner.local.example.json` → `owner.local.json` — filing status, income estimate,
     residency state, platforms, ticker→name map (the personal half of tax seeding).
   - `import.local.example.json` → `import.local.json` — only if migrating a legacy sheet.
3. Run it:

```sh
cargo run -p pt -- summary        # headless daily summary (--json for machines)
cargo run -p pt                   # the TUI: [1-5] screens, [tab] entry, [?] help, [q] quit
```

**Seeding tax brackets.** Tax calculation needs bracket tables for your year and state.
`docs/runbooks/refresh-tax-brackets.md` is the maintenance runbook (the TUI reminds you when
tables go stale); the executable companion is a gated test that writes the validated tables
to your workbook, selecting your personal subset from a checked-in multi-state library:

```sh
PT_SEED_TAXRULES=1 PT_WORKBOOK_ID=<id> GOOGLE_APPLICATION_CREDENTIALS=<sa.json> \
  cargo test -p runtime --test seed_taxrules -- --include-ignored --nocapture
```

**Migrating a legacy sheet.** `cargo run -p pt -- import` reads the legacy workbook
read-only, reconstructs the event stream, validates it through the kernel, and prints a
reconciliation report against the legacy positions — it is a **dry run by default and writes
nothing**. After reviewing the report, `cargo run -p pt -- import --commit` performs the
gated one-time write into the new workbook.

## CI notes

`.github/workflows/ci.yml` is a thin host around `scripts/ci.sh` — the gate is identical
locally and in CI. On a fresh fork, out of the box:

- **build + test + @spec-coverage run and gate** — offline, no secrets needed.
- **Verus and Kani skip** unless the runner provisions their toolchains (commented-out
  provisioning steps are in the workflow; flip `PT_CI_REQUIRE_VERUS=1` /
  `PT_CI_REQUIRE_KANI=1` once cached to make absence a hard failure).
- **The live real-Sheets e2e skips** unless the `PT_WORKBOOK_ID` and `GOOGLE_SHEETS_SA`
  repository secrets are configured; with them it round-trips events against a real
  workbook, self-cleaning its dedicated test tabs.

So a fresh fork's CI is green-with-skips **by design** — the deterministic gates always run,
and the heavyweight ones light up as you provision them.

## Status & roadmap

This is a personal tool, live and in daily use by its owner-operator. Known feature-sized
gaps are tracked in `TODO.md` — currently unvested-grant tracking (the model starts at
*Vest*) and non-security holdings (e.g. physical gold) — each awaiting its own LID pass.

Contributions are welcome, with one house rule: **changes start at intent, not code.** Walk
the arrow — find the owning LLD under `docs/intent/`, adjust the design and its EARS, then
tests, then code. The CLAUDE.md and the `docs/arrows/` overlay will orient you; the coverage
gate will keep you honest.

## License

[MIT](LICENSE).
