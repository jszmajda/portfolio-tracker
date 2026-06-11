//! `store` — the single Sheets-row ↔ event trust seam.
//!
//! Owns the workbook's two append-only event-log tabs (`Ledger Events`,
//! `Tax Events`), the **total structural** row ↔ event serialization that
//! crosses into/out of the verified kernels (`ledger-core`, `tax`), the
//! **append + read-back-verify** write path, the **rebuildable local cache**
//! (two-tier currency probe), and the **on-load integrity check**. It holds NO
//! accounting/tax math — that is the kernels'. See
//! `docs/intent/store/store-design.md` and `-specs.md` (prefix `STORE`).
//!
//! This crate is the project's one unverified seam: all serde and I/O live here,
//! OUTSIDE the `verus!{}` boundary (STORE-GUARD-003). It is NOT a `verus!{}`
//! crate; `cargo test` is the gate. The row ↔ event conversion is locked by the
//! **drift-guard** test (STORE-GUARD-001/002), the prior project's drift guard
//! analog: a wildcard-free exhaustive match over every `Kind`, plus a round-trip
//! identity property over every kind and field.
//!
//! The low-level Sheets access layer and the advisory write-lock are
//! `runtime`'s; `store` depends on the [`SheetsClient`] and [`Lock`] traits, with
//! in-memory / no-op fakes (`testkit`) for ALL tests. Runtime supplies the real
//! impls later. The advisory write-lock is acquired INSIDE the append primitive.

use std::collections::BTreeMap;

use ledger_core::LedgerEvent;
use pt_core::Seq;
use tax::TaxEvent;

pub mod cache;
pub mod serde_rows;
pub mod sheets;
pub mod testkit;

pub use cache::{Cache, Fingerprint, InMemoryCache};
pub use sheets::{Lock, NoopLock, SheetsClient};

// ===========================================================================
// Tab identity & schema (store-design.md → "Workbook Event-Log Schema";
// STORE-SCHEMA-001). Two append-only tabs, typed columns, sparse population.
// ===========================================================================

/// The append-only `Ledger Events` tab name.
pub const LEDGER_TAB: &str = "Ledger Events";
/// The append-only `Tax Events` tab name.
pub const TAX_TAB: &str = "Tax Events";

/// Which of the two event-log tabs a row/event belongs to. The two tabs have
/// **independent** dense `Seq` sequences (STORE-SCHEMA-002).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Tab {
    /// `LedgerEvent`s (Buy / Vest / Sell / Split / Reversal).
    Ledger,
    /// `TaxEvent`s (Allocate / Move / Pay / AmountOverride / SeedMigration).
    Tax,
}

impl Tab {
    /// The human-readable tab name.
    pub fn name(self) -> &'static str {
        match self {
            Tab::Ledger => LEDGER_TAB,
            Tab::Tax => TAX_TAB,
        }
    }

    /// The ordered typed-column header for this tab. The universal columns
    /// (`Seq`, `EventId`, `Date`, `Kind`) lead; the union of that family's
    /// fields follows (sparse — only the columns a `Kind` uses are populated).
    /// (STORE-SCHEMA-001)
    pub fn header(self) -> &'static [&'static str] {
        match self {
            Tab::Ledger => serde_rows::LEDGER_HEADER,
            Tab::Tax => serde_rows::TAX_HEADER,
        }
    }
}

// ===========================================================================
// Row model (store-design.md → "Workbook Event-Log Schema"). A `Row` is the
// typed-column cell vector as it sits in a Sheets tab — a JSON-blob column is
// rejected (it would make the tab unreadable/unfilterable). Each cell is a
// string keyed by its column name; an unpopulated column is the empty string.
// ===========================================================================

/// One workbook row: column-name → cell string, sparse. The order of columns is
/// fixed by the tab's [`Tab::header`]; a missing/sparse column is the empty
/// string. This is the wire shape the [`SheetsClient`] reads and appends.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Row {
    /// Column name → cell value. Columns not present read as the empty string.
    pub cells: BTreeMap<String, String>,
}

impl Row {
    /// An empty row.
    pub fn new() -> Self {
        Row {
            cells: BTreeMap::new(),
        }
    }

    /// Set a column's cell value.
    pub fn set(&mut self, col: &str, val: impl Into<String>) {
        // Canonical SPARSE form: a spreadsheet grid cannot distinguish an empty
        // cell from an absent one, so neither may `Row` — an empty value removes
        // the key. Without this, a serializer that sets `""` (e.g. a combined
        // migration accrual key's blank sale/lot ids) produces a Row that can
        // never compare equal to its own cells→sheet→cells round-trip
        // (`from_cells` skips empties), failing write-verify on a correct row.
        // (STORE-GUARD-002)
        let val: String = val.into();
        if val.is_empty() {
            self.cells.remove(col);
        } else {
            self.cells.insert(col.to_string(), val);
        }
    }

    /// Read a column's cell value (empty string when unpopulated).
    pub fn get(&self, col: &str) -> &str {
        self.cells.get(col).map(|s| s.as_str()).unwrap_or("")
    }

