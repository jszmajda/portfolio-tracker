//! In-memory fake [`SheetsViewClient`] for ALL tests. Models the view-tab
//! workbook: the atomically-published view tabs (with their never-rewritten
//! frozen headers and tail-truncated data ranges), the workbook tab ordering,
//! the programmable `GOOGLEFINANCE` settle-pass readings (so a test can stage a
//! transient→numeric or transient→permanent sequence and watch the settle loop),
//! the per-tab STALE banners, and the publish-failure injection. `runtime`
//! supplies the real Sheets-backed impl later (sheets-view-design.md → "Trust
//! Boundary & Interfaces").

use std::cell::RefCell;
use std::collections::BTreeMap;

use ledger_core::Symbol;

use crate::{PricePass, PriceReading, SheetsViewClient, ViewError, ViewRow, ViewTab};

/// A published view tab as it sits in the fake workbook: the frozen header and
/// the data rows written from the anchored start row. A republish overwrites the
/// data rows wholesale (tail-truncated to the new row count) but never the
/// header — mirroring the atomic `batchUpdate` + tail-truncate. (SHEET-PUB-002,
/// SHEET-FORMULA-002, SHEET-TAB-003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PublishedTab {
    pub header: Vec<String>,
    pub rows: Vec<ViewRow>,
    /// `true` once the frozen header has been written; a republish must leave it
    /// byte-identical thereafter (the fake asserts this). (SHEET-FORMULA-002)
    pub header_frozen: bool,
}

/// The in-memory view-tab workbook fake.
pub struct InMemorySheetsView {
    /// Published tabs by name (the atomic batchUpdate result).
    tabs: RefCell<BTreeMap<String, PublishedTab>>,
    /// The last asserted workbook tab order (idempotent reassert). (SHEET-TAB-002)
    tab_order: RefCell<Vec<String>>,
    /// Per-tab STALE banner text (best-effort single-cell). (SHEET-PUB-003)
    banners: RefCell<BTreeMap<String, String>>,
    /// The programmable settle-pass script: a queue of price passes, popped one
    /// per `read_price_pass` call (the last is repeated once exhausted). Models
    /// `GOOGLEFINANCE` recalculating asynchronously after a write. (SHEET-MARK-001/003)
    price_passes: RefCell<Vec<PricePass>>,
    /// How many settle-pass reads have happened (a test asserts the bounded poll
    /// count / that reads are a separate pass). (SHEET-MARK-001)
    read_count: RefCell<u32>,
    /// When `true`, `batch_update_view` fails (models a failed republish so the
    /// view is left stale). (SHEET-PUB-003)
    publish_fails: RefCell<bool>,
    /// When `true`, even the best-effort banner write fails (network fully down).
    /// (SHEET-PUB-003)
    banner_fails: RefCell<bool>,
    /// When `true`, `set_tab_order` fails (a partial republish failure).
    order_fails: RefCell<bool>,
    /// When `true`, `read_price_pass` fails (the workbook is unreachable /
    /// offline) so a test can assert `read_marks` propagates the error rather than
    /// silently emitting an empty marks set. (SHEET-MARK-005)
    read_fails: RefCell<bool>,
    /// An append-only log of seam calls in the exact order they happened, so a
    /// test can assert every settle-pass read happens AFTER all the formula
    /// writes in a `republish_then_settle` cycle (the read is a separate pass,
    /// never inline with the write). (SHEET-MARK-001)
    call_log: RefCell<Vec<SeamCall>>,
}

/// One recorded seam call, for the ordering assertion. (SHEET-MARK-001)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeamCall {
    /// A `batch_update_view` (formula/value write) for a tab.
    BatchUpdate,
    /// A `read_price_pass` (settle-pass read).
    ReadPricePass,
    /// A `set_tab_order` reassert.
    SetTabOrder,
}

impl Default for InMemorySheetsView {
    fn default() -> Self {
        InMemorySheetsView::new()
    }
}

impl InMemorySheetsView {
    /// A fresh, empty fake (no tabs published, no settle script).
    pub fn new() -> Self {
        InMemorySheetsView {
            tabs: RefCell::new(BTreeMap::new()),
            tab_order: RefCell::new(Vec::new()),
            banners: RefCell::new(BTreeMap::new()),
            price_passes: RefCell::new(Vec::new()),
            read_count: RefCell::new(0),
            publish_fails: RefCell::new(false),
            banner_fails: RefCell::new(false),
            order_fails: RefCell::new(false),
            read_fails: RefCell::new(false),
            call_log: RefCell::new(Vec::new()),
        }
    }

    /// Stage the settle-pass script: each pass is returned by one
    /// `read_price_pass` call, in order; once the queue is down to one it repeats.
    /// So `[transient_pass, numeric_pass]` models a `Loading...` then a settled
    /// price on the next poll. (SHEET-MARK-001/003)
    pub fn set_price_script(&self, passes: Vec<PricePass>) {
        *self.price_passes.borrow_mut() = passes;
    }

    /// A convenience: one settle pass that is the same for every poll (the price
    /// settles on the first read). (SHEET-MARK-001)
    pub fn set_prices(&self, pass: PricePass) {
        *self.price_passes.borrow_mut() = vec![pass];
    }

