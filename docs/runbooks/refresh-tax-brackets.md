# Runbook: Refresh Tax Brackets

A **you-and-me maintenance task**, not code. The system only *reminds* you to do this (the
`config` staleness signal, surfaced in the TUI); these are the steps to actually do it. No EARS,
no tests — operational documentation. Claude can fetch and parse the source documents when asked.

## When to run

- The TUI shows a **tax-rules-stale** reminder (a current-tax-year bracket set is missing or its
  `last_verified` is older than the threshold, default 12 months), **or**
- a **new tax year** has begun, **or**
- you **move to a new state** (a new jurisdiction becomes active and needs its own brackets).

## What you need (primary sources)

Always use primary sources for the target `tax_year`; secondary summaries drift.

- **Federal** — the IRS annual inflation-adjustment **Revenue Procedure** for the year:
  - ordinary income tax brackets — **all four filing-status tables** (the library carries
    Single, MFJ, MFS, and HoH),
  - long-term capital-gains brackets (0 / 15 / 20%) thresholds, per filing status,
  - **NIIT**: 3.8% rate and the per-status MAGI thresholds (statutory, not indexed).
- **Each state the library carries** — that state revenue department's individual income
  tax brackets for the year (e.g. DC's Office of Tax and Revenue rate schedule); where a
  state keys its schedule on filing status (NJ, CA, NY, MD, MN, OR), all of its tables.

## Steps

1. **Confirm filing status** for the year (Single / MFJ / …) — the owner seed's
   `filing_status` selects which federal bracket and NIIT-threshold set the seeding writes;
   the library still carries all four.
2. For **each** jurisdiction, transcribe the brackets as ordered `(lower_threshold, rate)` rows,
   lowest first, starting at `$0`. Enter rates as percentages; the system stores fixed-point
   parts-per-million (so NJ's 5.525% is exact, not rounded).
3. Enter the year's **federal LT brackets**, **NIIT** rate + MAGI threshold, and your
   **annual ordinary income** estimate.
4. Confirm the **de-minimis** threshold (rarely changes).
5. **Set `last_verified` = today** and record the **`source_note`** (the exact publication, e.g.
   "IRS Rev. Proc. 2026-NN", "DC OTR 2026 rate schedule") for each set you touched.

## Verify

- `config` validation passes: brackets are ordered, start at `$0`, every combined marginal rate
  is `< 100%`.
- The TUI **stale** reminder clears for the refreshed (jurisdiction, year).
- Spot-check: pick a known realized gain and confirm its computed accrual matches a hand
  calculation against the new brackets.

## Notes

- Entering this year's brackets does **not** change prior closed years — those keep the brackets
  they were verified with.
- Until a year's brackets are entered, `tax` degrades to the most recent prior year's set,
  **flagged stale** — estimates still appear, clearly marked, so do this promptly each year.

## Executable companion

The transcription lives in `crates/runtime/tests/seed_taxrules.rs` as a multi-status /
multi-state **library** (its `tables` module): all four federal filing-status table sets
(ordinary + long-term + NIIT threshold) and many states' schedules, each with its own
`source_note` and `last_verified`. The library is deliberately a superset of what any
seeding run writes, so the checked-in data does not reveal which filing status or state the
owner actually uses — keep it that way: a refresh updates **all** the tables and their
verified dates, not just the owner's subset. States whose indexed figures for the target
year aren't published yet carry the latest published year, with that fact stated in the
`source_note`.

Offline tests re-check the whole library in plain `cargo test`
(`every_library_table_passes_the_validation_gate` runs every state × every federal status
through config's validation gate; `the_2026_figures_pass_the_validation_gate` checks the
subset the example owner seed selects).

The PERSONAL half of the inputs — filing status, the annual ordinary-income estimate,
residency state, de-minimis, platforms, and the symbol→display-name map — lives in the
gitignored repo-root `owner.local.json` (copy `owner.local.example.json` and fill it in;
the seeding fails loudly without it). Seeding writes exactly the subset the owner seed
selects: one federal status set plus the residency state's schedule. Then run the gated
seeding:

```sh
PT_SEED_TAXRULES=1 \
PT_WORKBOOK_ID=<workbook id> \
GOOGLE_APPLICATION_CREDENTIALS=<service-account json> \
cargo test -p runtime --test seed_taxrules -- --include-ignored --nocapture
```

It writes through `config`'s validated put path (the same gate the TUI uses) and reads the
rules back to confirm the round-trip.
