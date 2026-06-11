//! The trust boundary: `row → event` and `event → row`, **total and
//! structural** — a wildcard-free exhaustive match over every `Kind`, copying
//! typed fields, with **no arithmetic** (store-design.md → "Serialization").
//! This is the project's one unverified seam, locked by the drift-guard test
//! (STORE-GUARD-001/002). Everything here is plain serde/string conversion,
//! OUTSIDE the `verus!{}` boundary (STORE-GUARD-003).
//!
//! @spec STORE-GUARD-003
//!
//! `store` assigns the `EventId` for a live append; both tabs carry it as a
//! universal column even though `tax::TaxEvent` has no `id` field — so a tax row
//! round-trips as `(event, EventId)` (the id is the idempotency / Reversal key).
//! Ledger events carry their own `id`, so a ledger row round-trips the event
//! directly.

use config::{Jurisdiction, TaxYear};
use ledger_core::{LedgerEvent, LedgerEventKind, LotRef};
use pt_core::{Cents, Date, MicroShares, Seq};
use tax::{AccrualKey, Quarter, TaxEvent, TaxEventKind};

use crate::{EventId, Row, StoreError};

// ===========================================================================
// Universal + per-family column names (store-design.md → "Workbook Event-Log
// Schema"). Sparse: only the columns a `Kind` uses are populated.
// ===========================================================================

// Universal columns, on BOTH tabs.
const COL_SEQ: &str = "Seq";
const COL_EVENT_ID: &str = "EventId";
const COL_DATE: &str = "Date";
const COL_KIND: &str = "Kind";

// Ledger field columns (the union over Buy/Vest/Sell/Split/Reversal).
const COL_LOT_ID: &str = "LotId";
const COL_SALE_ID: &str = "SaleId";
const COL_SYMBOL: &str = "Symbol";
const COL_QTY: &str = "Qty";
const COL_UNIT_PRICE: &str = "UnitPriceCents";
const COL_FEES: &str = "FeesCents";
const COL_FMV: &str = "FmvPerShareCents";
const COL_LOT_REFS: &str = "LotRefs";
const COL_ACCRUES_TO_STATE: &str = "AccruesToState";
const COL_PLATFORM: &str = "Platform";
const COL_TRACKING_CODE: &str = "TrackingCode";
const COL_RATIO_NUM: &str = "RatioNum";
const COL_RATIO_DEN: &str = "RatioDen";
const COL_TARGET_EVENT_ID: &str = "TargetEventId";

// Tax field columns (the union over Allocate/Move/Pay/AmountOverride/SeedMigration).
const COL_ACCRUAL_SALE_ID: &str = "AccrualSaleId";
const COL_ACCRUAL_LOT_ID: &str = "AccrualLotId";
const COL_JURISDICTION: &str = "Jurisdiction";
const COL_TAX_YEAR: &str = "TaxYear";
const COL_ACCOUNT_LABEL: &str = "AccountLabel";
const COL_AMOUNT: &str = "AmountCents";
const COL_APPLIED_AMOUNT: &str = "AppliedAmountCents";
const COL_PERIOD: &str = "Period";
const COL_COVERS: &str = "Covers";
const COL_REASON: &str = "Reason";

/// The ordered typed-column header for the `Ledger Events` tab.
pub const LEDGER_HEADER: &[&str] = &[
    COL_SEQ,
    COL_EVENT_ID,
    COL_DATE,
    COL_KIND,
    COL_LOT_ID,
    COL_SALE_ID,
    COL_SYMBOL,
    COL_QTY,
    COL_UNIT_PRICE,
    COL_FEES,
    COL_FMV,
    COL_LOT_REFS,
    COL_ACCRUES_TO_STATE,
    COL_PLATFORM,
    COL_TRACKING_CODE,
    COL_RATIO_NUM,
    COL_RATIO_DEN,
    COL_TARGET_EVENT_ID,
];

/// The ordered typed-column header for the `Tax Events` tab.
pub const TAX_HEADER: &[&str] = &[
    COL_SEQ,
    COL_EVENT_ID,
    COL_DATE,
    COL_KIND,
    COL_ACCRUAL_SALE_ID,
    COL_ACCRUAL_LOT_ID,
    COL_JURISDICTION,
    COL_TAX_YEAR,
    COL_ACCOUNT_LABEL,
    COL_AMOUNT,
    COL_APPLIED_AMOUNT,
    COL_PERIOD,
    COL_COVERS,
    COL_REASON,
];