    /// Project this row onto a tab's header order as a cell vector (the order a
    /// Sheets append/read uses). Columns absent from `cells` become empty.
    pub fn to_cells(&self, tab: Tab) -> Vec<String> {
        tab.header()
            .iter()
            .map(|c| self.get(c).to_string())
            .collect()
    }

    /// Build a row from a tab's header order and a parallel cell vector.
    pub fn from_cells(tab: Tab, cells: &[String]) -> Row {
        let mut row = Row::new();
        for (col, val) in tab.header().iter().zip(cells.iter()) {
            if !val.is_empty() {
                row.set(col, val.clone());
            }
        }
        row
    }
}

// ===========================================================================
// EventId (store-design.md → "EventId assignment"; STORE-SCHEMA-003). Globally
// unique across both tabs, fixed at creation, invariant across retries.
// ===========================================================================

/// The opaque, globally-unique, retry-stable event identifier. `store` assigns
/// it for a **live** append (the entry path); `import` supplies a deterministic
/// one in the same id-space. Mirrors `ledger_core::EventId` (a `String`).
pub type EventId = ledger_core::EventId;

/// Assign a globally-unique, retry-stable `EventId` for a **live** append.
/// `store` is the assigner on the entry path (STORE-SCHEMA-003); the id is
/// invariant across retries of the *same* event because the caller assigns once
/// at event creation and reuses it. `existing` is every id already present
/// across BOTH tabs (global uniqueness). `nonce` makes the assignment
/// deterministic while still globally fresh: a content-addressed prefix plus the
/// nonce, bumped until it does not collide with any existing id (across tabs).
pub fn assign_event_id(existing: &std::collections::BTreeSet<EventId>, nonce: u64) -> EventId {
    let mut bump = 0u64;
    loop {
        let candidate = format!("evt-{:016x}-{:x}", nonce, bump);
        if !existing.contains(&candidate) {
            return candidate;
        }
        bump += 1;
    }
}

// ===========================================================================
// Error model (store-design.md → "Append & Read-Back-Verify Write Path" /
// "Replay Loading & Integrity"). One variant per failure the design names.
// ===========================================================================

/// A failure crossing the trust seam: a write that could not be durably
/// confirmed, an integrity defect on load, or a serde defect.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StoreError {
    /// A read-back or idempotency check found a row that exists but does NOT
    /// deserialize equal to the event written. Existence ≠ correctness on a
    /// human-editable tab. (STORE-WRITE-003/004)
    WriteVerifyMismatch,
    /// The workbook was unreachable, or a read-back found no row: return control
    /// to the owner to retry, leaving no partial state and no background queue.
    /// (STORE-WRITE-005)
    Unreachable,
    /// The tab does not exist in the workbook — a fresh-workbook cold start,
    /// classified by the Sheets client distinctly from a transport failure.
    /// Reads treat it as an empty event log (STORE-LOAD-007); the append path
    /// creates the tab and retries once (STORE-WRITE-009).
    TabMissing,
    /// A tab's `Seq` values are not dense (a gap, duplicate, or out-of-order
    /// value): refuse to load rather than fold a corrupt log (recoverable via
    /// Sheets version history). (STORE-LOAD-002)
    NonDenseSeq,
    /// A row's `Kind` is unknown, or a required field is missing/unparseable:
    /// refuse rather than silently skip. (STORE-LOAD-003)
    UnknownKind,
    /// A required field is missing or could not be parsed. (STORE-LOAD-003)
    MissingField,
    /// A Reversal's structural guarantees fail: its target `EventId` does not
    /// exist, does not have a lower `Seq`, or is itself a Reversal.
    /// (STORE-LOAD-004)
    BadReversal,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for StoreError {}

/// The two deserialized event logs handed to the kernels for replay
/// (STORE-LOAD-001). `ledger` feeds `ledger_core::replay`; `tax` feeds the tax
/// kernel.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EventLogs {
    pub ledger: Vec<LedgerEvent>,
    pub tax: Vec<TaxEvent>,
}

/// A full read's deserialized logs plus the parallel tax `EventId`s. The kernels
/// only ever see [`EventLogs`] (the tax event carries no id); the ids are kept
/// here so the tax content hash can fold each row's real `EventId` cell
/// (STORE-CACHE-003) — they never leave `store`. `tax_ids` is parallel to
/// `logs.tax` (same order, same length).
struct LoadedLogs {
    logs: EventLogs,
    tax_ids: Vec<EventId>,
}

impl LoadedLogs {
    /// The tax events zipped with their store-assigned ids, for the content hash.
    fn tax_pairs(&self) -> Vec<(TaxEvent, EventId)> {
        self.logs
            .tax
            .iter()
            .cloned()
            .zip(self.tax_ids.iter().cloned())
            .collect()
    }
}

// ===========================================================================
// The store façade (store-design.md → "Interfaces"). Wraps a `SheetsClient`, a
// `Lock`, and a `Cache`. The append primitive acquires the lock INSIDE itself.
// ===========================================================================

