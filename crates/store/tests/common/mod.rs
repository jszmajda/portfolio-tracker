//! Shared test fixtures for the `store` suite: event factories covering every
//! `LedgerEventKind` and `TaxEventKind`, so the drift-guard round-trip and the
//! write/load tests all draw from one place. No `#[test]`s here.
#![allow(dead_code)]
#![allow(clippy::inconsistent_digit_grouping)]

use config::{Jurisdiction, TaxYear};
use ledger_core::{LedgerEvent, LedgerEventKind, LotRef};
use pt_core::{Cents, Date, MicroShares, Seq};
use tax::{AccrualKey, Quarter, TaxEvent, TaxEventKind};

// ---------------------------------------------------------------------------
// Ledger event factories (one per LedgerEventKind variant).
// ---------------------------------------------------------------------------

pub fn buy(seq: u64, id: &str) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(19_000),
        kind: LedgerEventKind::Buy {
            lot_id: format!("lot-{id}"),
            symbol: "AMZN".to_string(),
            qty: MicroShares(3_000_000),
            unit_price_cents: Cents(150_00),
            fees_cents: Cents(1_25),
            platform: "schwab".to_string(),
            tracking_code: Some("TC-1".to_string()),
        },
    }
}

pub fn vest(seq: u64, id: &str) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(19_010),
        kind: LedgerEventKind::Vest {
            lot_id: format!("lot-{id}"),
            symbol: "GOOG".to_string(),
            qty: MicroShares(1_500_000),
            fmv_per_share_cents: Cents(2_800_00),
            platform: "vestco".to_string(),
            // tracking_code intentionally None to exercise the Option round-trip.
            tracking_code: None,
        },
    }
}

pub fn sell(seq: u64, id: &str) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(19_100),
        kind: LedgerEventKind::Sell {
            sale_id: format!("sale-{id}"),
            symbol: "AMZN".to_string(),
            qty: MicroShares(2_000_000),
            unit_price_cents: Cents(175_00),
            fees_cents: Cents(2_00),
            lot_refs: vec![
                LotRef {
                    lot_id: "lot-a".to_string(),
                    qty: MicroShares(1_000_000),
                },
                LotRef {
                    lot_id: "lot-b".to_string(),
                    qty: MicroShares(1_000_000),
                },
            ],
            accrues_to_state: Some("NJ".to_string()),
            platform: "schwab".to_string(),
            tracking_code: Some("TC-9".to_string()),
        },
    }
}

/// A Sell with empty lot_refs (FIFO fallback) and a blank `accrues_to_state`, to
/// exercise the empty-Vec and None paths in the round-trip.
pub fn sell_fifo(seq: u64, id: &str) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(19_100),
        kind: LedgerEventKind::Sell {
            sale_id: format!("sale-{id}"),
            symbol: "GOOG".to_string(),
            qty: MicroShares(500_000),
            unit_price_cents: Cents(3_000_00),
            fees_cents: Cents(0),
            lot_refs: vec![],
            accrues_to_state: None,
            platform: "vestco".to_string(),
            tracking_code: None,
        },
    }
}

pub fn split(seq: u64, id: &str) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(19_050),
        kind: LedgerEventKind::Split {
            symbol: "AMZN".to_string(),
            ratio_num: 20,
            ratio_den: 1,
        },
    }
}

pub fn reversal(seq: u64, id: &str, target: &str) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(19_200),
        kind: LedgerEventKind::Reversal {
            target_event_id: target.to_string(),
        },
    }
}

/// One event of every `LedgerEventKind` variant — the drift-guard exhaustive set.
pub fn every_ledger_kind() -> Vec<LedgerEvent> {
    vec![
        buy(1, "e1"),
        vest(2, "e2"),
        sell(3, "e3"),
        sell_fifo(4, "e4"),
        split(5, "e5"),
        reversal(6, "e6", "e1"),
    ]
}

// ---------------------------------------------------------------------------
// Tax event factories (one per TaxEventKind variant).
// ---------------------------------------------------------------------------

pub fn akey(sale: &str, lot: &str) -> AccrualKey {
    AccrualKey {
        sale_id: sale.to_string(),
        lot_id: lot.to_string(),
        jurisdiction: Jurisdiction::Federal,
        tax_year: TaxYear(2025),
    }
}

pub fn akey_state(sale: &str, lot: &str, state: &str) -> AccrualKey {
    AccrualKey {
        sale_id: sale.to_string(),
        lot_id: lot.to_string(),
        jurisdiction: Jurisdiction::State(state.to_string()),
        tax_year: TaxYear(2025),
    }
}

pub fn allocate(seq: u64) -> TaxEvent {
    TaxEvent {
        seq: Seq(seq),
        kind: TaxEventKind::Allocate {
            accrual_key: akey("sale-1", "lot-1"),
            account_label: "Tax Reserve 2025".to_string(),
        },
    }
}

pub fn move_(seq: u64) -> TaxEvent {
    TaxEvent {
        seq: Seq(seq),
        kind: TaxEventKind::Move {
            accrual_key: akey("sale-1", "lot-1"),
            amount_cents: Cents(5_000_00),
            date: Date(19_300),
        },
    }
}

pub fn pay(seq: u64) -> TaxEvent {
    TaxEvent {
        seq: Seq(seq),
        kind: TaxEventKind::Pay {
            jurisdiction: Jurisdiction::Federal,
            tax_year: TaxYear(2025),
            period: Quarter::Q2,
            amount_cents: Cents(5_000_00),
            date: Date(19_320),
            covers: vec![akey("sale-1", "lot-1"), akey("sale-2", "lot-2")],
        },
    }
}

pub fn override_(seq: u64) -> TaxEvent {
    TaxEvent {
        seq: Seq(seq),
        kind: TaxEventKind::AmountOverride {
            accrual_key: akey_state("sale-3", "lot-3", "DC"),
            applied_amount_cents: Cents(1_234_56),
            reason: "manual; rounding adjustment".to_string(),
        },
    }
}

pub fn seed_migration(seq: u64) -> TaxEvent {
    TaxEvent {
        seq: Seq(seq),
        kind: TaxEventKind::SeedMigration {
            jurisdiction: Jurisdiction::State("NJ".to_string()),
            tax_year: TaxYear(2023),
            applied_amount_cents: Cents(9_999_00),
            reason: "legacy import 2023".to_string(),
        },
    }
}

/// One event of every `TaxEventKind` variant — the drift-guard exhaustive set.
pub fn every_tax_kind() -> Vec<TaxEvent> {
    vec![
        allocate(1),
        move_(2),
        pay(3),
        override_(4),
        seed_migration(5),
    ]
}

/// Store-assigned EventIds parallel to `every_tax_kind()` (the tax row carries a
/// store-assigned id even though `TaxEvent` has no id field).
pub fn every_tax_id() -> Vec<String> {
    vec![
        "tx-1".to_string(),
        "tx-2".to_string(),
        "tx-3".to_string(),
        "tx-4".to_string(),
        "tx-5".to_string(),
    ]
}
