# Arrow: config

Mutable, versioned reference data: tax-rule tables, the residency timeline, platforms and
aliases, local settings/secrets, validation, and staleness signaling.

## Status

**OK** — last audited 2026-06-11 (git SHA `2e43e4dd8896b05a75325c9a08fd37846cfb172d`). All 29
specs implemented with citing tests.

## References

### HLD
- docs/high-level-design.md (configuration & reference data)

### LLD
- docs/intent/config/config-design.md

### EARS
- docs/intent/config/config-specs.md (29 specs)

### Tests
- crates/config/tests/taxrules.rs
- crates/config/tests/residency.rs
- crates/config/tests/resolution.rs
- crates/config/tests/platforms.rs
- crates/config/tests/settings.rs
- crates/config/tests/validation.rs
- crates/config/tests/lock.rs
- crates/runtime/tests/adapters.rs (runtime persistence adapter seam)
- crates/runtime/tests/seed_taxrules.rs
- crates/tui/tests/entry_activity.rs (platform suggestions seam)
- crates/tui/tests/entry_submit_wiring.rs

### Code
- crates/config/src/lib.rs
- crates/runtime/src/adapters.rs (the single-JSON-cell persistence adapter)
- crates/tui/src/entry.rs, crates/tui/src/lib.rs (consuming seams)

## Architecture

**Purpose:** Owns validated bracket/income/residency/platform reference data with
`Verified | Stale | NoBracketsAvailable` tagging; secrets stay in a local file.

**Key Components:**
1. Tax-rule tables — brackets/NIIT/income/de-minimis in `ppm`/`Cents`
2. Residency timeline — effective-dated, founding-entry gated
3. Platforms & aliases — suggestions, never a constraint
4. Validation — stacked-rate ceiling and table-shape gates
5. Staleness — active-set reminder signal

## Spec Coverage

| Category | Spec IDs | Implemented | Deferred | Gaps |
|----------|----------|-------------|----------|------|
| Tax-Rule Tables | CONFIG-TAXRULES-001..006 | 6 | 0 | 0 |
| Residency Timeline | CONFIG-RESIDENCY-001..005 | 5 | 0 | 0 |
| Resolution, Degradation & Staleness | CONFIG-STALE-001..004 | 4 | 0 | 0 |
| Platforms | CONFIG-PLATFORM-001..003 | 3 | 0 | 0 |
| Settings & Read Contract | CONFIG-SETTINGS-001..006 | 6 | 0 | 0 |
| Validation | CONFIG-VALID-001..005 | 5 | 0 | 0 |

**Summary:** 29 of 29 active specs implemented; 0 deferred.

## Key Findings

1. **Tab persistence shape** — the design names three human-readable config tabs, but the
   runtime adapter persists the whole domain config as one validated JSON cell
   (`'Tax Rules'!A1`). Recorded as a Decisions & Alternatives row in
   docs/intent/config/config-design.md (per-tab human-readable schema deferred).

## Work Required

None — segment is coherent.