/// The `store` façade over a `SheetsClient` (workbook I/O), a `Lock` (advisory
/// write-lock, acquired inside the append primitive), and a `Cache` (the
/// rebuildable local mirror). Generic over all three so tests inject fakes and
/// `runtime` injects the real impls.
pub struct Store<S: SheetsClient, L: Lock, C: Cache> {
    sheets: S,
    lock: L,
    cache: C,
}

impl<S: SheetsClient, L: Lock, C: Cache> Store<S, L, C> {
    /// Construct a store over its three collaborators.
    pub fn new(sheets: S, lock: L, cache: C) -> Self {
        Store {
            sheets,
            lock,
            cache,
        }
    }

    /// Borrow the cache (tests assert on the mirror).
    pub fn cache(&self) -> &C {
        &self.cache
    }

    /// Borrow the sheets client (tests inspect the authoritative tabs).
    pub fn sheets(&self) -> &S {
        &self.sheets
    }

    /// Borrow the advisory write-lock, so a caller orchestrating a multi-append
    /// operation can HOLD the lock across the whole operation while the inner
    /// append primitives re-acquire it re-entrantly (the same machine-local lock,
    /// a no-op against the same holder). `import::commit` uses this to hold the
    /// lock across its legacy-target check plus all appends, so no foreign writer
    /// interleaves between the check and the appends (no TOCTOU).
    /// (RUNTIME-LOCK-006, IMPORT-RUN-007) The returned `LockGuard` releases on drop;
    /// the inner primitives still each acquire inside themselves (STORE-WRITE-007),
    /// so single-writer coverage is independent of whether a caller holds the lock.
    pub fn lock(&self) -> &L {
        &self.lock
    }

    /// Mutably borrow the sheets client (tests toggle reachability, etc.).
    pub fn sheets_mut(&mut self) -> &mut S {
        &mut self.sheets
    }

    /// Whether the cache is stale for a tab per the cheap currency probe: read
    /// the fingerprint cells in ONE call and compare against the cache's stored
    /// fingerprint. A missing/errored fingerprint, or a cold cache, reads as
    /// stale (a change). Catches appends, deletions, and in-place edits to
    /// covered columns without re-reading the log. (STORE-CACHE-002)
    pub fn cache_is_stale(&self, tab: Tab) -> Result<bool, StoreError> {
        let stored = match tab {
            Tab::Ledger => self.cache.ledger_fingerprint(),
            Tab::Tax => self.cache.tax_fingerprint(),
        };
        // A cold cache (never built) is stale → force a full read.
        let stored = match stored {
            Some(fp) => fp,
            None => return Ok(true),
        };
        // Read the cheap probe in one call (not the log). Missing/errored
        // fingerprint cells are treated as a change; a missing TAB reads as
        // absent cells (a fresh workbook is a cold start, STORE-LOAD-007).
        let probe = self.read_probe_cold(tab)?;
        if !probe.is_present() {
            return Ok(true);
        }
        let changed = probe.count != Some(stored.count)
            || probe.max_seq != Some(stored.max_seq)
            || probe.checksum != Some(stored.checksum);
        Ok(changed)
    }

    /// Confirm currency and, on any probe change, rebuild the cache from the
    /// workbook (the workbook wins) and return the refreshed logs; otherwise
    /// serve the current cache. (STORE-CACHE-004)
    pub fn refresh(&mut self) -> Result<EventLogs, StoreError> {
        let stale = self.cache_is_stale(Tab::Ledger)? || self.cache_is_stale(Tab::Tax)?;
        if stale || !self.cache.is_populated() {
            // A probe change (or a cold cache) rebuilds from the workbook.
            self.load()
        } else {
            self.cache.read()
        }
    }

    /// Confirm cache currency before a write (STORE-WRITE-001): if the cheap
    /// probe shows a change (or the cache is cold), rebuild from the workbook so
    /// validation and append share one consistent view. Runs BEFORE Seq
    /// assignment.
    fn confirm_currency(&mut self) -> Result<(), StoreError> {
        if !self.cache.is_populated()
            || self.cache_is_stale(Tab::Ledger)?
            || self.cache_is_stale(Tab::Tax)?
        {
            self.load()?;
        }
        Ok(())
    }

    /// Read a tab's rows treating a missing tab as an EMPTY event log — the
    /// fresh-workbook cold start, classified by the client distinctly from a
    /// transport failure (which still propagates as `Unreachable`).
    ///
    /// @spec STORE-LOAD-007
    fn read_rows_cold(&self, tab: Tab) -> Result<Vec<Row>, StoreError> {
        match self.sheets.read_rows(tab) {
            Err(StoreError::TabMissing) => Ok(Vec::new()),
            r => r,
        }
    }

    /// Read a tab's probe treating a missing tab as absent fingerprint cells
    /// (all `None` ⇒ the probe reports a change), the same cold-start posture as
    /// [`Store::read_rows_cold`]. (STORE-LOAD-007 / STORE-CACHE-002)
    fn read_probe_cold(&self, tab: Tab) -> Result<sheets::ProbeCells, StoreError> {
        match self.sheets.read_probe(tab) {
            Err(StoreError::TabMissing) => Ok(sheets::ProbeCells::default()),
            r => r,
        }
    }

