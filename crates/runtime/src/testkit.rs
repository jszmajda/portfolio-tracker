//! In-memory fakes for ALL unit tests (no network): a [`FakeSheetsApi`] modeling
//! the low-level cell-grid Sheets API, plus convenience re-exports.
//!
//! The lock + cycle are unit-tested against these fakes (and `sheets-view`'s /
//! `store`'s own in-memory fakes); the real [`crate::sheets::GoogleSheetsApi`] and
//! the JWT auth are exercised by the finalize e2e against a real workbook.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::sheets::{Grid, SheetsApi, SheetsError};

/// An in-memory low-level Sheets API modeling per-**sheet** grids, so the
/// [`crate::sheets::StoreSheetsAdapter`] (and any other adapter) can be driven with
/// no network. It is geometry-aware enough that a read of `'Tab'!A1:L`, an append
/// to `'Tab'!A2:L`, and an update of `'Tab'!A2:L` all compose on the SAME sheet
/// (row 1 = header, data from row 2) — faithfully modeling the real Sheets
/// `values.get` / `:append` / `update` behavior. Plus a failure injection so a
/// test can watch the adapter map a transport failure to its segment error.
/// (RUNTIME-SHEETS-001/002)
pub struct FakeSheetsApi {
    /// Sheet name (the part before `!`) -> the full grid (row 0 = header).
    sheets: RefCell<BTreeMap<String, Grid>>,
    /// When `true`, every call returns `Unreachable`, modeling an offline workbook
    /// so a test can watch the adapter return control to the owner.
    unreachable: RefCell<bool>,
    /// Sheets marked as NOT EXISTING (a fresh workbook): any values call against
    /// one returns the real API's 400 `Unable to parse range` error, until
    /// [`SheetsApi::ensure_sheet`] creates it. Distinct from an absent map entry,
    /// which models an existing-but-empty sheet (the fake's historical default).
    missing: RefCell<std::collections::BTreeSet<String>>,
    /// Per-method call counts, so a test can assert the adapter went through the
    /// ONE low-level primitive (RUNTIME-SHEETS-002).
    reads: RefCell<u32>,
    appends: RefCell<u32>,
    updates: RefCell<u32>,
    clears: RefCell<u32>,
}

impl Default for FakeSheetsApi {
    fn default() -> Self {
        FakeSheetsApi::new()
    }
}

/// Parse a range like `'Ledger Events'!A2:L` into `(sheet_name, start_row_0based)`.
/// A range with no `!` is treated as a whole-sheet reference starting at row 0.
/// The start row is the digits after the first column letters in the A1 anchor.
fn parse_range(range: &str) -> (String, usize) {
    let (sheet_raw, a1) = match range.split_once('!') {
        Some((s, a)) => (s, a),
        None => (range, ""),
    };
    // Strip surrounding single quotes from the sheet name.
    let sheet = sheet_raw.trim_matches('\'').to_string();
    // The anchor is before any ':'. Its trailing digits are the 1-based start row.
    let anchor = a1.split(':').next().unwrap_or("");
    let digits: String = anchor.chars().filter(|c| c.is_ascii_digit()).collect();
    let start_1based = digits.parse::<usize>().unwrap_or(1).max(1);
    (sheet, start_1based - 1)
}

impl FakeSheetsApi {
    /// A fresh, empty fake (no sheets populated).
    pub fn new() -> Self {
        FakeSheetsApi {
            sheets: RefCell::new(BTreeMap::new()),
            unreachable: RefCell::new(false),
            missing: RefCell::new(std::collections::BTreeSet::new()),
            reads: RefCell::new(0),
            appends: RefCell::new(0),
            updates: RefCell::new(0),
            clears: RefCell::new(0),
        }
    }

    /// Mark a sheet as NOT EXISTING (a fresh workbook that has never been
    /// written): values calls against it 400 the way the real API does, until
    /// `ensure_sheet` creates it. (STORE-LOAD-007 / STORE-WRITE-009)
    pub fn set_sheet_missing(&self, title: &str) {
        self.missing.borrow_mut().insert(title.to_string());
    }

    /// Whether a sheet is currently marked missing (assert the bootstrap ran).
    pub fn sheet_missing(&self, title: &str) -> bool {
        self.missing.borrow().contains(title)
    }

    /// The real API's missing-tab failure for a values call: a non-retryable 400
    /// whose message carries `Unable to parse range`.
    fn missing_err(range: &str) -> SheetsError {
        SheetsError::Api {
            status: 400,
            message: format!("Unable to parse range: {range}"),
        }
    }

    /// 400 if the range's sheet is marked missing.
    fn gate_missing(&self, range: &str) -> Result<(), SheetsError> {
        let (sheet, _) = parse_range(range);
        if self.missing.borrow().contains(&sheet) {
            return Err(Self::missing_err(range));
        }
        Ok(())
    }

