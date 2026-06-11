//! History Capture & Persistence: REPORT-HIST-001/002/003/004.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use pt_core::Date;
use reports::testkit::{InMemoryHistory, NoopLock};
use reports::{
    append_snapshot, build_series_point, point_checksum, read_history, read_series_cache_checked,
    HistoryError, HistoryRow, SeriesPoint, TradingDayKey,
};

/// Build a series point for the small portfolio at a trading-day key.
fn point_at(key_day: i32, amzn_cents: i64) -> SeriesPoint {
    let marks = priced_marks(&[("AMZN", amzn_cents, key_day), ("GOOG", 120_00, key_day)]);
    let snap = small_snapshot(&ledger_marks(&[("AMZN", amzn_cents), ("GOOG", 120_00)]));
    let as_of = Date(19_500);
    let ctx = ctx(2022);
    let est = estimates(&snap, as_of, &ctx);
    build_series_point(
        &snap,
        &marks,
        &est,
        TradingDayKey(Date(key_day)),
        1_700_000_000,
        Date(19_500),
    )
}

/// A well-formed History row for a point (checksum computed honestly).
fn row(point: &SeriesPoint) -> HistoryRow {
    HistoryRow {
        key: point.key,
        point: point.clone(),
        checksum: point_checksum(point),
    }
}

// @spec REPORT-HIST-001
#[test]
fn append_snapshot_writes_via_lock_and_read_back_verifies() {
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    let p = point_at(19_490, 200_00);

    append_snapshot(&mut client, &lock, &p).expect("capture lands");

    // The advisory write-lock was acquired INSIDE the primitive (a TUI and a cron
    // capture cannot race).
    assert!(
        lock.acquire_count() >= 1,
        "append_snapshot must acquire the lock"
    );
    // The point landed durably and reads back equal.
    let stored = client
        .row_for(TradingDayKey(Date(19_490)))
        .expect("row present");
    assert_eq!(stored.point, p);
}

// @spec REPORT-HIST-001
#[test]
fn failed_batch_update_is_flagged_not_silently_lost() {
    let mut client = InMemoryHistory::new();
    client.set_write_fails(true);
    let lock = NoopLock::new();
    let p = point_at(19_490, 200_00);

    // A failed batchUpdate returns control to the owner to retry — a lost point is
    // non-reconstructable, so it is never a silent success.
    let r = append_snapshot(&mut client, &lock, &p);
    assert_eq!(r, Err(HistoryError::WriteVerifyFailed));
    assert_eq!(client.row_count(), 0, "nothing landed");
}

// @spec REPORT-HIST-001
#[test]
fn dropped_write_fails_read_back_returns_control_to_owner() {
    // The write reports success but lands nothing; the post-write read-back finds
    // the point absent and flags rather than confirming a phantom capture.
    let mut client = InMemoryHistory::new();
    client.set_drop_on_write(true);
    let lock = NoopLock::new();
    let p = point_at(19_490, 200_00);

    let r = append_snapshot(&mut client, &lock, &p);
    assert_eq!(r, Err(HistoryError::WriteVerifyFailed));
}

// @spec REPORT-HIST-002
#[test]
fn capture_for_existing_trading_day_overwrites_last_wins_no_duplicate() {
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();

    // First capture for the trading day.
    let p1 = point_at(19_490, 200_00);
    append_snapshot(&mut client, &lock, &p1).unwrap();
    // A SAME-day retake (re-run) refreshes that day — overwrite, never duplicate.
    let p2 = point_at(19_490, 215_00);
    append_snapshot(&mut client, &lock, &p2).unwrap();

    assert_eq!(
        client.row_count(),
        1,
        "re-run never duplicates the trading day"
    );
    let stored = client.row_for(TradingDayKey(Date(19_490))).unwrap();
    assert_eq!(stored.point, p2, "last-wins by trading-day key");

    // A capture for a DIFFERENT trading day appends a new row.
    let p3 = point_at(19_491, 220_00);
    append_snapshot(&mut client, &lock, &p3).unwrap();
    assert_eq!(client.row_count(), 2);
}

