# config — EARS Specs

Specs owned by the `config` leaf (prefix `CONFIG`; see `config-design.md`). Status: `[x]`
implemented · `[ ]` active gap · `[D]` deferred. Money is `Cents`; rates are fixed-point
parts-per-million (`ppm`, actual = `ppm/1_000_000`).

## Tax-Rule Tables

- [x] **CONFIG-TAXRULES-001**: The system shall store, per `tax_year`, the filing status, federal ordinary brackets, federal long-term brackets, NIIT (rate + MAGI threshold), and per-state ordinary brackets.
- [x] **CONFIG-TAXRULES-002**: The system shall store all tax rates as fixed-point parts-per-million (`ppm`) and all monetary thresholds as `Cents`.
- [x] **CONFIG-TAXRULES-003**: While a `tax_year` is the open (current) year, the system shall treat its annual ordinary income estimate as live and updatable.
- [x] **CONFIG-TAXRULES-004**: The system shall record a `last_verified` date and a `source_note` on each rule set.
- [x] **CONFIG-TAXRULES-005**: The system shall record one filing status per `tax_year`.
- [x] **CONFIG-TAXRULES-006**: The system shall store a single `de_minimis_cents` threshold, compared by `tax` against `|accrual amount|` for auto-settling and reused by the annual report as a `|gain|` display floor.

## Residency Timeline

- [x] **CONFIG-RESIDENCY-001**: The system shall store a residency timeline of `(effective_date, state_code)` entries, kept sorted with unique effective dates and no consecutive same-state entry.
- [x] **CONFIG-RESIDENCY-002**: The system shall resolve `residency_on(date)` as the `state_code` of the entry with the greatest `effective_date ≤ date`, so a sale on an effective date resolves to the new state.
- [x] **CONFIG-RESIDENCY-003**: Where a residency entry is future-dated, the system shall store it and resolve it normally for dates on or after its effective date.
- [x] **CONFIG-RESIDENCY-004**: When a sale is entered, the system shall default its `accrues_to_state` to `residency_on(sale_date)`, leaving it overridable per transaction.
- [x] **CONFIG-RESIDENCY-005**: When a manual sale is entered and the residency default `residency_on(sale_date)` is None, the system shall require an explicit `accrues_to_state` on the entry, blocking submit otherwise.

## Resolution, Degradation & Staleness

- [x] **CONFIG-STALE-001**: When `tax` requests a `(jurisdiction, tax_year)` bracket set, the system shall return that year's verified set if present, else that jurisdiction's most recent prior-year set flagged `Stale`, else `NoBracketsAvailable`.
- [x] **CONFIG-STALE-002**: If, for the current tax year, a jurisdiction has no verified bracket set or its newest set's `last_verified` is older than the configured threshold (default 12 months), then the system shall flag that jurisdiction stale.
- [x] **CONFIG-STALE-003**: The system shall compute the staleness active set as federal plus every state in the residency timeline within the configured lookback window (excluding not-yet-effective future entries), without consulting `tax` accrual state.
- [x] **CONFIG-STALE-004**: The system shall surface, per stale `(jurisdiction, tax_year)`, a signal for the TUI to present.

## Platforms

- [x] **CONFIG-PLATFORM-001**: The system shall maintain a platform suggestion list that does not constrain event entry; an event may name a platform absent from the list.
- [x] **CONFIG-PLATFORM-002**: The system shall maintain a symbol→ticker alias map (identity when a symbol has no entry), consumed by `sheets-view` to resolve the `GOOGLEFINANCE` ticker for a ledger symbol.
- [x] **CONFIG-PLATFORM-003**: The system shall maintain a symbol→display-name map (the ticker itself when a symbol has no entry), persisted and round-tripped with the rest of the domain config, consumed by the TUI to render a company-name column beside tickers.

## Settings & Read Contract

- [x] **CONFIG-SETTINGS-001**: The system shall store machine/secret settings (`workbook_id`, `credentials_path`, `cache_path`, `reporting_timezone` default `US/Eastern`) in a local file, never in the workbook.
- [x] **CONFIG-SETTINGS-002**: The system shall persist domain config in dedicated workbook tabs, mirrored to the local cache.
- [x] **CONFIG-SETTINGS-003**: If the workbook has no config tabs, then the system shall treat config as cold-start (`NoBracketsAvailable`) and prompt to seed.
- [x] **CONFIG-SETTINGS-004**: If the credentials file is missing or invalid, then the system shall surface a hard error rather than returning empty config.
- [x] **CONFIG-SETTINGS-005**: When the local cache and workbook config tabs disagree, the system shall treat the workbook tabs as authoritative and rebuild the cache. CONTRACT: `runtime` owns disagreement *detection* and the cache-*rebuild* mechanism; `config` consumes the rebuilt cache and does not author the detection/rebuild here.
- [x] **CONFIG-SETTINGS-006**: When a config `put_*` primitive writes a workbook config tab, the system shall acquire the runtime-owned advisory write-lock before mutating and release it after. CONTRACT: `runtime` owns the lock type and the canonical acquisition requirement; the lock is reentrant for a same holder.

## Validation

- [x] **CONFIG-VALID-001**: When a bracket set is written, the system shall require it to be non-empty with strictly ascending, unique `lower_threshold_cents`, a first row at `0`, and every `rate_ppm ≥ 0`, rejecting the write otherwise.
- [x] **CONFIG-VALID-002**: When tax-rule tables are written, the system shall reject them if, for any `(tax_year, state)`, the stacked top marginal rate `max(federal-ordinary top, federal-LT top) + state top + NIIT` reaches `1_000_000 ppm` (100%).
- [x] **CONFIG-VALID-005**: When tax-rule tables are written, the system shall reject any `tax_year` whose state-independent federal baseline top rate `max(federal-ordinary top, federal-LT top) + NIIT` reaches `1_000_000 ppm` (100%), so a stateless year is gated even when no `(tax_year, state)` pair exists.
- [x] **CONFIG-VALID-003**: The system shall require the NIIT rate, `magi_threshold_cents`, `de_minimis_cents`, and annual income to be non-negative.
- [x] **CONFIG-VALID-004**: If an import would run while the residency timeline has no entry at or before the earliest event date to be classified, then the system shall block the import until a founding residency entry exists.
