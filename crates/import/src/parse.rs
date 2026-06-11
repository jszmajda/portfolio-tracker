//! The legacy-workbook PARSE: raw cell grids (as the Sheets values API returns
//! them) → the typed legacy rows the importer reconstructs from. Pure — no I/O;
//! the binary fetches the grids read-only and hands them here. The mapping is
//! the owner's ACTUAL layout: headers are located by NAME (the `Stock Actions`
//! tab carries a note row above its header), only source columns are read
//! (derived figures are never trusted), and a row whose required source cell is
//! missing/unparseable routes to the malformed list rather than being dropped.
//!
//! @spec IMPORT-RUN-008

use std::collections::BTreeMap;

use ledger_core::{Marks, Symbol};
use pt_core::{Cents, Date, MicroShares};

use crate::{
    parse_dollars_to_cents, LegacyActionKind, LegacyActionRow, LegacyPositionRow, LegacySaleRow,
    MalformedRow, RowCoord,
};

/// One tab's raw cell grid as the values API returns it (row-major strings).
pub type Grid = Vec<Vec<String>>;

/// The parsed legacy portfolio tabs plus the live `Cur $/s` marks (the
/// reconciliation prices, read off the legacy `Positions` tab so the unrealized
/// comparison shares the legacy sheet's own frame) and the malformed rows.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ParsedLegacy {
    pub actions: Vec<LegacyActionRow>,
    pub sales: Vec<LegacySaleRow>,
    pub positions: Vec<LegacyPositionRow>,
    /// Legacy `Positions.Cur $/s` per symbol → the dry-run reconciliation marks.
    pub marks: Marks,
    /// Source rows with a missing/unparseable REQUIRED cell (never dropped).
    pub malformed: Vec<MalformedRow>,
    /// Rows deliberately NOT reconstructed, each with its reason — reported,
    /// never silent: legacy `Split` delta rows (the owner-input `Split` events
    /// carry the split; the delta-row bookkeeping is the legacy sheet's own
    /// convention) and `Grant` rows (unvested grants are not holdings yet — see
    /// TODO.md's grant-tracking feature). (IMPORT-CORP-006)
    pub skipped: Vec<SkippedRow>,
    /// Per-symbol Σ of the sales tab's PRE-TAX `Profit` column — the realized
    /// reconcile target (the Positions "Realized Profit" is post-tax).
    /// (IMPORT-RECON-007)
    pub profit_sums: BTreeMap<String, Cents>,
}

/// A legacy row deliberately not reconstructed, with the reason it was skipped.
/// Reported in the dry-run output so an exclusion is always visible.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SkippedRow {
    pub coord: RowCoord,
    pub reason: String,
}

/// How the parser maps each tranche to a platform (a fact the legacy sheet does
/// not record EXCEPT in its tranche-id prefixes): longest-prefix match over the
/// tranche id (an owner-supplied prefix→platform map, e.g. `xy-` → SomeBroker),
/// then a per-symbol override, then the default.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PlatformAssignment {
    pub default_platform: String,
    /// Tranche-id prefix → platform (longest prefix wins).
    pub id_prefixes: BTreeMap<String, String>,
    pub per_symbol: BTreeMap<Symbol, String>,
}

impl PlatformAssignment {
    fn for_tranche(&self, tranche_id: &str, symbol: &str) -> String {
        let best = self
            .id_prefixes
            .iter()
            .filter(|(p, _)| tranche_id.starts_with(p.as_str()))
            .max_by_key(|(p, _)| p.len())
            .map(|(_, plat)| plat.clone());
        best.or_else(|| self.per_symbol.get(symbol).cloned())
            .unwrap_or_else(|| self.default_platform.clone())
    }
}

