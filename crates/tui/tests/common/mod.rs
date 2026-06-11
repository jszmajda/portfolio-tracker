//! Shared test helpers for the `tui` integration tests. Everything rides the FAKE
//! runtime ([`tui::testkit::FakeRuntime`]) — no real terminal, no Sheets.

#![allow(dead_code)]

use pt_core::{Cents, Date, MicroShares};
use tui::testkit::{buy, flat_federal_ctx, replay, FakeRuntime, ViewBuilder};

/// A tax year used across tests.
pub const YEAR: i32 = 2026;

/// A simple two-position snapshot priced live: AMZN (620 sh @ $261.26) and GOOGL
/// (60 sh @ $376.37), both Robinhood. Returns the built ViewState.
pub fn two_position_view() -> tui::port::ViewState {
    let log = vec![
        buy(1, 18_000, "L-AMZN", "AMZN", 620, 5_000, "Robinhood"),
        buy(2, 18_100, "L-GOOGL", "GOOGL", 60, 10_000, "Robinhood"),
    ];
    let snap = replay(&log, &[("AMZN", 26_126), ("GOOGL", 37_637)]);
    ViewBuilder::new(snap)
        .mark("AMZN", 26_126, Date(20_000))
        .mark("GOOGL", 37_637, Date(20_000))
        .estimate("AMZN", est("AMZN", 5_000_00, 1_000_00))
        .estimate("GOOGL", est("GOOGL", 1_600_00, 320_00))
        .build()
}

/// A per-symbol unrealized estimate (pretax + estimated tax in cents).
pub fn est(symbol: &str, pretax_cents: i64, tax_cents: i64) -> tax::UnrealizedEstimate {
    tax::UnrealizedEstimate {
        symbol: symbol.to_string(),
        unrealized_pretax_cents: Cents(pretax_cents),
        estimated_tax_cents: Some(Cents(tax_cents)),
        effective_rate_ppm: Some(config::Ppm(200_000)),
        bracket_state: config::BracketState::Verified,
    }
}

/// A fake runtime over the two-position view, federal flat 22% brackets.
pub fn two_position_runtime() -> FakeRuntime {
    FakeRuntime::new(two_position_view(), flat_federal_ctx(YEAR, 220_000))
}

/// Whole shares as MicroShares.
pub fn sh(n: i64) -> MicroShares {
    MicroShares(n * pt_core::SHARE_SCALE)
}