// @spec REPORT-HIST-003
#[test]
fn read_history_returns_points_in_key_order_when_valid() {
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    append_snapshot(&mut client, &lock, &point_at(19_490, 200_00)).unwrap();
    append_snapshot(&mut client, &lock, &point_at(19_492, 220_00)).unwrap();

    let points = read_history(&client).expect("valid series reads");
    assert_eq!(points.len(), 2);
    // Ascending, non-decreasing by trading-day key.
    assert_eq!(points[0].key, TradingDayKey(Date(19_490)));
    assert_eq!(points[1].key, TradingDayKey(Date(19_492)));
}

// @spec REPORT-HIST-003
#[test]
fn read_history_flags_loudly_on_non_monotonic_keys() {
    // An out-of-band human edit reorders the rows so the keys are NOT
    // non-decreasing (or duplicated). The integrity check must flag loudly rather
    // than fold a corrupt series.
    let p1 = point_at(19_490, 200_00);
    let p2 = point_at(19_492, 220_00);
    // Rows in DESCENDING key order (and thus non-monotonic as stored).
    let client = InMemoryHistory::seeded(vec![row(&p2), row(&p1)]);
    let r = read_history(&client);
    assert_eq!(r, Err(HistoryError::NonMonotonicKeys));

    // A duplicate key is also flagged.
    let dup = InMemoryHistory::seeded(vec![row(&p1), row(&p1)]);
    assert_eq!(read_history(&dup), Err(HistoryError::NonMonotonicKeys));
}

// @spec REPORT-HIST-003
#[test]
fn read_history_flags_loudly_on_checksum_mismatch() {
    // A human edits a stored point's value but not its checksum cell — the content
    // checksum no longer matches, so the integrity check flags loudly (it cannot
    // recompute the non-reconstructable value; recovery is via Sheets history).
    let p = point_at(19_490, 200_00);
    let mut bad = row(&p);
    // Tamper with the persisted point (different total) while leaving the stored
    // checksum stale.
    bad.point.total_market_value_cents = pt_core::Cents(999_99);
    let client = InMemoryHistory::seeded(vec![bad]);
    assert_eq!(read_history(&client), Err(HistoryError::ChecksumMismatch));
}

// @spec REPORT-HIST-003
#[test]
fn read_history_flags_loudly_on_unparseable_row() {
    // The real Sheets-backed client meets a stored row it cannot deserialize into a
    // SeriesPoint (a malformed/truncated cell): it surfaces UnparseableRow across
    // the trait boundary, and the on-read integrity check propagates it loudly
    // rather than folding a corrupt, non-reconstructable series — exercising the
    // third "rows parse" sub-requirement of REPORT-HIST-003 through read_history.
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    append_snapshot(&mut client, &lock, &point_at(19_490, 200_00)).unwrap();
    // A row exists, but the client now reports it as unparseable on read.
    client.set_read_unparseable(true);
    assert_eq!(read_history(&client), Err(HistoryError::UnparseableRow));
}

// @spec REPORT-HIST-004
#[test]
fn cache_currency_check_workbook_wins_and_cache_is_reread_not_recomputed() {
    // The workbook holds the authoritative series; the local cache DIVERGES (a
    // stale/edited copy). On read the workbook wins and the series is re-READ from
    // the workbook (not recomputed), and the divergence is reported.
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    append_snapshot(&mut client, &lock, &point_at(19_490, 200_00)).unwrap();
    append_snapshot(&mut client, &lock, &point_at(19_492, 220_00)).unwrap();

    // A cache that disagrees with the workbook (missing the second point).
    let stale_cache = vec![point_at(19_490, 200_00)];
    let (series, diverged) = read_series_cache_checked(&client, &stale_cache).unwrap();
    assert!(diverged, "a divergent cache is detected");
    // The returned series is the WORKBOOK's (authoritative), re-read, never the
    // stale cache and never recomputed.
    assert_eq!(series.len(), 2);
    assert_eq!(series[1].key, TradingDayKey(Date(19_492)));
}

// @spec REPORT-HIST-004
#[test]
fn cache_in_sync_with_workbook_reports_no_divergence() {
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    append_snapshot(&mut client, &lock, &point_at(19_490, 200_00)).unwrap();

    let cache = read_history(&client).unwrap();
    let (series, diverged) = read_series_cache_checked(&client, &cache).unwrap();
    assert!(!diverged, "an in-sync cache reports no divergence");
    assert_eq!(series.len(), 1);
}