// ===========================================================================
// Ledger row <-> event (STORE-GUARD-001/002). Exhaustive, wildcard-free.
// ===========================================================================

/// `LedgerEvent → Row`: a total, wildcard-free structural match over every
/// `LedgerEventKind`, copying typed fields, with no arithmetic. The ledger event
/// carries its own `id`/`seq`/`date`. (STORE-GUARD-001)
pub fn ledger_to_row(event: &LedgerEvent) -> Row {
    let mut row = Row::new();
    // Universal columns, on every ledger row.
    row.set(COL_SEQ, enc_seq(event.seq));
    row.set(COL_EVENT_ID, event.id.clone());
    row.set(COL_DATE, enc_date(event.date));

    // Wildcard-free exhaustive match: a new LedgerEventKind variant fails to
    // compile here until its mapping is added (the drift guard). No arithmetic.
    match &event.kind {
        LedgerEventKind::Buy {
            lot_id,
            symbol,
            qty,
            unit_price_cents,
            fees_cents,
            platform,
            tracking_code,
        } => {
            row.set(COL_KIND, "Buy");
            row.set(COL_LOT_ID, lot_id.clone());
            row.set(COL_SYMBOL, symbol.clone());
            row.set(COL_QTY, enc_micro(*qty));
            row.set(COL_UNIT_PRICE, enc_cents(*unit_price_cents));
            row.set(COL_FEES, enc_cents(*fees_cents));
            row.set(COL_PLATFORM, platform.clone());
            set_opt(&mut row, COL_TRACKING_CODE, tracking_code);
        }
        LedgerEventKind::Vest {
            lot_id,
            symbol,
            qty,
            fmv_per_share_cents,
            platform,
            tracking_code,
        } => {
            row.set(COL_KIND, "Vest");
            row.set(COL_LOT_ID, lot_id.clone());
            row.set(COL_SYMBOL, symbol.clone());
            row.set(COL_QTY, enc_micro(*qty));
            row.set(COL_FMV, enc_cents(*fmv_per_share_cents));
            row.set(COL_PLATFORM, platform.clone());
            set_opt(&mut row, COL_TRACKING_CODE, tracking_code);
        }
        LedgerEventKind::Sell {
            sale_id,
            symbol,
            qty,
            unit_price_cents,
            fees_cents,
            lot_refs,
            accrues_to_state,
            platform,
            tracking_code,
        } => {
            row.set(COL_KIND, "Sell");
            row.set(COL_SALE_ID, sale_id.clone());
            row.set(COL_SYMBOL, symbol.clone());
            row.set(COL_QTY, enc_micro(*qty));
            row.set(COL_UNIT_PRICE, enc_cents(*unit_price_cents));
            row.set(COL_FEES, enc_cents(*fees_cents));
            // lot_refs encode to the empty string for the FIFO-fallback case; the
            // Kind discriminator distinguishes it from a non-Sell, so an empty
            // LotRefs cell round-trips to an empty Vec unambiguously.
            row.set(COL_LOT_REFS, enc_lot_refs(lot_refs));
            set_opt(&mut row, COL_ACCRUES_TO_STATE, accrues_to_state);
            row.set(COL_PLATFORM, platform.clone());
            set_opt(&mut row, COL_TRACKING_CODE, tracking_code);
        }
        LedgerEventKind::Split {
            symbol,
            ratio_num,
            ratio_den,
        } => {
            row.set(COL_KIND, "Split");
            row.set(COL_SYMBOL, symbol.clone());
            row.set(COL_RATIO_NUM, enc_i64(*ratio_num));
            row.set(COL_RATIO_DEN, enc_i64(*ratio_den));
        }
        LedgerEventKind::Reversal { target_event_id } => {
            row.set(COL_KIND, "Reversal");
            row.set(COL_TARGET_EVENT_ID, target_event_id.clone());
        }
    }
    row
}