    /// Read the live workbook's current max `Seq` for a tab (never the cache).
    /// `0` for an empty tab — or a tab not created yet — so the first assigned
    /// Seq is `1`. (STORE-WRITE-002)
    fn live_max_seq(&self, tab: Tab) -> Result<u64, StoreError> {
        let rows = self.read_rows_cold(tab)?;
        let mut max = 0u64;
        for r in &rows {
            if let Ok(v) = r.get("Seq").trim().parse::<u64>() {
                if v > max {
                    max = v;
                }
            }
        }
        Ok(max)
    }

    /// Find a row by its `EventId` GLOBALLY across BOTH tabs, returning the tab it
    /// was found in alongside the row. The design mandates that every `EventId`
    /// lookup — idempotency, read-back, Reversal target — is global across both
    /// tabs, because `EventId` is globally unique (store-design.md → "Workbook
    /// Event-Log Schema"; STORE-SCHEMA-003). A ledger id colliding with a tax row
    /// (or vice versa) is therefore visible to the idempotency check.
    /// (STORE-WRITE-003/004)
    fn find_row_by_id_global(&self, id: &str) -> Result<Option<(Tab, Row)>, StoreError> {
        for tab in [Tab::Ledger, Tab::Tax] {
            // A missing tab holds no rows (cold start, STORE-LOAD-007).
            if let Some(row) = self
                .read_rows_cold(tab)?
                .into_iter()
                .find(|r| r.get("EventId") == id)
            {
                return Ok(Some((tab, row)));
            }
        }
        Ok(None)
    }

    /// Recompute and store the per-tab fingerprints from the live workbook
    /// probe (the cheap probe values) plus the freshly-computed content hash,
    /// then overwrite the cache mirror with `loaded.logs` (the workbook wins).
    /// The tax content hash folds in each row's real `EventId` (carried on
    /// `loaded.tax_ids`), so an out-of-band edit to a tax `EventId` cell is caught.
    /// (STORE-CACHE-003/004)
    fn rebuild_cache(&self, loaded: &LoadedLogs) -> Result<(), StoreError> {
        let ledger_probe = self.read_probe_cold(Tab::Ledger)?;
        let tax_probe = self.read_probe_cold(Tab::Tax)?;
        let ledger_fp = Fingerprint {
            count: ledger_probe.count.unwrap_or(0),
            max_seq: ledger_probe.max_seq.unwrap_or(0),
            checksum: ledger_probe.checksum.unwrap_or(0),
            content_hash: Fingerprint::content_hash_ledger(&loaded.logs.ledger),
        };
        let tax_fp = Fingerprint {
            count: tax_probe.count.unwrap_or(0),
            max_seq: tax_probe.max_seq.unwrap_or(0),
            checksum: tax_probe.checksum.unwrap_or(0),
            content_hash: Fingerprint::content_hash_tax(&loaded.tax_pairs()),
        };
        self.cache.rebuild(&loaded.logs, ledger_fp, tax_fp)
    }

    // -----------------------------------------------------------------------
    // Append & read-back-verify write path (STORE-WRITE-001..006).
    // -----------------------------------------------------------------------

    /// Append a single, kernel-validated `LedgerEvent`, then read it back and
    /// verify structural field equality before confirming. The full path:
    ///
    /// 1. Acquire the advisory write-lock (INSIDE this primitive).
    /// 2. Confirm cache currency via the cheap probe; rebuild on change
    ///    (STORE-WRITE-001).
    /// 3. Assign `Seq` from the live workbook max `+ 1` (never the cache)
    ///    (STORE-WRITE-002).
    /// 4. Idempotency: if the `EventId` exists, skip on equality / error on
    ///    difference (STORE-WRITE-003).
    /// 5. Append the row (STORE-WRITE-004).
    /// 6. Read back by `EventId`, assert field equality, then update the cache
    ///    (STORE-WRITE-004/006). A missing row / unreachable workbook returns
    ///    control to the owner (STORE-WRITE-005).
    ///
    /// @spec STORE-WRITE-007
    pub fn append_ledger(&mut self, event: &LedgerEvent) -> Result<AppendOutcome, StoreError> {
        // 1. Acquire the advisory write-lock INSIDE the primitive. Held for the
        //    whole read-max / append / read-back critical section.
        let _guard = self.lock.acquire()?;

        // 2. Confirm cache currency; rebuild on a probe change (STORE-WRITE-001).
        self.confirm_currency()?;

        // 3. Idempotency on the EventId (STORE-WRITE-003), GLOBAL across both tabs
        //    (the id is globally unique). Compare event content (ignoring the
        //    store-assigned Seq) against the stored row. A collision against the
        //    OTHER tab is a mismatch (a tax row cannot deserialize as this ledger
        //    event), so a cross-tab id collision is detected, never double-appended.
        if let Some((found_tab, stored)) = self.find_row_by_id_global(&event.id)? {
            if found_tab != Tab::Ledger {
                return Err(StoreError::WriteVerifyMismatch);
            }
            return self.ledger_idempotent_result(event, &stored);
        }

        // 4. Assign Seq from the LIVE workbook max + 1 (never the cache).
        let seq = Seq(self.live_max_seq(Tab::Ledger)? + 1);
        let stamped = with_ledger_seq(event, seq);

        // 5. Append the row, then 6. read back & verify field equality, update
        //    the cache only on success (STORE-WRITE-004/006).
        let row = serde_rows::ledger_to_row(&stamped);
        self.append_row_bootstrapping(Tab::Ledger, &row)?;
        self.read_back_verify(Tab::Ledger, &stamped.id, &row)?;
        let logs = self.load_logs()?;
        self.rebuild_cache(&logs)?;

        Ok(AppendOutcome {
            event_id: stamped.id,
            seq,
            idempotent_skip: false,
        })
    }

