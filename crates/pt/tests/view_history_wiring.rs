//! The binary-side History threading for the TUI port: the durable History
//! points read from the workbook bundle into the (points-by-key, calendar) pair
//! `ViewState` carries, so the Positions day-change column and the History chart
//! render from real captures. Pure projection — unit-tested without the network.

use std::collections::BTreeMap;

use pt::wiring::history_bundle;
use pt_core::{Cents, Date};
use reports::{SeriesPoint, TradingDayKey};

fn point(day: i32, total: i64) -> SeriesPoint {
    SeriesPoint {
        key: TradingDayKey(Date(day)),
        total_market_value_cents: Cents(total),
        total_unrealized_pretax_cents: Cents(0),
        total_unrealized_net_of_tax_cents: Cents(0),
        total_basis_cents: Cents(0),
        per_symbol_value_cents: BTreeMap::new(),
        per_symbol_shares: BTreeMap::new(),
        marks: reports::PricedMarks::new(),
        captured_at_epoch_secs: 0,
        reporting_tz_date: Date(day),
        incomplete: false,
    }
}

// @spec TUI-VIEW-POS-005, TUI-VIEW-HIST-003
#[test]
fn history_bundle_maps_points_by_key_with_an_ascending_calendar() {
    // Points arrive in capture order; the bundle keys them by trading day
    // (last-wins, mirroring the tab's upsert) with an ascending calendar.
    let pts = vec![point(102, 2_00), point(100, 1_00), point(102, 3_00)];
    let (map, calendar) = history_bundle(pts);
    assert_eq!(
        calendar,
        vec![TradingDayKey(Date(100)), TradingDayKey(Date(102))],
        "the calendar is the ascending distinct trading days"
    );
    assert_eq!(
        map.get(&TradingDayKey(Date(102))).unwrap().total_market_value_cents,
        Cents(3_00),
        "last-wins by trading-day key"
    );
    assert_eq!(map.len(), 2);
}

// @spec TUI-VIEW-POS-005
#[test]
fn history_bundle_is_empty_for_no_captures() {
    let (map, calendar) = history_bundle(Vec::new());
    assert!(map.is_empty());
    assert!(calendar.is_empty());
}