/// `Row → LedgerEvent`: the inverse total structural conversion. An unknown
/// `Kind` or a missing/unparseable required field is an integrity error
/// (STORE-LOAD-003), never a silent skip.
pub fn row_to_ledger(row: &Row) -> Result<LedgerEvent, StoreError> {
    let seq = dec_seq(row.get(COL_SEQ), COL_SEQ)?;
    let id = req_str(row, COL_EVENT_ID)?;
    let date = dec_date(row.get(COL_DATE), COL_DATE)?;

    let kind = match row.get(COL_KIND) {
        "Buy" => LedgerEventKind::Buy {
            lot_id: req_str(row, COL_LOT_ID)?,
            symbol: req_str(row, COL_SYMBOL)?,
            qty: dec_micro(row.get(COL_QTY), COL_QTY)?,
            unit_price_cents: dec_cents(row.get(COL_UNIT_PRICE), COL_UNIT_PRICE)?,
            fees_cents: dec_cents(row.get(COL_FEES), COL_FEES)?,
            platform: req_str(row, COL_PLATFORM)?,
            tracking_code: dec_opt(row.get(COL_TRACKING_CODE)),
        },
        "Vest" => LedgerEventKind::Vest {
            lot_id: req_str(row, COL_LOT_ID)?,
            symbol: req_str(row, COL_SYMBOL)?,
            qty: dec_micro(row.get(COL_QTY), COL_QTY)?,
            fmv_per_share_cents: dec_cents(row.get(COL_FMV), COL_FMV)?,
            platform: req_str(row, COL_PLATFORM)?,
            tracking_code: dec_opt(row.get(COL_TRACKING_CODE)),
        },
        "Sell" => LedgerEventKind::Sell {
            sale_id: req_str(row, COL_SALE_ID)?,
            symbol: req_str(row, COL_SYMBOL)?,
            qty: dec_micro(row.get(COL_QTY), COL_QTY)?,
            unit_price_cents: dec_cents(row.get(COL_UNIT_PRICE), COL_UNIT_PRICE)?,
            fees_cents: dec_cents(row.get(COL_FEES), COL_FEES)?,
            lot_refs: dec_lot_refs(row.get(COL_LOT_REFS))?,
            accrues_to_state: dec_opt(row.get(COL_ACCRUES_TO_STATE)),
            platform: req_str(row, COL_PLATFORM)?,
            tracking_code: dec_opt(row.get(COL_TRACKING_CODE)),
        },
        "Split" => LedgerEventKind::Split {
            symbol: req_str(row, COL_SYMBOL)?,
            ratio_num: dec_i64(row.get(COL_RATIO_NUM), COL_RATIO_NUM)?,
            ratio_den: dec_i64(row.get(COL_RATIO_DEN), COL_RATIO_DEN)?,
        },
        "Reversal" => LedgerEventKind::Reversal {
            target_event_id: req_str(row, COL_TARGET_EVENT_ID)?,
        },
        "" => return Err(StoreError::MissingField),
        _ => return Err(StoreError::UnknownKind),
    };

    Ok(LedgerEvent { id, seq, date, kind })
}

// ===========================================================================
// Tax row <-> event (STORE-GUARD-001/002). `tax::TaxEvent` has no `id` field;
// the store-assigned `EventId` rides on the row as a universal column, so a tax
// row round-trips as `(event, EventId)`.
// ===========================================================================

/// `(TaxEvent, EventId) → Row`: total, wildcard-free structural match over every
/// `TaxEventKind`. The `EventId` is store-assigned metadata carried on the row
/// (STORE-SCHEMA-003); the `TaxEvent` type itself has no id. The universal
/// `Date` column is populated from the kind's own date where it has one (a
/// human-readable convenience); reconstruction reads the typed field columns, so
/// the round-trip identity is over `(event, EventId)`. (STORE-GUARD-001)
///
/// @spec STORE-SCHEMA-005
pub fn tax_to_row(event: &TaxEvent, event_id: &EventId) -> Row {
    let mut row = Row::new();
    row.set(COL_SEQ, enc_seq(event.seq));
    row.set(COL_EVENT_ID, event_id.clone());

    // Wildcard-free exhaustive match over every TaxEventKind (the drift guard).
    match &event.kind {
        TaxEventKind::Allocate {
            accrual_key,
            account_label,
        } => {
            row.set(COL_KIND, "Allocate");
            set_accrual_key(&mut row, accrual_key);
            row.set(COL_ACCOUNT_LABEL, account_label.clone());
        }
        TaxEventKind::Move {
            accrual_key,
            amount_cents,
            date,
        } => {
            row.set(COL_KIND, "Move");
            set_accrual_key(&mut row, accrual_key);
            row.set(COL_AMOUNT, enc_cents(*amount_cents));
            row.set(COL_DATE, enc_date(*date));
        }
        TaxEventKind::Pay {
            jurisdiction,
            tax_year,
            period,
            amount_cents,
            date,
            covers,
        } => {
            row.set(COL_KIND, "Pay");
            row.set(COL_JURISDICTION, enc_jurisdiction(jurisdiction));
            row.set(COL_TAX_YEAR, enc_tax_year(*tax_year));
            row.set(COL_PERIOD, enc_quarter(*period));
            row.set(COL_AMOUNT, enc_cents(*amount_cents));
            row.set(COL_DATE, enc_date(*date));
            row.set(COL_COVERS, enc_covers(covers));
        }
        TaxEventKind::AmountOverride {
            accrual_key,
            applied_amount_cents,
            reason,
        } => {
            row.set(COL_KIND, "AmountOverride");
            set_accrual_key(&mut row, accrual_key);
            row.set(COL_APPLIED_AMOUNT, enc_cents(*applied_amount_cents));
            row.set(COL_REASON, reason.clone());
        }
        TaxEventKind::SeedMigration {
            jurisdiction,
            tax_year,
            applied_amount_cents,
            reason,
        } => {
            row.set(COL_KIND, "SeedMigration");
            row.set(COL_JURISDICTION, enc_jurisdiction(jurisdiction));
            row.set(COL_TAX_YEAR, enc_tax_year(*tax_year));
            row.set(COL_APPLIED_AMOUNT, enc_cents(*applied_amount_cents));
            row.set(COL_REASON, reason.clone());
        }
    }
    row
}

