//! In-memory fakes for ALL tests: an [`InMemorySheets`] modeling the two
//! append-only tabs (the authoritative workbook), plus re-exports of the no-op
//! [`crate::sheets::NoopLock`] and in-memory [`crate::cache::InMemoryCache`].
//! `runtime` supplies the real impls later (store-design.md → "Interfaces").
//!
//! The fake computes the cheap probe ([`crate::sheets::ProbeCells`]) directly
//! from its in-memory rows the way `runtime`'s real formula cells would, so a
//! test can simulate appends, deletions, and in-place edits and watch the probe
//! catch them (STORE-CACHE-002).

use std::cell::RefCell;

use crate::sheets::{ProbeCells, SheetsClient};
use crate::{Row, StoreError, Tab};

/// An in-memory model of the workbook's two append-only event-log tabs (the
/// authoritative store). Tests mutate it directly to simulate out-of-band human
/// edits, then drive `store` against it.
pub struct InMemorySheets {
    ledger: RefCell<Vec<Row>>,
    tax: RefCell<Vec<Row>>,
    /// When `true`, every read/append returns [`StoreError::Unreachable`],
    /// modeling an offline / unreachable workbook (STORE-WRITE-005).
    unreachable: bool,
    /// When `true`, the cheap fingerprint cells read as missing/errored, which
    /// the probe must treat as a change (STORE-CACHE-002).
    fingerprint_broken: bool,
    /// When `true`, `append_row` lands a row whose `FeesCents` cell is coerced to
    /// a differing value — modeling a Sheets write that landed but differs from
    /// what was written, so the post-append read-back must see a field mismatch
    /// (STORE-WRITE-004).
    mutate_on_append: RefCell<bool>,
    /// When `true`, `append_row` silently drops the row — modeling an append that
    /// reported success but left no row, so the post-append read-back finds
    /// nothing and returns control to the owner (STORE-WRITE-005).
    drop_on_append: RefCell<bool>,
    /// Per-tab "the tab does not exist" markers `(ledger, tax)` — a fresh
    /// workbook that has never been written. Every I/O against a missing tab
    /// returns [`StoreError::TabMissing`] (the real client's classification of
    /// the Sheets missing-tab error); [`SheetsClient::ensure_tab`] creates it.
    /// (STORE-LOAD-007 / STORE-WRITE-009)
    missing: RefCell<(bool, bool)>,
}

impl Default for InMemorySheets {
    fn default() -> Self {
        InMemorySheets::new()
    }
}

impl InMemorySheets {
    /// A fresh, empty (cold) workbook: both tabs have headers only, no data rows.
    pub fn new() -> Self {
        InMemorySheets {
            ledger: RefCell::new(Vec::new()),
            tax: RefCell::new(Vec::new()),
            unreachable: false,
            fingerprint_broken: false,
            mutate_on_append: RefCell::new(false),
            drop_on_append: RefCell::new(false),
            missing: RefCell::new((false, false)),
        }
    }

    /// A brand-new workbook on which NEITHER event-log tab exists yet (the state
    /// of a workbook `pt` has never written): every I/O returns `TabMissing`
    /// until `ensure_tab` creates the tab. (STORE-LOAD-007 / STORE-WRITE-009)
    pub fn fresh_workbook() -> Self {
        let s = InMemorySheets::new();
        *s.missing.borrow_mut() = (true, true);
        s
    }

    /// A workbook seeded with explicit ledger / tax data rows (test setup —
    /// bypasses the write path to model pre-existing or hand-edited content).
    pub fn seeded(ledger: Vec<Row>, tax: Vec<Row>) -> Self {
        InMemorySheets {
            ledger: RefCell::new(ledger),
            tax: RefCell::new(tax),
            unreachable: false,
            fingerprint_broken: false,
            mutate_on_append: RefCell::new(false),
            drop_on_append: RefCell::new(false),
            missing: RefCell::new((false, false)),
        }
    }

    /// Mark a single tab as not existing (test setup for the one-tab-missing
    /// cold-start case). (STORE-LOAD-007)
    pub fn set_tab_missing(&self, tab: Tab, v: bool) {
        let mut m = self.missing.borrow_mut();
        match tab {
            Tab::Ledger => m.0 = v,
            Tab::Tax => m.1 = v,
        }
    }