/// Parse the three legacy tabs. Grid rows are 1-based in `RowCoord` (matching
/// the sheet's own row numbers, header included), so a malformed row's coordinate
/// is directly clickable in the legacy sheet.
pub fn parse_legacy_tabs(
    actions_grid: &Grid,
    sales_grid: &Grid,
    positions_grid: &Grid,
    platforms: &PlatformAssignment,
) -> ParsedLegacy {
    let mut out = ParsedLegacy::default();
    parse_actions(actions_grid, platforms, &mut out);
    parse_sales(sales_grid, platforms, &mut out);
    parse_positions(positions_grid, &mut out);

    // Symbol-casing normalization: the legacy sheet is hand-typed, and a sale
    // row's symbol cell can differ in CASE from its tranche's (`privco` vs
    // `PrivCo`). The tranche reference is the stronger identity — when the two
    // agree case-insensitively, the sale adopts the tranche's casing so the
    // kernel's symbol/lot agreement check sees one symbol. A case-INSENSITIVE
    // mismatch is left alone (a genuinely wrong symbol must fail loudly).
    let tranche_symbols: BTreeMap<String, String> = out
        .actions
        .iter()
        .map(|a| (a.tranche_id.clone(), a.symbol.clone()))
        .collect();
    for s in &mut out.sales {
        if let Some(canon) = tranche_symbols.get(&s.tranche_id) {
            if s.symbol != *canon && s.symbol.eq_ignore_ascii_case(canon) {
                s.symbol = canon.clone();
            }
        }
    }
    // The same casing normalization for the profit-sum keys (a lowercase sale
    // symbol must land on the canonical symbol's target sum).
    let mut canon_sums: BTreeMap<String, Cents> = BTreeMap::new();
    for (sym, sum) in std::mem::take(&mut out.profit_sums) {
        let canon = out
            .sales
            .iter()
            .find(|s| s.symbol.eq_ignore_ascii_case(&sym))
            .map(|s| s.symbol.clone())
            .unwrap_or(sym);
        canon_sums.entry(canon).or_insert(Cents(0)).0 += sum.0;
    }
    out.profit_sums = canon_sums;

    // The realized reconcile target is the PRE-TAX per-symbol Profit sum — the
    // Positions cell (post-tax net of the sheet's estimated tax) is replaced
    // here at the boundary. A never-sold symbol's target stays zero either way.
    // (IMPORT-RECON-007)
    for p in &mut out.positions {
        p.realized_pnl_cents = out.profit_sums.get(&p.symbol).copied().unwrap_or(Cents(0));
    }
    out
}

// ---------------------------------------------------------------------------
// Header location + cell access. Headers are matched by trimmed, case-insensitive
// NAME so a cosmetic edit (or the `Stock Actions` note row) cannot silently shift
// a source column. (IMPORT-RUN-008)
// ---------------------------------------------------------------------------

/// Find the header row: the first row containing `anchor` (trimmed,
/// case-insensitive) in any cell. Returns `(row_index, name → column_index)`.
fn locate_header(grid: &Grid, anchor: &str) -> Option<(usize, BTreeMap<String, usize>)> {
    for (i, row) in grid.iter().enumerate() {
        if row.iter().any(|c| c.trim().eq_ignore_ascii_case(anchor)) {
            let map = row
                .iter()
                .enumerate()
                .map(|(j, c)| (c.trim().to_ascii_lowercase(), j))
                .collect();
            return Some((i, map));
        }
    }
    None
}

fn cell<'a>(row: &'a [String], cols: &BTreeMap<String, usize>, name: &str) -> &'a str {
    cols.get(name)
        .and_then(|&j| row.get(j))
        .map(|s| s.trim())
        .unwrap_or("")
}

/// `M/D/YYYY` (the legacy sheet's date format) → days since 1970-01-01.
/// Howard Hinnant's days_from_civil; no float, no chrono.
pub fn parse_legacy_date(s: &str) -> Option<Date> {
    let mut it = s.trim().split('/');
    let m: i64 = it.next()?.trim().parse().ok()?;
    let d: i64 = it.next()?.trim().parse().ok()?;
    let y: i64 = it.next()?.trim().parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) || y < 1900 {
        return None;
    }
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(Date((era * 146_097 + doe - 719_468) as i32))
}

/// A legacy share count (`"4"`, `"280"`, `"0.5"`) → `MicroShares` (1e-6),
/// exactly; `None` for blank/`#…`/unparseable or more than 6 fractional digits.
pub fn parse_shares_to_micro(s: &str) -> Option<MicroShares> {
    let t = s.trim().replace(',', "");
    if t.is_empty() || t.starts_with('#') {
        return None;
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.as_str()),
    };
    let mut parts = body.splitn(2, '.');
    let whole_str = parts.next().unwrap_or("");
    let frac_str = parts.next().unwrap_or("");
    if whole_str.chars().any(|c| !c.is_ascii_digit())
        || frac_str.chars().any(|c| !c.is_ascii_digit())
        || (whole_str.is_empty() && frac_str.is_empty())
        || frac_str.len() > 6
    {
        return None;
    }
    let whole: i64 = if whole_str.is_empty() {
        0
    } else {
        whole_str.parse().ok()?
    };
    let frac: i64 = if frac_str.is_empty() {
        0
    } else {
        format!("{frac_str:0<6}").parse().ok()?
    };
    let micro = whole.checked_mul(1_000_000)?.checked_add(frac)?;
    Some(MicroShares(if neg { -micro } else { micro }))
}