    /// Append a single, kernel-validated `TaxEvent`; same path as
    /// [`Store::append_ledger`] against the `Tax Events` tab. The `TaxEvent` has
    /// no id field, so `store` assigns a globally-unique, retry-stable `EventId`
    /// derived from the event's content (so a retry of the same event reuses the
    /// same id and is idempotent). (STORE-WRITE-001..006, STORE-SCHEMA-003)
    pub fn append_tax(&mut self, event: &TaxEvent) -> Result<AppendOutcome, StoreError> {
        let _guard = self.lock.acquire()?;
        self.confirm_currency()?;

        // A tax event's store-assigned id is deterministic in its content, so a
        // retry produces the same id (retry-stable idempotency on a kind that
        // carries no id field). (STORE-SCHEMA-003)
        let event_id = tax_event_id(event);

        // Idempotency GLOBAL across both tabs (the id is globally unique). A
        // collision against the LEDGER tab is a mismatch (a ledger row cannot
        // deserialize as this tax event), so a cross-tab id collision is detected.
        if let Some((found_tab, stored)) = self.find_row_by_id_global(&event_id)? {
            if found_tab != Tab::Tax {
                return Err(StoreError::WriteVerifyMismatch);
            }
            return self.tax_idempotent_result(event, &event_id, &stored);
        }

        let seq = Seq(self.live_max_seq(Tab::Tax)? + 1);
        let stamped = TaxEvent {
            seq,
            kind: event.kind.clone(),
        };
        let row = serde_rows::tax_to_row(&stamped, &event_id);
        self.append_row_bootstrapping(Tab::Tax, &row)?;
        self.read_back_verify(Tab::Tax, &event_id, &row)?;
        let logs = self.load_logs()?;
        self.rebuild_cache(&logs)?;

        Ok(AppendOutcome {
            event_id,
            seq,
            idempotent_skip: false,
        })
    }

    /// Append a contiguous BLOCK of `LedgerEvent`s (e.g. a Vest plus its
    /// same-day sell-to-cover) atomically with respect to `Seq`: a single
    /// `base = liveMax + 1` is allocated up front and the block is assigned
    /// `base, base+1, …` locally, then appended strictly in `Seq` order — Seqs
    /// are not re-derived per row (STORE-WRITE-002). Each row is then
    /// read-back-verified. A partial landing leaves a valid dense prefix; the
    /// unlanded events re-append by idempotency on retry (STORE-WRITE-006).
    pub fn append_ledger_block(
        &mut self,
        events: &[LedgerEvent],
    ) -> Result<Vec<AppendOutcome>, StoreError> {
        let _guard = self.lock.acquire()?;
        self.confirm_currency()?;

        // Allocate the contiguous Seq block up front from the live max. `next`
        // is the Seq counter that advances ONLY when a row is actually appended:
        // an idempotently-skipped event (a partial prior landing) does NOT consume
        // a Seq, so the surviving suffix stays dense against the already-landed
        // prefix instead of being offset by the skipped count. (STORE-WRITE-002/006)
        let mut next = self.live_max_seq(Tab::Ledger)? + 1;
        let mut outs = Vec::with_capacity(events.len());

        for event in events {
            // Idempotency per event (a partial prior landing re-appends only the
            // unlanded suffix), GLOBAL across both tabs. A skip leaves `next`
            // untouched. (STORE-WRITE-006) A cross-tab id collision is a mismatch.
            if let Some((found_tab, stored)) = self.find_row_by_id_global(&event.id)? {
                if found_tab != Tab::Ledger {
                    return Err(StoreError::WriteVerifyMismatch);
                }
                outs.push(self.ledger_idempotent_result(event, &stored)?);
                continue;
            }

            let seq = Seq(next);
            let stamped = with_ledger_seq(event, seq);
            let row = serde_rows::ledger_to_row(&stamped);
            self.append_row_bootstrapping(Tab::Ledger, &row)?;
            self.read_back_verify(Tab::Ledger, &stamped.id, &row)?;
            outs.push(AppendOutcome {
                event_id: stamped.id,
                seq,
                idempotent_skip: false,
            });
            next += 1;
        }

        // Update the cache only after the block's writes are all verified
        // (STORE-WRITE-006).
        let logs = self.load_logs()?;
        self.rebuild_cache(&logs)?;
        Ok(outs)
    }