    /// Seed a range with rows (test setup — bypasses the write path). The rows are
    /// placed on the range's sheet starting at the range's start row.
    pub fn seed(&self, range: &str, rows: Grid) {
        let (sheet, start) = parse_range(range);
        let mut sheets = self.sheets.borrow_mut();
        let grid = sheets.entry(sheet).or_default();
        for (i, row) in rows.into_iter().enumerate() {
            let idx = start + i;
            while grid.len() <= idx {
                grid.push(Vec::new());
            }
            grid[idx] = row;
        }
    }

    /// The full grid of a sheet (test assertions). Accepts a range or bare sheet
    /// name.
    pub fn rows_at(&self, range: &str) -> Grid {
        let (sheet, _) = parse_range(range);
        self.sheets.borrow().get(&sheet).cloned().unwrap_or_default()
    }

    /// Mark the workbook unreachable (every call returns `Unreachable`).
    pub fn set_unreachable(&self, v: bool) {
        *self.unreachable.borrow_mut() = v;
    }

    /// How many low-level reads went through the primitive. (RUNTIME-SHEETS-002)
    pub fn read_count(&self) -> u32 {
        *self.reads.borrow()
    }

    /// How many low-level appends went through the primitive. (RUNTIME-SHEETS-002)
    pub fn append_count(&self) -> u32 {
        *self.appends.borrow()
    }

    /// How many low-level updates went through the primitive. (RUNTIME-SHEETS-002)
    pub fn update_count(&self) -> u32 {
        *self.updates.borrow()
    }

    /// How many low-level clears went through the primitive. (RUNTIME-SHEETS-002)
    pub fn clear_count(&self) -> u32 {
        *self.clears.borrow()
    }

    fn guard(&self) -> Result<(), SheetsError> {
        if *self.unreachable.borrow() {
            Err(SheetsError::Unreachable("fake offline".to_string()))
        } else {
            Ok(())
        }
    }
}

impl SheetsApi for FakeSheetsApi {
    fn read_range(&self, range: &str) -> Result<Grid, SheetsError> {
        self.guard()?;
        self.gate_missing(range)?;
        *self.reads.borrow_mut() += 1;
        let (sheet, start) = parse_range(range);
        let sheets = self.sheets.borrow();
        let grid = sheets.get(&sheet).cloned().unwrap_or_default();
        // Return rows from the range's start row down (the real values.get returns
        // the requested window; the adapter reads A1:.. so start = 0).
        Ok(grid.into_iter().skip(start).collect())
    }

    fn append_rows(&self, range: &str, rows: &Grid) -> Result<(), SheetsError> {
        self.guard()?;
        self.gate_missing(range)?;
        *self.appends.borrow_mut() += 1;
        let (sheet, _) = parse_range(range);
        // Append to the END of the sheet's table (the real :append finds the table
        // and inserts beneath it), regardless of the A2 anchor.
        self.sheets
            .borrow_mut()
            .entry(sheet)
            .or_default()
            .extend(rows.iter().cloned());
        Ok(())
    }

    fn update_range(&self, range: &str, rows: &Grid) -> Result<(), SheetsError> {
        self.guard()?;
        self.gate_missing(range)?;
        *self.updates.borrow_mut() += 1;
        let (sheet, start) = parse_range(range);
        let mut sheets = self.sheets.borrow_mut();
        let grid = sheets.entry(sheet).or_default();
        // Overwrite from the start row, and TRUNCATE the residual tail to the new
        // row count (the full-tab batchUpdate + tail-truncate discipline).
        grid.truncate(start);
        for row in rows {
            grid.push(row.clone());
        }
        Ok(())
    }

    fn clear_range(&self, range: &str) -> Result<(), SheetsError> {
        self.guard()?;
        self.gate_missing(range)?;
        *self.clears.borrow_mut() += 1;
        let (sheet, start) = parse_range(range);
        let mut sheets = self.sheets.borrow_mut();
        if let Some(grid) = sheets.get_mut(&sheet) {
            grid.truncate(start); // clear the data rows from the start row down
        }
        Ok(())
    }

    fn ensure_sheet(&self, title: &str) -> Result<(), SheetsError> {
        self.guard()?;
        // Creating the sheet clears its missing marker; a no-op when it exists
        // (the real addSheet-if-absent). (STORE-WRITE-009)
        self.missing.borrow_mut().remove(title);
        self.sheets.borrow_mut().entry(title.to_string()).or_default();
        Ok(())
    }

    fn append_rows_raw(&self, range: &str, rows: &Grid) -> Result<(), SheetsError> {
        // The fake stores cell strings verbatim either way (it models no
        // USER_ENTERED coercion); RAW rides the same append path + counter.
        self.append_rows(range, rows)
    }
}