/// Sum two legacy dollar cells (Commission + Fees) into one decimal-dollar
/// string for the row boundary; a blank cell counts as zero (the legacy sheet
/// leaves fee cells empty on many sale rows).
fn fees_total(commission: &str, fees: &str) -> Option<String> {
    let one = |s: &str| -> Option<Cents> {
        if s.trim().is_empty() {
            Some(Cents(0))
        } else {
            parse_dollars_to_cents(s)
        }
    };
    let total = one(commission)?.0.checked_add(one(fees)?.0)?;
    Some(format!("{}.{:02}", total / 100, (total % 100).abs()))
}

// ---------------------------------------------------------------------------
// Per-tab parses. Required source cells route to `malformed` on failure; rows
// that are entirely blank (or the header/note furniture) are skipped silently.
// ---------------------------------------------------------------------------

fn is_blank(row: &[String]) -> bool {
    row.iter().all(|c| c.trim().is_empty())
}

fn parse_actions(grid: &Grid, platforms: &PlatformAssignment, out: &mut ParsedLegacy) {
    const TAB: &str = "Stock Actions";
    let Some((hdr, cols)) = locate_header(grid, "Activity") else {
        out.malformed.push(MalformedRow {
            coord: RowCoord::new(TAB, 1),
            reason: "no header row containing `Activity` found".to_string(),
        });
        return;
    };
    for (i, row) in grid.iter().enumerate().skip(hdr + 1) {
        if is_blank(row) {
            continue;
        }
        let coord = RowCoord::new(TAB, (i + 1) as u32);
        let id = cell(row, &cols, "id");
        let activity = cell(row, &cols, "activity");
        // The owner's activity vocabulary: Buy and Vest reconstruct directly; an
        // Exercise IS a purchase (the cash paid at the strike is the basis) and
        // reconstructs as a Buy carrying an `exercise` provenance note; a legacy
        // `Split` row is the sheet's delta-share bookkeeping (skipped — the
        // owner-input Split events carry the split, and a sale referencing the
        // delta row is remapped to its original tranche); a `Grant` is unvested
        // equity, not a holding (skipped, pending the grant-tracking feature).
        // Anything else is surfaced, not guessed. (IMPORT-MAP-005, IMPORT-CORP-006)
        let (kind, tracking_code) = match activity.to_ascii_lowercase().as_str() {
            "buy" => (LegacyActionKind::Buy, None),
            "vest" => (LegacyActionKind::Vest, None),
            "exercise" => (LegacyActionKind::Buy, Some("exercise".to_string())),
            "split" => {
                out.skipped.push(SkippedRow {
                    coord,
                    reason: format!(
                        "legacy Split delta row ({} {}): the owner-input Split event carries the split",
                        cell(row, &cols, "stock"),
                        cell(row, &cols, "shares"),
                    ),
                });
                continue;
            }
            "grant" => {
                out.skipped.push(SkippedRow {
                    coord,
                    reason: format!(
                        "unvested Grant ({} {}): not a holding yet (TODO.md: grant tracking)",
                        cell(row, &cols, "stock"),
                        cell(row, &cols, "shares"),
                    ),
                });
                continue;
            }
            "" if id.is_empty() => continue, // residual formatting row
            other => {
                out.malformed.push(MalformedRow {
                    coord,
                    reason: format!("unknown Activity {other:?}"),
                });
                continue;
            }
        };
        let symbol = cell(row, &cols, "stock");
        let date = parse_legacy_date(cell(row, &cols, "date"));
        let qty = parse_shares_to_micro(cell(row, &cols, "shares"));
        let fees = fees_total(cell(row, &cols, "commission"), cell(row, &cols, "fees"));
        let (Some(date), Some(qty), Some(fees)) = (date, qty, fees) else {
            out.malformed.push(MalformedRow {
                coord,
                reason: format!(
                    "unparseable required cell (Date {:?} / Shares {:?} / fees)",
                    cell(row, &cols, "date"),
                    cell(row, &cols, "shares"),
                ),
            });
            continue;
        };
        if id.is_empty() || symbol.is_empty() {
            out.malformed.push(MalformedRow {
                coord,
                reason: "blank ID or Stock on a source row".to_string(),
            });
            continue;
        }
        // The raw `$/share` decimal string IS the row boundary: blank/`#…`
        // stays as-is for the reconstruction's unrecoverable-Vest-source rule.
        let mut dps = cell(row, &cols, "$/share")
            .trim_start_matches('$')
            .replace(',', "");
        let mut fees = fees;
        // True-cash-basis rule (IMPORT-MAP-006): on a whole-share Buy, when the
        // row's `Total Cost` disagrees with `shares × $/share + fees` (the
        // `$/share` cell is display-rounded, or an Exercise aggregates strikes),
        // re-derive unit price = ⌊Total Cost / shares⌋ and fold the exact
        // remainder into fees, so the reconstructed basis equals the cash paid.
        // Vests carry FMV semantics ($0 Total Cost by design) — never touched.
        if kind == LegacyActionKind::Buy && qty.0 > 0 && qty.0 % 1_000_000 == 0 {
            let whole_shares = qty.0 / 1_000_000;
            let tc = parse_dollars_to_cents(cell(row, &cols, "total cost"));
            let price = parse_dollars_to_cents(&dps);
            let fee_c = parse_dollars_to_cents(&fees).unwrap_or(Cents(0));
            if let (Some(tc), Some(price)) = (tc, price) {
                let implied = whole_shares
                    .checked_mul(price.0)
                    .and_then(|b| b.checked_add(fee_c.0));
                if tc.0 > 0 && implied != Some(tc.0) {
                    let unit = tc.0 / whole_shares; // floor: remainder is non-negative
                    let remainder = tc.0 - unit * whole_shares;
                    dps = format!("{}.{:02}", unit / 100, (unit % 100).abs());
                    fees = format!("{}.{:02}", remainder / 100, (remainder % 100).abs());
                }
            }
        }
        out.actions.push(LegacyActionRow {
            coord,
            tranche_id: id.to_string(),
            symbol: symbol.to_string(),
            kind,
            qty,
            dollars_per_share: dps,
            fees_dollars: fees,
            date,
            platform: platforms.for_tranche(id, symbol),
            tracking_code,
        });
    }
}