    /// Whether a tab is currently marked missing (test assertions on the
    /// `ensure_tab` bootstrap). (STORE-WRITE-009)
    pub fn tab_missing(&self, tab: Tab) -> bool {
        let m = self.missing.borrow();
        match tab {
            Tab::Ledger => m.0,
            Tab::Tax => m.1,
        }
    }

    /// The missing-tab gate shared by every I/O: `Unreachable` wins (an offline
    /// workbook hides tab existence), then a missing tab returns `TabMissing`.
    fn gate(&self, tab: Tab) -> Result<(), StoreError> {
        if self.unreachable {
            return Err(StoreError::Unreachable);
        }
        if self.tab_missing(tab) {
            return Err(StoreError::TabMissing);
        }
        Ok(())
    }

    /// Mark the workbook unreachable (every I/O returns `Unreachable`).
    /// (STORE-WRITE-005 / STORE-CACHE-005)
    pub fn set_unreachable(&mut self, v: bool) {
        self.unreachable = v;
    }

    /// Break the fingerprint cells (read as missing/errored → probe sees a
    /// change). (STORE-CACHE-002)
    pub fn set_fingerprint_broken(&mut self, v: bool) {
        self.fingerprint_broken = v;
    }

    /// Next `append_row` lands a row that DIFFERS from what was written (its
    /// `FeesCents` cell is coerced), modeling a Sheets write that landed a
    /// differing value so the post-append read-back sees a field mismatch
    /// (STORE-WRITE-004). One-shot: cleared after it fires. Takes `&self` so a
    /// test can arm it on a `Store`-owned client via [`crate::Store::sheets`].
    pub fn set_mutate_on_append(&self, v: bool) {
        *self.mutate_on_append.borrow_mut() = v;
    }

    /// Next `append_row` silently drops the row (reports success, stores nothing),
    /// modeling an append whose row never materialized so the post-append
    /// read-back finds no row and returns control to the owner (STORE-WRITE-005).
    /// One-shot: cleared after it fires.
    pub fn set_drop_on_append(&self, v: bool) {
        *self.drop_on_append.borrow_mut() = v;
    }

    fn tab_rows(&self, tab: Tab) -> &RefCell<Vec<Row>> {
        match tab {
            Tab::Ledger => &self.ledger,
            Tab::Tax => &self.tax,
        }
    }

    /// Direct access to a tab's rows for test assertions / out-of-band mutation.
    pub fn rows(&self, tab: Tab) -> Vec<Row> {
        self.tab_rows(tab).borrow().clone()
    }

    /// Replace a tab's rows wholesale (simulate a human edit / deletion).
    pub fn set_rows(&self, tab: Tab, rows: Vec<Row>) {
        *self.tab_rows(tab).borrow_mut() = rows;
    }

    /// Push a raw row to a tab, bypassing the write path (test setup).
    pub fn push_raw(&self, tab: Tab, row: Row) {
        self.tab_rows(tab).borrow_mut().push(row);
    }

    /// The columns the cheap `SUMPRODUCT` checksum covers, mirroring the design's
    /// actual coverage: the `Seq`/`Date`/money columns plus character-code sums
    /// over the designated KEY text columns (`EventId`, `Kind`, `Symbol`).
    /// Deliberately NOT every column — fields like `Reason`, `AccountLabel`,
    /// `TrackingCode`, `LotRefs`, and `Covers` sit OUTSIDE the cheap checksum's
    /// reach, so an out-of-band edit to one of them is caught only by the content
    /// hash (the belt-and-suspenders, STORE-CACHE-003) — exactly the real formula's
    /// blind spot, so a test can surface a column the cheap probe would miss.
    /// (store-design.md → "Local Cache"; STORE-CACHE-002)
    fn checksum_covers(col: &str) -> bool {
        matches!(
            col,
            // Seq / Date.
            "Seq" | "Date"
            // Money + share columns.
            | "UnitPriceCents" | "FeesCents" | "FmvPerShareCents"
            | "AmountCents" | "AppliedAmountCents" | "Qty"
            // Designated key text columns.
            | "EventId" | "Kind" | "Symbol"
        )
    }

