//! In-memory fake [`HistoryClient`] for ALL tests, plus the no-op [`Lock`] (re-
//! exported from `store`). Models the durable workbook History tab: the
//! last-wins-by-trading-day upsert (`batchUpdate`), the read-back, the
//! write-failure injection, the unreachable workbook, and out-of-band human edits
//! (a corrupted checksum cell, a reordered/duplicated key) so a test can watch the
//! on-read integrity check flag loudly. `runtime` supplies the real Sheets-backed
//! impl later (reports-design.md → "Snapshot Capture & Persistence").

use std::cell::RefCell;

use crate::{HistoryClient, HistoryError, HistoryRow, TradingDayKey};

pub use store::NoopLock;

/// The in-memory History-tab fake. Holds the durable rows in tab order; an upsert
/// overwrites the row sharing a trading-day key (last-wins — REPORT-HIST-002) or
/// appends a new key. A test can mutate `rows` directly to model an out-of-band
/// human edit, then drive the on-read integrity check against it.
pub struct InMemoryHistory {
    /// The durable rows, in tab order.
    rows: RefCell<Vec<HistoryRow>>,
    /// When `true`, `upsert_point` fails (models a failed `batchUpdate` so the
    /// capture is retried/flagged — a lost point is non-reconstructable).
    /// (REPORT-HIST-001)
    write_fails: RefCell<bool>,
    /// When `true`, `upsert_point` reports success but lands nothing — so the
    /// post-write read-back finds the point absent and returns control to the
    /// owner. (REPORT-HIST-001)
    drop_on_write: RefCell<bool>,
    /// When `true`, every read/write returns `Unreachable` (offline workbook).
    /// (REPORT-HIST-004)
    unreachable: RefCell<bool>,
    /// When `true`, `read_history` returns `UnparseableRow` — modeling the real
    /// Sheets-backed client meeting a stored row it cannot deserialize into a
    /// `SeriesPoint` (a malformed cell, a truncated row). The on-read integrity
    /// check must propagate it loudly rather than fold a corrupt series.
    /// (REPORT-HIST-003)
    read_unparseable: RefCell<bool>,
    /// How many upserts actually landed (a test confirms last-wins never grows the
    /// row count for a repeated key). (REPORT-HIST-002)
    upsert_count: RefCell<u32>,
}

impl Default for InMemoryHistory {
    fn default() -> Self {
        InMemoryHistory::new()
    }
}

impl InMemoryHistory {
    /// A fresh, empty History tab (no captured points yet — the series begins at
    /// the first capture, REPORT-VOT-005).
    pub fn new() -> Self {
        InMemoryHistory {
            rows: RefCell::new(Vec::new()),
            write_fails: RefCell::new(false),
            drop_on_write: RefCell::new(false),
            unreachable: RefCell::new(false),
            read_unparseable: RefCell::new(false),
            upsert_count: RefCell::new(0),
        }
    }

    /// A History tab seeded with explicit rows (test setup — bypasses the write
    /// path to model pre-existing or hand-edited content).
    pub fn seeded(rows: Vec<HistoryRow>) -> Self {
        let h = InMemoryHistory::new();
        *h.rows.borrow_mut() = rows;
        h
    }

    /// The current durable rows (test assertions / out-of-band mutation source).
    pub fn rows(&self) -> Vec<HistoryRow> {
        self.rows.borrow().clone()
    }

    /// Replace the rows wholesale — simulate a human edit / reorder / duplicate so
    /// the on-read integrity check can be exercised. (REPORT-HIST-003)
    pub fn set_rows(&self, rows: Vec<HistoryRow>) {
        *self.rows.borrow_mut() = rows;
    }

    /// Make `upsert_point` fail (models a failed `batchUpdate`). (REPORT-HIST-001)
    pub fn set_write_fails(&self, v: bool) {
        *self.write_fails.borrow_mut() = v;
    }

    /// Make the next write report success but land nothing, so the read-back finds
    /// the point absent and returns control to the owner. (REPORT-HIST-001)
    pub fn set_drop_on_write(&self, v: bool) {
        *self.drop_on_write.borrow_mut() = v;
    }

    /// Mark the workbook unreachable (every I/O returns `Unreachable`).
    /// (REPORT-HIST-004)
    pub fn set_unreachable(&self, v: bool) {
        *self.unreachable.borrow_mut() = v;
    }

    /// Make `read_history` return `UnparseableRow` — model the real Sheets-backed
    /// client meeting a stored row it cannot deserialize into a `SeriesPoint`, so a
    /// test can watch the on-read integrity check propagate it loudly rather than
    /// fold a corrupt series. (REPORT-HIST-003)
    pub fn set_read_unparseable(&self, v: bool) {
        *self.read_unparseable.borrow_mut() = v;
    }

    /// How many upserts the fake actually applied (a re-run of the same trading day
    /// overwrites; the row count does not grow). (REPORT-HIST-002)
    pub fn upsert_count(&self) -> u32 {
        *self.upsert_count.borrow()
    }

    /// The number of durable rows currently in the tab. (REPORT-HIST-002)
    pub fn row_count(&self) -> usize {
        self.rows.borrow().len()
    }

    /// The stored row for a trading-day key, if present.
    pub fn row_for(&self, key: TradingDayKey) -> Option<HistoryRow> {
        self.rows.borrow().iter().find(|r| r.key == key).cloned()
    }
}

impl HistoryClient for InMemoryHistory {
    fn read_history(&self) -> Result<Vec<HistoryRow>, HistoryError> {
        if *self.unreachable.borrow() {
            return Err(HistoryError::Unreachable);
        }
        // A stored row that cannot be deserialized into a `SeriesPoint` surfaces as
        // `UnparseableRow` from the real client; the on-read integrity check
        // ([`crate::read_history`]) must propagate it loudly. (REPORT-HIST-003)
        if *self.read_unparseable.borrow() {
            return Err(HistoryError::UnparseableRow);
        }
        Ok(self.rows.borrow().clone())
    }

    fn upsert_point(&mut self, row: &HistoryRow) -> Result<(), HistoryError> {
        if *self.unreachable.borrow() {
            return Err(HistoryError::Unreachable);
        }
        if *self.write_fails.borrow() {
            return Err(HistoryError::WriteVerifyFailed);
        }
        // One-shot: report success but land nothing (the row never materializes),
        // so the post-write read-back finds it absent. (REPORT-HIST-001)
        if *self.drop_on_write.borrow() {
            *self.drop_on_write.borrow_mut() = false;
            return Ok(());
        }
        *self.upsert_count.borrow_mut() += 1;
        // Last-wins by trading-day key: overwrite the row sharing the key, else
        // append. A re-run never duplicates. (REPORT-HIST-002)
        let mut rows = self.rows.borrow_mut();
        match rows.iter_mut().find(|r| r.key == row.key) {
            Some(existing) => *existing = row.clone(),
            None => rows.push(row.clone()),
        }
        Ok(())
    }
}