fn parse_sales(grid: &Grid, platforms: &PlatformAssignment, out: &mut ParsedLegacy) {
    const TAB: &str = "Stock Sales";
    let Some((hdr, cols)) = locate_header(grid, "Shares Sold") else {
        out.malformed.push(MalformedRow {
            coord: RowCoord::new(TAB, 1),
            reason: "no header row containing `Shares Sold` found".to_string(),
        });
        return;
    };
    for (i, row) in grid.iter().enumerate().skip(hdr + 1) {
        if is_blank(row) {
            continue;
        }
        let coord = RowCoord::new(TAB, (i + 1) as u32);
        let tranche_id = cell(row, &cols, "id");
        let symbol = cell(row, &cols, "stock");
        if tranche_id.is_empty() && symbol.is_empty() {
            continue; // residual formatting row
        }
        let date = parse_legacy_date(cell(row, &cols, "date"));
        let qty = parse_shares_to_micro(cell(row, &cols, "shares sold"));
        let fees = fees_total(cell(row, &cols, "commission"), cell(row, &cols, "fees"));
        let (Some(date), Some(qty), Some(fees)) = (date, qty, fees) else {
            out.malformed.push(MalformedRow {
                coord,
                reason: format!(
                    "unparseable required cell (Date {:?} / Shares Sold {:?} / fees)",
                    cell(row, &cols, "date"),
                    cell(row, &cols, "shares sold"),
                ),
            });
            continue;
        };
        if tranche_id.is_empty() || symbol.is_empty() {
            out.malformed.push(MalformedRow {
                coord,
                reason: "blank ID or Stock on a source row".to_string(),
            });
            continue;
        }
        // The PRE-TAX realized target: the sale's `Profit` cell, summed per
        // symbol (the Positions "Realized Profit" is net of estimated tax and is
        // not a valid pre-tax comparison). Unparseable Profit on a sale row is a
        // malformed source row; blank reads zero. (IMPORT-RECON-007)
        let profit_cell = cell(row, &cols, "profit");
        let profit = if profit_cell.is_empty() {
            Some(Cents(0))
        } else {
            parse_dollars_to_cents(profit_cell)
        };
        let Some(profit) = profit else {
            out.malformed.push(MalformedRow {
                coord,
                reason: format!("unparseable Profit {profit_cell:?} (the pre-tax realized target)"),
            });
            continue;
        };
        let entry = out
            .profit_sums
            .entry(symbol.to_string())
            .or_insert(Cents(0));
        entry.0 += profit.0;

        // Never-filled price cell (IMPORT-MAP-007): a zero/blank `$/share` with
        // POSITIVE recorded proceeds derives the price from `Sales - Fees` so
        // reconstructed proceeds equal the recorded proceeds to the cent. A
        // present price always governs; a zero-price zero-proceeds Vest sale is
        // the sell-to-cover case the reconstruction substitutes FMV for.
        let mut dps = cell(row, &cols, "$/share")
            .trim_start_matches('$')
            .replace(',', "");
        let mut fees = fees;
        let price_zero = matches!(parse_dollars_to_cents(&dps), Some(Cents(0)) | None);
        if price_zero && qty.0 > 0 && qty.0 % 1_000_000 == 0 {
            let whole = qty.0 / 1_000_000;
            let sf = parse_dollars_to_cents(cell(row, &cols, "sales - fees"));
            let fee_c = parse_dollars_to_cents(&fees).unwrap_or(Cents(0));
            if let Some(sf) = sf.filter(|sf| sf.0 > 0) {
                let gross = sf.0 + fee_c.0;
                let (unit, new_fees) = if gross % whole == 0 {
                    (gross / whole, fee_c.0) // exact: the true execution price
                } else {
                    let unit = sf.0.div_euclid(whole) + 1; // ceil(SF/shares)
                    (unit, whole * unit - sf.0)
                };
                dps = format!("{}.{:02}", unit / 100, (unit % 100).abs());
                fees = format!("{}.{:02}", new_fees / 100, (new_fees % 100).abs());
            }
        }

        out.sales.push(LegacySaleRow {
            coord,
            // The legacy sheet reuses the tranche id across its sales of that
            // tranche; the row number makes the tag locally unique while staying
            // deterministic (it is also the EventId's row-coordinate half).
            sale_tag: format!("{tranche_id}@r{}", i + 1),
            tranche_id: tranche_id.to_string(),
            symbol: symbol.to_string(),
            qty,
            dollars_per_share: dps,
            fees_dollars: fees,
            date,
            platform: platforms.for_tranche(tranche_id, symbol),
        });
    }
}