    /// Append a row, bootstrapping a missing tab: on the missing-tab signal a
    /// fresh workbook raises, have the client create the tab (frozen header +
    /// fingerprint block) and retry the append ONCE, so the first-ever write
    /// lands without a separate init step.
    ///
    /// @spec STORE-WRITE-009
    fn append_row_bootstrapping(&mut self, tab: Tab, row: &Row) -> Result<(), StoreError> {
        match self.sheets.append_row(tab, row) {
            Err(StoreError::TabMissing) => {
                self.sheets.ensure_tab(tab)?;
                self.sheets.append_row(tab, row)
            }
            r => r,
        }
    }

    /// The idempotency decision for a ledger event whose `EventId` already
    /// exists: skip on content equality (ignoring the store-assigned Seq), error
    /// on difference (STORE-WRITE-003).
    fn ledger_idempotent_result(
        &self,
        event: &LedgerEvent,
        stored: &Row,
    ) -> Result<AppendOutcome, StoreError> {
        let stored_seq = serde_rows::row_to_ledger(stored)?.seq;
        let mut candidate = serde_rows::ledger_to_row(&with_ledger_seq(event, stored_seq));
        let mut stored_norm = stored.clone();
        // Compare ignoring the store-assigned Seq column (it is store metadata).
        candidate.cells.remove("Seq");
        stored_norm.cells.remove("Seq");
        if candidate == stored_norm {
            Ok(AppendOutcome {
                event_id: event.id.clone(),
                seq: stored_seq,
                idempotent_skip: true,
            })
        } else {
            Err(StoreError::WriteVerifyMismatch)
        }
    }

    /// The idempotency decision for a tax event whose store-assigned `EventId`
    /// already exists. (STORE-WRITE-003)
    fn tax_idempotent_result(
        &self,
        event: &TaxEvent,
        event_id: &str,
        stored: &Row,
    ) -> Result<AppendOutcome, StoreError> {
        // Deserialize once (rejects a malformed stored row, and yields its Seq).
        let stored_seq = serde_rows::row_to_tax(stored)?.0.seq;
        let stamped = TaxEvent {
            seq: stored_seq,
            kind: event.kind.clone(),
        };
        let mut candidate = serde_rows::tax_to_row(&stamped, &event_id.to_string());
        let mut stored_norm = stored.clone();
        candidate.cells.remove("Seq");
        stored_norm.cells.remove("Seq");
        if candidate == stored_norm {
            Ok(AppendOutcome {
                event_id: event_id.to_string(),
                seq: stored_seq,
                idempotent_skip: true,
            })
        } else {
            Err(StoreError::WriteVerifyMismatch)
        }
    }

    /// Read the appended row back by `EventId` (GLOBAL lookup, since the id is
    /// globally unique) and assert structural equality with the row written; a
    /// field mismatch — or a row that landed in the wrong tab — is
    /// `WriteVerifyMismatch`, a missing row is `Unreachable` (return control to
    /// the owner). (STORE-WRITE-004/005)
    fn read_back_verify(&self, tab: Tab, id: &str, written: &Row) -> Result<(), StoreError> {
        match self.find_row_by_id_global(id)? {
            Some((found_tab, back)) if found_tab == tab && &back == written => Ok(()),
            Some(_) => Err(StoreError::WriteVerifyMismatch),
            None => Err(StoreError::Unreachable),
        }
    }

    // -----------------------------------------------------------------------
    // Replay loading & integrity (STORE-LOAD-001..005).
    // -----------------------------------------------------------------------

    /// Full read of both event-log tabs: deserialize to `Vec<LedgerEvent>` /
    /// `Vec<TaxEvent>`, run the integrity check (dense Seq, known Kind,
    /// structural Reversal checks), compute the content hash, rebuild the cache,
    /// and return the logs for the kernels (STORE-LOAD-001..004, STORE-CACHE-003).
    pub fn load(&mut self) -> Result<EventLogs, StoreError> {
        let loaded = self.load_logs()?;
        // Rebuild the cache from this full read (the workbook wins) and store the
        // content hash + the cheap probe values. (STORE-CACHE-003/004)
        self.rebuild_cache(&loaded)?;
        Ok(loaded.logs)
    }

