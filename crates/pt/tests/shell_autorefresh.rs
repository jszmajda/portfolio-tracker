//! The TUI shell's **ambient auto-refresh decision** (TUI-VIEW-NAV-017/018/019). The
//! binary's event loop polls on a short tick and asks the pure
//! `pt::shell::should_autorefresh(elapsed, entry_open)` whether the hourly refresh is
//! due; these pin that decision — the hourly threshold, the entry-context suppression
//! (and that nothing else suppresses: the helper takes no help-overlay input), and the
//! tick-retry consequence of a failed attempt never advancing the clock — purely, with
//! no TTY and no real clock.

use std::time::Duration;

use pt::shell::{should_autorefresh, AUTOREFRESH_INTERVAL};

// @spec TUI-VIEW-NAV-017
#[test]
fn autorefresh_fires_once_an_hour_has_elapsed_with_no_entry_open() {
    // The shipped cadence is "every hour or so": exactly one hour.
    assert_eq!(AUTOREFRESH_INTERVAL, Duration::from_secs(60 * 60));

    // Under the threshold: not due — even one tick short.
    assert!(!should_autorefresh(Duration::ZERO, false));
    assert!(!should_autorefresh(Duration::from_secs(59 * 60), false));
    assert!(!should_autorefresh(
        AUTOREFRESH_INTERVAL - Duration::from_secs(1),
        false
    ));

    // At and past the threshold: due.
    assert!(should_autorefresh(AUTOREFRESH_INTERVAL, false));
    assert!(should_autorefresh(
        AUTOREFRESH_INTERVAL + Duration::from_secs(1),
        false
    ));
    assert!(should_autorefresh(Duration::from_secs(9 * 60 * 60), false));
}

// @spec TUI-VIEW-NAV-018
#[test]
fn autorefresh_is_suppressed_while_an_entry_context_is_open_and_runs_on_return() {
    // Overdue but composing: suppressed — a mid-composition reflow would yank
    // fields/focus. Suppression is the ONLY input besides elapsed time: the helper
    // takes no help-overlay flag, so an open help overlay cannot suppress the
    // refresh (the overlay is chrome over the screen it covers).
    let overdue = AUTOREFRESH_INTERVAL + Duration::from_secs(30 * 60);
    assert!(!should_autorefresh(overdue, true));

    // The same overdue elapsed with the entry stack emptied (the next tick after
    // the composer closes): the deferred refresh fires.
    assert!(should_autorefresh(overdue, false));

    // Suppression never *causes* a refresh either: not due + composing = not due.
    assert!(!should_autorefresh(Duration::from_secs(60), true));
}

// @spec TUI-VIEW-NAV-019
#[test]
fn a_failed_autorefresh_leaves_the_clock_unadvanced_so_the_next_tick_retries() {
    // The loop advances the last-successful clock ONLY on a live refreshed view; a
    // failed attempt leaves it where it was, so on the next ~1s tick the elapsed
    // time has only grown and the decision is still "due" — retry until one lands.
    let mut elapsed = AUTOREFRESH_INTERVAL; // the attempt that just failed
    for _ in 0..3 {
        assert!(
            should_autorefresh(elapsed, false),
            "still due on the next tick"
        );
        elapsed += Duration::from_secs(1); // the clock was not advanced; time passes
    }

    // A SUCCESSFUL refresh resets the elapsed time to zero: not due again until a
    // fresh hour passes.
    assert!(!should_autorefresh(Duration::ZERO, false));
}