fn parse_positions(grid: &Grid, out: &mut ParsedLegacy) {
    const TAB: &str = "Positions";
    let Some((hdr, cols)) = locate_header(grid, "Cur Shs") else {
        out.malformed.push(MalformedRow {
            coord: RowCoord::new(TAB, 1),
            reason: "no header row containing `Cur Shs` found".to_string(),
        });
        return;
    };
    for (i, row) in grid.iter().enumerate().skip(hdr + 1) {
        if is_blank(row) {
            continue;
        }
        let coord = RowCoord::new(TAB, (i + 1) as u32);
        let symbol = cell(row, &cols, "stock");
        if symbol.is_empty() {
            continue; // totals/furniture rows carry no Stock
        }
        let shares = parse_shares_to_micro(cell(row, &cols, "cur shs"));
        // The live `Cur $/s` mark first: an unmarked symbol (a private holding's
        // `#N/A` GOOGLEFINANCE) has no computable unrealized anywhere.
        let mark = parse_dollars_to_cents(cell(row, &cols, "cur $/s"));
        // Realized/unrealized are the RECONCILE TARGETS; a blank cell reads as
        // zero (the legacy sheet leaves them empty for never-sold symbols), but a
        // `#…` error in a TARGET cell is malformed — the comparison would be
        // meaningless. Exception: an UNMARKED symbol's `#N/A` unrealized is the
        // derived live column doing the only thing it can — it reads as zero,
        // since no unrealized comparison can exist without a mark.
        let money = |name: &str| -> Option<Cents> {
            let v = cell(row, &cols, name);
            if v.is_empty() {
                Some(Cents(0))
            } else {
                parse_dollars_to_cents(v)
            }
        };
        let realized = money("realized profit");
        let unrealized = match money("unr. profit") {
            None if mark.is_none() => Some(Cents(0)),
            u => u,
        };
        let (Some(shares), Some(realized), Some(unrealized)) = (shares, realized, unrealized)
        else {
            out.malformed.push(MalformedRow {
                coord,
                reason: format!(
                    "unparseable reconcile cell (Cur Shs {:?} / Realized {:?} / Unr. {:?})",
                    cell(row, &cols, "cur shs"),
                    cell(row, &cols, "realized profit"),
                    cell(row, &cols, "unr. profit"),
                ),
            });
            continue;
        };
        // The live `Cur $/s` is the reconciliation mark; `#…`/blank means the
        // symbol is unmarked (the reconciliation degrades per its own rule).
        if let Some(mark) = mark {
            out.marks.insert(symbol.to_string(), mark);
        }
        out.positions.push(LegacyPositionRow {
            symbol: symbol.to_string(),
            shares,
            realized_pnl_cents: realized,
            unrealized_cents: unrealized,
        });
    }
}