    /// Recompute the content hash over a fresh full read and compare it against
    /// the stored per-tab `Fingerprint::content_hash` — the **on-demand integrity
    /// verification** the design names (startup / pre-report). This is the
    /// belt-and-suspenders catch for an out-of-band edit to a field OUTSIDE the
    /// cheap checksum's reach, which the hot-path probe (STORE-CACHE-002) would
    /// miss. On a mismatch (the recomputed hash differs from the stored one) the
    /// cache is rebuilt from the workbook — the workbook wins — and `true` is
    /// returned (drift was found and healed); `false` means the content hash
    /// agreed. A cold cache forces a rebuild. (STORE-CACHE-003/004)
    ///
    /// @spec STORE-LOAD-006
    pub fn verify_integrity(&mut self) -> Result<bool, StoreError> {
        let loaded = self.load_logs()?;
        let ledger_now = Fingerprint::content_hash_ledger(&loaded.logs.ledger);
        let tax_now = Fingerprint::content_hash_tax(&loaded.tax_pairs());

        let stored_matches = match (
            self.cache.ledger_fingerprint(),
            self.cache.tax_fingerprint(),
        ) {
            (Some(l), Some(t)) => l.content_hash == ledger_now && t.content_hash == tax_now,
            _ => false, // a cold/partial cache never "matches" — force a rebuild.
        };

        if stored_matches {
            Ok(false)
        } else {
            // Drift outside the cheap checksum's reach (or a cold cache): rebuild
            // from the workbook. (STORE-CACHE-004)
            self.rebuild_cache(&loaded)?;
            Ok(true)
        }
    }

    /// Read + deserialize + integrity-check both tabs WITHOUT touching the cache
    /// (the shared body of `load` and the post-append cache refresh). Folds in
    /// `Seq` order — visual row position is irrelevant. Also carries the parallel
    /// tax `EventId`s so the tax content hash can fold the real id.
    /// (STORE-LOAD-001..004)
    fn load_logs(&self) -> Result<LoadedLogs, StoreError> {
        // Ledger tab: deserialize every row (unknown Kind / missing field is a
        // hard error — STORE-LOAD-003), then check dense Seq + Reversal structure.
        // A missing tab is a fresh-workbook cold start: an empty log
        // (STORE-LOAD-007).
        let mut ledger: Vec<LedgerEvent> = Vec::new();
        for row in self.read_rows_cold(Tab::Ledger)? {
            ledger.push(serde_rows::row_to_ledger(&row)?);
        }
        check_ledger_integrity(&ledger)?;
        ledger.sort_by_key(|e| e.seq.0);

        // Tax tab: deserialize every row; cross-tab references are NOT gated here
        // (STORE-LOAD-005 — the tax kernel's orphan rule handles them). Keep the
        // store-assigned EventId alongside each event so the content hash covers
        // the EventId cell (STORE-CACHE-003).
        let mut tax: Vec<TaxEvent> = Vec::new();
        let mut tax_ids: Vec<EventId> = Vec::new();
        for row in self.read_rows_cold(Tab::Tax)? {
            let (event, id) = serde_rows::row_to_tax(&row)?;
            tax.push(event);
            tax_ids.push(id);
        }
        check_tax_integrity(&tax)?;
        // Sort the (event, id) pairs together by Seq so they stay parallel.
        let mut paired: Vec<(TaxEvent, EventId)> = tax.into_iter().zip(tax_ids).collect();
        paired.sort_by_key(|(e, _)| e.seq.0);
        let (tax, tax_ids): (Vec<TaxEvent>, Vec<EventId>) = paired.into_iter().unzip();

        Ok(LoadedLogs {
            logs: EventLogs { ledger, tax },
            tax_ids,
        })
    }

    /// Serve event-log reads from the cache (offline path), without touching the
    /// workbook (STORE-CACHE-005).
    pub fn read_cached(&self) -> Result<EventLogs, StoreError> {
        self.cache.read()
    }
}

/// Clone a `LedgerEvent` with a new store-assigned `Seq` (the kind/id/date are
/// the caller's; the Seq is store metadata). (STORE-WRITE-002)
fn with_ledger_seq(event: &LedgerEvent, seq: Seq) -> LedgerEvent {
    LedgerEvent {
        id: event.id.clone(),
        seq,
        date: event.date,
        kind: event.kind.clone(),
    }
}

/// Derive a tax event's store-assigned, retry-stable `EventId` from its content
/// (the `TaxEvent` type carries no id). A retry of the same event yields the same
/// id, so the idempotency lookup matches and never double-appends.
/// (STORE-SCHEMA-003) The Seq is excluded — it is store-assigned, not content.
///
/// The hash MUST be **stable across toolchains and platforms**: the id is
/// persisted into the `EventId` column and is the sole idempotency key for tax
/// events, so a retry after a Rust upgrade must compute the SAME id. We therefore
/// use a fixed FNV-1a over the canonical cell projection — NOT `DefaultHasher`,
/// whose output is explicitly not guaranteed stable across releases. A golden
/// test pins a fixed event's id so any future change to the derivation fails loud.
///
/// Public so a caller appending tax events (e.g. `import`'s migration lifecycle)
/// can compute the SAME store-assigned id up front — to gate an own-events-only
/// target against the exact set of ids this run will write, rather than a loose
/// `tax-` prefix test.
///
/// @spec STORE-SCHEMA-004, STORE-WRITE-008
pub fn tax_event_id(event: &TaxEvent) -> EventId {
    // Canonical content projection: the kind/field cells, id- and seq-independent.
    let row = serde_rows::tax_to_row(
        &TaxEvent {
            seq: Seq(0),
            kind: event.kind.clone(),
        },
        &String::new(),
    );
    let mut cells = row.to_cells(Tab::Tax);
    // Drop the Seq + EventId cells so the id is purely content-addressed.
    cells[0] = String::new(); // Seq column (header order: Seq is index 0)
    cells[1] = String::new(); // EventId column (index 1)
                              // A single unit-separator-joined byte stream, then a fixed FNV-1a.
    let mut bytes: Vec<u8> = Vec::new();
    for (i, c) in cells.iter().enumerate() {
        if i > 0 {
            bytes.push(0x1f); // ASCII unit separator: an unambiguous cell delimiter.
        }
        bytes.extend_from_slice(c.as_bytes());
    }
    format!("tax-{:016x}", fnv1a_64(&bytes))
}