    /// How many times the settle pass was read (so a test confirms reads are a
    /// separate pass and the poll count is bounded). (SHEET-MARK-001)
    pub fn read_count(&self) -> u32 {
        *self.read_count.borrow()
    }

    /// Make the next (and subsequent) `batch_update_view` calls fail until reset.
    /// (SHEET-PUB-003)
    pub fn set_publish_fails(&self, v: bool) {
        *self.publish_fails.borrow_mut() = v;
    }

    /// Make the best-effort banner write fail too (network fully down). (SHEET-PUB-003)
    pub fn set_banner_fails(&self, v: bool) {
        *self.banner_fails.borrow_mut() = v;
    }

    /// Make `set_tab_order` fail. (SHEET-TAB-002)
    pub fn set_order_fails(&self, v: bool) {
        *self.order_fails.borrow_mut() = v;
    }

    /// Make `read_price_pass` fail (the workbook is unreachable / offline). A test
    /// asserts `read_marks` propagates the error rather than silently emitting an
    /// empty marks set. (SHEET-MARK-005)
    pub fn set_read_fails(&self, v: bool) {
        *self.read_fails.borrow_mut() = v;
    }

    /// The recorded seam-call order, so a test can assert every settle-pass read
    /// happened after all the formula writes in a cycle. (SHEET-MARK-001)
    pub fn call_log(&self) -> Vec<SeamCall> {
        self.call_log.borrow().clone()
    }

    /// The published tab by name (test assertions on the atomic result).
    pub fn published(&self, tab: &str) -> Option<PublishedTab> {
        self.tabs.borrow().get(tab).cloned()
    }

    /// The current asserted workbook tab order. (SHEET-TAB-002)
    pub fn tab_order(&self) -> Vec<String> {
        self.tab_order.borrow().clone()
    }

    /// The STALE banner text written for a tab, if any. (SHEET-PUB-003)
    pub fn banner(&self, tab: &str) -> Option<String> {
        self.banners.borrow().get(tab).cloned()
    }
}

impl SheetsViewClient for InMemorySheetsView {
    fn batch_update_view(&mut self, tab: &ViewTab) -> Result<(), ViewError> {
        self.call_log.borrow_mut().push(SeamCall::BatchUpdate);
        if *self.publish_fails.borrow() {
            return Err(ViewError::PublishFailed);
        }
        let mut tabs = self.tabs.borrow_mut();
        // Atomic rewrite: write the new data range and TRUNCATE the residual tail
        // to the exact row count (replace `rows` wholesale, never append). The
        // frozen header is written once and never rewritten thereafter — a
        // republish must present a byte-identical header. This is the hard
        // SheetsViewClient contract (see the trait doc), so the fake enforces it
        // unconditionally (a panic in any build), not via a release-stripped
        // debug_assert. (SHEET-PUB-002, SHEET-FORMULA-002, SHEET-TAB-003)
        match tabs.get_mut(&tab.name) {
            Some(existing) => {
                assert_eq!(
                    existing.header, tab.header,
                    "the frozen header must never be rewritten on republish"
                );
                existing.rows = tab.rows.clone(); // tail-truncate: wholesale replace
            }
            None => {
                tabs.insert(
                    tab.name.clone(),
                    PublishedTab {
                        header: tab.header.clone(),
                        rows: tab.rows.clone(),
                        header_frozen: true,
                    },
                );
            }
        }
        Ok(())
    }

    fn set_tab_order(&mut self, order: &[&str]) -> Result<(), ViewError> {
        self.call_log.borrow_mut().push(SeamCall::SetTabOrder);
        if *self.order_fails.borrow() {
            return Err(ViewError::PublishFailed);
        }
        *self.tab_order.borrow_mut() = order.iter().map(|s| s.to_string()).collect();
        Ok(())
    }

    fn read_price_pass(&self) -> Result<PricePass, ViewError> {
        self.call_log.borrow_mut().push(SeamCall::ReadPricePass);
        *self.read_count.borrow_mut() += 1;
        if *self.read_fails.borrow() {
            // The workbook is unreachable / offline. (SHEET-MARK-005)
            return Err(ViewError::PublishFailed);
        }
        let mut passes = self.price_passes.borrow_mut();
        if passes.is_empty() {
            return Ok(BTreeMap::new());
        }
        // Pop the front pass; once only one remains, repeat it (the price has
        // settled and stays settled).
        if passes.len() > 1 {
            Ok(passes.remove(0))
        } else {
            Ok(passes[0].clone())
        }
    }

    fn write_stale_banner(&mut self, tab: &str, banner: &str) -> Result<(), ViewError> {
        if *self.banner_fails.borrow() {
            return Err(ViewError::PublishFailed);
        }
        self.banners
            .borrow_mut()
            .insert(tab.to_string(), banner.to_string());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Settle-pass construction helpers for tests.
// ---------------------------------------------------------------------------

/// A settle pass mapping each `(symbol, reading)` pair. (SHEET-MARK-001)
pub fn pass(entries: &[(&str, PriceReading)]) -> PricePass {
    entries
        .iter()
        .map(|(s, r)| (Symbol::from(*s), r.clone()))
        .collect()
}
