//! The SYNTHETIC in-memory legacy fixture (and the store fakes) ALL `import`
//! tests build against. The real legacy workbook (its id lives in the
//! gitignored owner-inputs file `import.local.json`) is for a manual run, NEVER
//! in tests (import-design.md → "References").
//!
//! These are real helpers (they carry no behavior to defer): convenience builders
//! for the three portfolio tabs plus the owner inputs, and re-exports of the
//! in-memory `store` fakes so a commit test drives `store` with no network.

use config::{ResidencyEntry, ResidencyTimeline};
use pt_core::{Cents, Date, MicroShares};

use crate::{
    ClosedYear, KnownCorporateAction, LegacyActionKind, LegacyActionRow, LegacyPositionRow,
    LegacySaleRow, LegacyWorkbook, RowCoord,
};

pub use store::testkit::InMemorySheets;
pub use store::{InMemoryCache, NoopLock, Store};

/// Build a `Store` over the in-memory fakes (a fresh, empty workbook) for commit
/// tests. (IMPORT-RUN-001)
pub fn fresh_store() -> Store<InMemorySheets, NoopLock, InMemoryCache> {
    Store::new(InMemorySheets::new(), NoopLock::new(), InMemoryCache::new())
}

/// Build a fresh `Store` AND return a clone of its advisory lock, so a commit test
/// can assert the commit acquired the lock (the count is shared across clones).
/// Used to confirm `commit` HOLDS the lock across the whole commit while the inner
/// append primitives re-acquire it. (IMPORT-RUN-007, RUNTIME-LOCK-006)
pub fn fresh_store_with_lock() -> (Store<InMemorySheets, NoopLock, InMemoryCache>, NoopLock) {
    let lock = NoopLock::new();
    let store = Store::new(InMemorySheets::new(), lock.clone(), InMemoryCache::new());
    (store, lock)
}

/// A `Store` over a workbook pre-seeded with explicit rows (test setup — e.g. a
/// partial-commit resume target, or a foreign-event target). (IMPORT-RUN-001)
pub fn seeded_store(
    ledger: Vec<store::Row>,
    tax: Vec<store::Row>,
) -> Store<InMemorySheets, NoopLock, InMemoryCache> {
    Store::new(
        InMemorySheets::seeded(ledger, tax),
        NoopLock::new(),
        InMemoryCache::new(),
    )
}

/// A `Buy` `Stock Actions` row. `dps` / `fees` are decimal-dollar strings (the
/// input boundary). (IMPORT-MAP-001)
pub fn buy_row(
    tab: &str,
    row: u32,
    tranche_id: &str,
    symbol: &str,
    qty_shares: i64,
    dps: &str,
    fees: &str,
    date: Date,
    platform: &str,
) -> LegacyActionRow {
    LegacyActionRow {
        coord: RowCoord::new(tab, row),
        tranche_id: tranche_id.to_string(),
        symbol: symbol.to_string(),
        kind: LegacyActionKind::Buy,
        qty: MicroShares(qty_shares * 1_000_000),
        dollars_per_share: dps.to_string(),
        fees_dollars: fees.to_string(),
        date,
        platform: platform.to_string(),
        tracking_code: None,
    }
}

/// A `Vest` `Stock Actions` row; `fmv_dps` is the legacy `$/share` column (the
/// recovered FMV/share, NOT the `$0` `Total Cost`). (IMPORT-MAP-002, IMPORT-CORP-003)
pub fn vest_row(
    tab: &str,
    row: u32,
    tranche_id: &str,
    symbol: &str,
    qty_shares: i64,
    fmv_dps: &str,
    date: Date,
    platform: &str,
) -> LegacyActionRow {
    LegacyActionRow {
        coord: RowCoord::new(tab, row),
        tranche_id: tranche_id.to_string(),
        symbol: symbol.to_string(),
        kind: LegacyActionKind::Vest,
        qty: MicroShares(qty_shares * 1_000_000),
        dollars_per_share: fmv_dps.to_string(),
        fees_dollars: "0".to_string(),
        date,
        platform: platform.to_string(),
        tracking_code: None,
    }
}

/// A `Stock Sales` row, specific-ID against `tranche_id`. (IMPORT-MAP-003)
#[allow(clippy::too_many_arguments)]
pub fn sale_row(
    tab: &str,
    row: u32,
    sale_tag: &str,
    tranche_id: &str,
    symbol: &str,
    qty_shares: i64,
    dps: &str,
    fees: &str,
    date: Date,
    platform: &str,
) -> LegacySaleRow {
    LegacySaleRow {
        coord: RowCoord::new(tab, row),
        sale_tag: sale_tag.to_string(),
        tranche_id: tranche_id.to_string(),
        symbol: symbol.to_string(),
        qty: MicroShares(qty_shares * 1_000_000),
        dollars_per_share: dps.to_string(),
        fees_dollars: fees.to_string(),
        date,
        platform: platform.to_string(),
    }
}

/// A `Positions` reconcile-target row (cents in, micro-shares as whole shares).
/// (IMPORT-MAP-004)
pub fn position_row(
    symbol: &str,
    shares: i64,
    realized_cents: i64,
    unrealized_cents: i64,
) -> LegacyPositionRow {
    LegacyPositionRow {
        symbol: symbol.to_string(),
        shares: MicroShares(shares * 1_000_000),
        realized_pnl_cents: Cents(realized_cents),
        unrealized_cents: Cents(unrealized_cents),
    }
}

/// The AMZN 20:1 split on `date`. (IMPORT-CORP-002)
pub fn split_20_for_1(symbol: &str, date: Date) -> KnownCorporateAction {
    KnownCorporateAction {
        symbol: symbol.to_string(),
        date,
        ratio_num: 20,
        ratio_den: 1,
    }
}

/// A closed-year migration seed: `legacy_actual` in cents, allocated to `account`.
/// (IMPORT-TAX-002)
pub fn closed_year(
    jurisdiction: config::Jurisdiction,
    tax_year: i32,
    legacy_actual_cents: i64,
    account: &str,
) -> ClosedYear {
    ClosedYear {
        jurisdiction,
        tax_year: config::TaxYear(tax_year),
        legacy_actual_cents: Cents(legacy_actual_cents),
        account_label: account.to_string(),
    }
}

/// A residency timeline with a single founding entry effective on `from`. Wraps
/// `config`'s validated builder. (IMPORT-TAX-001)
pub fn founding_residency(state: &str, from: Date) -> ResidencyTimeline {
    ResidencyTimeline::from_entries(vec![ResidencyEntry {
        effective_date: from,
        state_code: state.to_string(),
    }])
    .expect("single-entry founding timeline is valid")
}

/// A residency timeline from `(effective_date, state)` pairs (sorted, distinct).
/// (IMPORT-TAX-001)
pub fn residency(entries: &[(Date, &str)]) -> ResidencyTimeline {
    ResidencyTimeline::from_entries(
        entries
            .iter()
            .map(|(d, s)| ResidencyEntry {
                effective_date: *d,
                state_code: s.to_string(),
            })
            .collect(),
    )
    .expect("residency timeline invariants hold for the fixture")
}

/// An empty legacy workbook with a founding residency entry (the common base a
/// test extends with rows). (IMPORT-RUN-009)
pub fn empty_workbook(founding_state: &str, founding_from: Date) -> LegacyWorkbook {
    LegacyWorkbook {
        residency: founding_residency(founding_state, founding_from),
        ..LegacyWorkbook::default()
    }
}