/// FNV-1a 64-bit — a fixed, documented, platform-stable hash. Used for the
/// persisted tax `EventId` so retry-idempotency survives a toolchain change
/// (STORE-SCHEMA-003). NOT a cryptographic hash; it only needs determinism and a
/// low collision rate over distinct tax-event content.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// What an append did: either it landed (was appended and read-back-verified) or
/// it was a retry-stable idempotent no-op (the `EventId` already existed and the
/// stored row was equal). Both carry the assigned `Seq` / `EventId`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AppendOutcome {
    pub event_id: EventId,
    pub seq: Seq,
    /// `true` when the row was already present and equal (idempotent skip);
    /// `false` when it was freshly appended. (STORE-WRITE-003/006)
    pub idempotent_skip: bool,
}

// ===========================================================================
// Integrity check (store-design.md → "Replay Loading & Integrity";
// STORE-LOAD-002/003/004). Pure over already-deserialized events; the load path
// calls it before handing logs to the kernels. Exposed for direct testing.
// ===========================================================================

/// Verify a ledger log's structural integrity before replay: dense `Seq` per tab
/// (STORE-LOAD-002), and for each Reversal its target exists, has a lower `Seq`,
/// and is not itself a Reversal (STORE-LOAD-004). `Kind`-knownness and
/// required-field presence are enforced during deserialization (STORE-LOAD-003).
/// Cross-tab references are NOT checked here (STORE-LOAD-005).
pub fn check_ledger_integrity(events: &[LedgerEvent]) -> Result<(), StoreError> {
    // Dense, contiguous per-tab Seq starting at 1 (STORE-LOAD-002).
    check_dense_seq(events.iter().map(|e| e.seq))?;

    // Structural Reversal checks (STORE-LOAD-004): the target exists, has a lower
    // Seq, and is not itself a Reversal. Build the id → (seq, is_reversal) map.
    use std::collections::BTreeMap;
    let mut by_id: BTreeMap<&str, (u64, bool)> = BTreeMap::new();
    for e in events {
        let is_rev = matches!(e.kind, ledger_core::LedgerEventKind::Reversal { .. });
        by_id.insert(e.id.as_str(), (e.seq.0, is_rev));
    }
    for e in events {
        if let ledger_core::LedgerEventKind::Reversal { target_event_id } = &e.kind {
            match by_id.get(target_event_id.as_str()) {
                // Target must exist, be strictly lower in Seq, and not be a Reversal.
                Some(&(target_seq, target_is_rev)) => {
                    if target_seq >= e.seq.0 || target_is_rev {
                        return Err(StoreError::BadReversal);
                    }
                }
                None => return Err(StoreError::BadReversal),
            }
        }
    }
    Ok(())
}

/// Verify a tax log's structural integrity before replay: dense `Seq`
/// (STORE-LOAD-002). Tax events carry no Reversal and no cross-tab gate
/// (STORE-LOAD-005).
pub fn check_tax_integrity(events: &[TaxEvent]) -> Result<(), StoreError> {
    check_dense_seq(events.iter().map(|e| e.seq))
}

/// The shared dense-Seq check: values are unique, start at 1, and have no gap
/// (a gap, duplicate, or out-of-order value is a `NonDenseSeq` integrity error).
/// (STORE-LOAD-002) An empty log is trivially dense.
fn check_dense_seq(seqs: impl Iterator<Item = Seq>) -> Result<(), StoreError> {
    let mut values: Vec<u64> = seqs.map(|s| s.0).collect();
    if values.is_empty() {
        return Ok(());
    }
    values.sort_unstable();
    // After sorting, the i-th (0-based) value must be exactly i+1 — that single
    // identity catches gaps, duplicates, an off-by-one start, and the wrong count
    // all at once. (Out-of-order visual position is irrelevant; Seq is the order.)
    for (i, v) in values.iter().enumerate() {
        if *v != (i as u64) + 1 {
            return Err(StoreError::NonDenseSeq);
        }
    }
    Ok(())
}

// ===========================================================================
// Re-exports of the kernel event types this seam serializes (so tests and
// `runtime` import them through `store`). The discriminator-driven exhaustive
// match in `serde_rows` is over exactly these kinds.
// ===========================================================================
pub use ledger_core::{LedgerEvent as LedgerEventTy, LedgerEventKind as LedgerKindTy};
pub use tax::{TaxEvent as TaxEventTy, TaxEventKind as TaxKindTy};
