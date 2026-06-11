//! Config-edit tests (TUI-ENTRY-CFG-*).

mod common;

use pt_core::Date;
use tui::entry::{self, ResidencyForm};

// @spec TUI-ENTRY-CFG-001
#[test]
fn residency_add_move_future_dated_ok_consecutive_same_state_rejected_with_note() {
    let existing = vec![config::ResidencyEntry {
        effective_date: Date(0),
        state_code: "DC".to_string(),
    }];

    // A future-dated move to a new state is accepted.
    let good = ResidencyForm {
        effective_date: Date(25_000),
        state: "NJ".to_string(),
    };
    assert!(
        good.validate(&existing).is_ok(),
        "future-dated move to a new state is allowed"
    );

    // A consecutive same-state move is rejected inline (a no-op move).
    let same = ResidencyForm {
        effective_date: Date(25_000),
        state: "DC".to_string(),
    };
    assert_eq!(
        same.validate(&existing),
        Err(config::ConfigError::ResidencyConsecutiveSameState),
        "consecutive same-state is rejected inline"
    );

    // The form notes the change affects only future-dated accrues_to_state defaults.
    assert!(ResidencyForm::future_only_note().contains("future-dated"));
    assert!(ResidencyForm::future_only_note().contains("not already-stamped events"));
}

// @spec TUI-ENTRY-CFG-001
#[test]
fn residency_founding_entry_violation_shown_inline() {
    // An import with no residency entry at/before the earliest event date is a
    // founding-entry violation — rendered as an inline config error. (TUI-ENTRY-CFG-001)
    let timeline = config::ResidencyTimeline::from_entries(vec![config::ResidencyEntry {
        effective_date: Date(20_000),
        state_code: "DC".to_string(),
    }])
    .unwrap();
    // The earliest event predates the founding entry.
    assert_eq!(
        config::check_import_founding_residency(&timeline, Date(10_000)),
        Err(config::ConfigError::MissingFoundingResidency)
    );
    // A founding entry at/before the earliest event clears it.
    assert!(config::check_import_founding_residency(&timeline, Date(20_500)).is_ok());
}

// @spec TUI-ENTRY-CFG-002
#[test]
fn tax_rule_edit_validates_inline_and_confirm_restates_the_retroactive_blast_radius() {
    // A non-monotonic / stacked-rate-breach bracket set is rejected inline by
    // config's validation; the confirm restates "reprices N unpaid accruals'
    // estimates". (TUI-ENTRY-CFG-002)
    use config::{BracketRow, BracketSet, Ppm};

    // A non-monotonic set (thresholds not strictly ascending) is rejected.
    let bad = BracketSet {
        rows: vec![
            BracketRow {
                lower_threshold_cents: pt_core::Cents(0),
                rate_ppm: Ppm(100_000),
            },
            BracketRow {
                lower_threshold_cents: pt_core::Cents(0),
                rate_ppm: Ppm(200_000),
            },
        ],
        last_verified: Date(19_000),
        source_note: "x".to_string(),
    };
    assert_eq!(
        config::validate_bracket_set(&bad),
        Err(config::ConfigError::ThresholdsNotStrictlyAscending),
        "a non-monotonic set is rejected inline"
    );

    // The confirm restates the retroactive blast radius.
    let notice = entry::bracket_reprice_notice(7);
    assert_eq!(notice, "this reprices 7 unpaid accruals' estimates");
}

// @spec TUI-ENTRY-CFG-003
#[test]
fn platform_and_alias_management() {
    // The platform suggestion list and the symbol→ticker alias map are managed.
    // (TUI-ENTRY-CFG-003)
    let platforms = config::PlatformList::new(vec!["Robinhood".to_string(), "Schwab".to_string()]);
    assert!(platforms.contains("Robinhood"));
    assert!(!platforms.contains("Fidelity"));

    let mut m = std::collections::BTreeMap::new();
    m.insert("GOOG".to_string(), "GOOGL".to_string());
    let aliases = config::AliasMap::new(m);
    assert_eq!(
        aliases.resolve("GOOG"),
        "GOOGL",
        "the alias map resolves symbol→ticker"
    );
    assert_eq!(
        aliases.resolve("AMZN"),
        "AMZN",
        "an unmapped symbol resolves to itself"
    );
}