/// `Row → (TaxEvent, EventId)`: the inverse total structural conversion.
pub fn row_to_tax(row: &Row) -> Result<(TaxEvent, EventId), StoreError> {
    let seq = dec_seq(row.get(COL_SEQ), COL_SEQ)?;
    let event_id = req_str(row, COL_EVENT_ID)?;

    let kind = match row.get(COL_KIND) {
        "Allocate" => TaxEventKind::Allocate {
            accrual_key: get_accrual_key(row)?,
            account_label: req_str(row, COL_ACCOUNT_LABEL)?,
        },
        "Move" => TaxEventKind::Move {
            accrual_key: get_accrual_key(row)?,
            amount_cents: dec_cents(row.get(COL_AMOUNT), COL_AMOUNT)?,
            date: dec_date(row.get(COL_DATE), COL_DATE)?,
        },
        "Pay" => TaxEventKind::Pay {
            jurisdiction: dec_jurisdiction(row.get(COL_JURISDICTION))?,
            tax_year: dec_tax_year(row.get(COL_TAX_YEAR), COL_TAX_YEAR)?,
            period: dec_quarter(row.get(COL_PERIOD))?,
            amount_cents: dec_cents(row.get(COL_AMOUNT), COL_AMOUNT)?,
            date: dec_date(row.get(COL_DATE), COL_DATE)?,
            covers: dec_covers(row.get(COL_COVERS))?,
        },
        "AmountOverride" => TaxEventKind::AmountOverride {
            accrual_key: get_accrual_key(row)?,
            applied_amount_cents: dec_cents(row.get(COL_APPLIED_AMOUNT), COL_APPLIED_AMOUNT)?,
            reason: opt_str(row, COL_REASON),
        },
        "SeedMigration" => TaxEventKind::SeedMigration {
            jurisdiction: dec_jurisdiction(row.get(COL_JURISDICTION))?,
            tax_year: dec_tax_year(row.get(COL_TAX_YEAR), COL_TAX_YEAR)?,
            applied_amount_cents: dec_cents(row.get(COL_APPLIED_AMOUNT), COL_APPLIED_AMOUNT)?,
            reason: opt_str(row, COL_REASON),
        },
        "" => return Err(StoreError::MissingField),
        _ => return Err(StoreError::UnknownKind),
    };

    Ok((TaxEvent { seq, kind }, event_id))
}

// ===========================================================================
// Small structural codecs (no arithmetic — pure string<->scalar). Kept here so
// the conversions above are one place.
// ===========================================================================

// Field codecs are intentionally simple, total string conversions — no
// arithmetic, pure string<->scalar (STORE-GUARD-001).

fn enc_i64(v: i64) -> String {
    v.to_string()
}

fn dec_i64(s: &str, _col: &str) -> Result<i64, StoreError> {
    s.trim()
        .parse::<i64>()
        .map_err(|_| StoreError::MissingField)
}

fn enc_cents(c: Cents) -> String {
    enc_i64(c.0)
}