    /// Compute the cheap probe directly the way the real formula cells would: the
    /// COUNTA, MAX(Seq), and a SUMPRODUCT-style checksum over the Seq/Date/money +
    /// designated key text columns ([`Self::checksum_covers`]) — NOT every cell, so
    /// the fake matches the design's actual checksum coverage and an edit outside
    /// it falls to the content hash. (STORE-CACHE-002)
    fn compute_probe(&self, tab: Tab) -> ProbeCells {
        if self.fingerprint_broken {
            return ProbeCells::default(); // all None → treated as a change
        }
        let rows = self.tab_rows(tab).borrow();
        let count = rows.len() as i64;
        let mut max_seq: i64 = 0;
        let mut checksum: i64 = 0;
        for (i, row) in rows.iter().enumerate() {
            // MAX(Seq): the Seq column.
            if let Ok(s) = row.get("Seq").trim().parse::<i64>() {
                if s > max_seq {
                    max_seq = s;
                }
            }
            // SUMPRODUCT-weighted checksum over only the covered columns: a
            // position- and content-sensitive sum, so an append (new row), a
            // deletion (fewer rows), or an in-place edit to a COVERED cell all
            // shift it; an edit to an uncovered cell does not (the content hash
            // catches that). (STORE-CACHE-002/003)
            for (col, val) in &row.cells {
                if !Self::checksum_covers(col) {
                    continue;
                }
                let col_sum: i64 = col.bytes().map(|b| b as i64).sum();
                let val_sum: i64 = val.bytes().map(|b| b as i64).sum();
                checksum = checksum
                    .wrapping_add((i as i64 + 1).wrapping_mul(col_sum.wrapping_add(val_sum)));
            }
        }
        ProbeCells {
            count: Some(count),
            max_seq: Some(max_seq),
            checksum: Some(checksum),
        }
    }
}

impl SheetsClient for InMemorySheets {
    fn read_rows(&self, tab: Tab) -> Result<Vec<Row>, StoreError> {
        self.gate(tab)?;
        Ok(self.tab_rows(tab).borrow().clone())
    }

    fn append_row(&mut self, tab: Tab, row: &Row) -> Result<(), StoreError> {
        self.gate(tab)?;
        // One-shot: drop the row (append "succeeds" but nothing lands), so the
        // post-append read-back finds no row (STORE-WRITE-005).
        if *self.drop_on_append.borrow() {
            *self.drop_on_append.borrow_mut() = false;
            return Ok(());
        }
        // One-shot: land a row that DIFFERS from what was written (a coerced
        // field), so the post-append read-back sees a field mismatch
        // (STORE-WRITE-004).
        if *self.mutate_on_append.borrow() {
            *self.mutate_on_append.borrow_mut() = false;
            let mut landed = row.clone();
            landed.set("FeesCents", "424242");
            self.tab_rows(tab).borrow_mut().push(landed);
            return Ok(());
        }
        self.tab_rows(tab).borrow_mut().push(row.clone());
        Ok(())
    }

    fn read_probe(&self, tab: Tab) -> Result<ProbeCells, StoreError> {
        self.gate(tab)?;
        Ok(self.compute_probe(tab))
    }

    fn batch_update(&mut self, tab: Tab, rows: &[Row]) -> Result<(), StoreError> {
        self.gate(tab)?;
        *self.tab_rows(tab).borrow_mut() = rows.to_vec();
        Ok(())
    }

    fn clear(&mut self, tab: Tab) -> Result<(), StoreError> {
        self.gate(tab)?;
        self.tab_rows(tab).borrow_mut().clear();
        Ok(())
    }

    fn ensure_tab(&mut self, tab: Tab) -> Result<(), StoreError> {
        if self.unreachable {
            return Err(StoreError::Unreachable);
        }
        // Creating the tab clears its missing marker (header + fingerprint block
        // are modeled implicitly: the fake's rows exclude the header and its probe
        // is computed, the way the real formula cells would be). A no-op when the
        // tab already exists. (STORE-WRITE-009)
        self.set_tab_missing(tab, false);
        Ok(())
    }
}