fn dec_cents(s: &str, col: &str) -> Result<Cents, StoreError> {
    Ok(Cents(dec_i64(s, col)?))
}

fn enc_micro(m: MicroShares) -> String {
    enc_i64(m.0)
}

fn dec_micro(s: &str, col: &str) -> Result<MicroShares, StoreError> {
    Ok(MicroShares(dec_i64(s, col)?))
}

fn enc_date(d: Date) -> String {
    (d.0 as i64).to_string()
}

fn dec_date(s: &str, col: &str) -> Result<Date, StoreError> {
    Ok(Date(dec_i64(s, col)? as i32))
}

fn enc_seq(s: Seq) -> String {
    s.0.to_string()
}

fn dec_seq(s: &str, col: &str) -> Result<Seq, StoreError> {
    let v = dec_i64(s, col)?;
    if v < 0 {
        return Err(StoreError::MissingField);
    }
    Ok(Seq(v as u64))
}

/// Read a REQUIRED string column: an empty/absent cell is a missing-field
/// integrity error (STORE-LOAD-003), never a silent default.
fn req_str(row: &Row, col: &str) -> Result<String, StoreError> {
    let v = row.get(col);
    if v.is_empty() {
        Err(StoreError::MissingField)
    } else {
        Ok(v.to_string())
    }
}

/// Read an OPTIONAL string column (empty cell → empty string). `reason` /
/// `account_label` round-trip as themselves, including the empty string.
fn opt_str(row: &Row, col: &str) -> String {
    row.get(col).to_string()
}

/// Set an `Option<String>` column: `None` leaves the cell unpopulated (sparse),
/// `Some(s)` writes it. The round-trip distinguishes them because an empty cell
/// decodes to `None`.
fn set_opt(row: &mut Row, col: &str, o: &Option<String>) {
    if let Some(v) = o {
        row.set(col, v.clone());
    }
}

/// Decode an `Option<String>` column: empty → `None`, else `Some`.
fn dec_opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// The reserved delimiter characters used by the hand-rolled list encodings
/// (`lot_refs`, `covers`) and the `%` escape introducer itself. A field value
/// (`lot_id`/`sale_id` from import or user input) containing any of these would
/// otherwise corrupt the round-trip, so [`esc`] percent-encodes them and [`unesc`]
/// reverses it — making `event → row → event` the identity for EVERY field value,
/// including delimiter-bearing ones (STORE-GUARD-002).
///
/// @spec STORE-GUARD-004
const RESERVED: &[char] = &['%', ';', ':', '~'];

/// Percent-encode the reserved delimiter characters so a field value carrying one
/// survives the list encodings byte-for-byte. `%` is escaped first (as the escape
/// introducer) by virtue of leading [`RESERVED`].
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if RESERVED.contains(&c) {
            // Percent + two-hex-digit byte(s). Reserved chars are all ASCII.
            for b in c.to_string().bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Reverse [`esc`]: decode `%XX` byte escapes back to the original string. A
/// malformed escape (`%` not followed by two hex digits) is a `MissingField`
/// integrity error rather than a silent corruption.
fn unesc(s: &str) -> Result<String, StoreError> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(StoreError::MissingField);
            }
            let hi = (bytes[i + 1] as char).to_digit(16).ok_or(StoreError::MissingField)?;
            let lo = (bytes[i + 2] as char).to_digit(16).ok_or(StoreError::MissingField)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| StoreError::MissingField)
}

/// Encode `Vec<LotRef>` as `lot_id:qty;lot_id:qty;…` (empty for FIFO fallback),
/// with each `lot_id` percent-escaped so a `:` or `;` inside an id cannot split a
/// ref or break the round-trip. Structural, reversible, no arithmetic.
/// (STORE-GUARD-001/002)
fn enc_lot_refs(refs: &[LotRef]) -> String {
    refs.iter()
        .map(|r| format!("{}:{}", esc(&r.lot_id), r.qty.0))
        .collect::<Vec<_>>()
        .join(";")
}

/// Decode the `lot_id:qty;…` encoding back to `Vec<LotRef>` (empty → empty Vec),
/// unescaping each `lot_id`.
fn dec_lot_refs(s: &str) -> Result<Vec<LotRef>, StoreError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for part in s.split(';') {
        let (lot_id, qty) = part.rsplit_once(':').ok_or(StoreError::MissingField)?;
        out.push(LotRef {
            lot_id: unesc(lot_id)?,
            qty: MicroShares(dec_i64(qty, COL_LOT_REFS)?),
        });
    }
    Ok(out)
}

// --- Tax-specific scalar codecs (Jurisdiction / TaxYear / Quarter). ---

/// `Federal` → `"Federal"`, `State("NJ")` → `"State:NJ"`. Structural, reversible.
fn enc_jurisdiction(j: &Jurisdiction) -> String {
    match j {
        Jurisdiction::Federal => "Federal".to_string(),
        Jurisdiction::State(code) => format!("State:{code}"),
    }
}

fn dec_jurisdiction(s: &str) -> Result<Jurisdiction, StoreError> {
    if s == "Federal" {
        Ok(Jurisdiction::Federal)
    } else if let Some(code) = s.strip_prefix("State:") {
        Ok(Jurisdiction::State(code.to_string()))
    } else {
        Err(StoreError::MissingField)
    }
}

fn enc_tax_year(y: TaxYear) -> String {
    y.0.to_string()
}

fn dec_tax_year(s: &str, col: &str) -> Result<TaxYear, StoreError> {
    Ok(TaxYear(dec_i64(s, col)? as i32))
}

/// `Quarter` ↔ `"Q1".."Q4"`. Wildcard-free so a new period fails compilation.
fn enc_quarter(q: Quarter) -> String {
    match q {
        Quarter::Q1 => "Q1",
        Quarter::Q2 => "Q2",
        Quarter::Q3 => "Q3",
        Quarter::Q4 => "Q4",
    }
    .to_string()
}

fn dec_quarter(s: &str) -> Result<Quarter, StoreError> {
    match s {
        "Q1" => Ok(Quarter::Q1),
        "Q2" => Ok(Quarter::Q2),
        "Q3" => Ok(Quarter::Q3),
        "Q4" => Ok(Quarter::Q4),
        _ => Err(StoreError::MissingField),
    }
}

// --- AccrualKey: stored across the dedicated accrual columns of one row. ---

fn set_accrual_key(row: &mut Row, k: &AccrualKey) {
    row.set(COL_ACCRUAL_SALE_ID, k.sale_id.clone());
    row.set(COL_ACCRUAL_LOT_ID, k.lot_id.clone());
    row.set(COL_JURISDICTION, enc_jurisdiction(&k.jurisdiction));
    row.set(COL_TAX_YEAR, enc_tax_year(k.tax_year));
}

fn get_accrual_key(row: &Row) -> Result<AccrualKey, StoreError> {
    Ok(AccrualKey {
        // sale_id / lot_id may be empty for a combined migration key, so they
        // are decoded as plain (possibly-empty) strings, not required.
        sale_id: row.get(COL_ACCRUAL_SALE_ID).to_string(),
        lot_id: row.get(COL_ACCRUAL_LOT_ID).to_string(),
        jurisdiction: dec_jurisdiction(row.get(COL_JURISDICTION))?,
        tax_year: dec_tax_year(row.get(COL_TAX_YEAR), COL_TAX_YEAR)?,
    })
}

/// Encode a `Pay`'s `covers: Vec<AccrualKey>` as
/// `sale~lot~jurisdiction~year; …`, with `sale_id`/`lot_id` and the jurisdiction
/// encoding percent-escaped so a `~`, `;`, or `:` inside any of them cannot split
/// a record/field or break the round-trip. Structural, reversible, no arithmetic.
/// (STORE-GUARD-001/002)
fn enc_covers(covers: &[AccrualKey]) -> String {
    covers
        .iter()
        .map(|k| {
            format!(
                "{}~{}~{}~{}",
                esc(&k.sale_id),
                esc(&k.lot_id),
                esc(&enc_jurisdiction(&k.jurisdiction)),
                enc_tax_year(k.tax_year)
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn dec_covers(s: &str) -> Result<Vec<AccrualKey>, StoreError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for rec in s.split(';') {
        let parts: Vec<&str> = rec.split('~').collect();
        if parts.len() != 4 {
            return Err(StoreError::MissingField);
        }
        out.push(AccrualKey {
            sale_id: unesc(parts[0])?,
            lot_id: unesc(parts[1])?,
            jurisdiction: dec_jurisdiction(&unesc(parts[2])?)?,
            tax_year: dec_tax_year(parts[3], COL_TAX_YEAR)?,
        });
    }
    Ok(out)
}
