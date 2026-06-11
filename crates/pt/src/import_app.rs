//! `pt import` — the one-time legacy migration's binary glue: read the owner
//! inputs, fetch the three legacy tabs READ-ONLY through the runtime Sheets
//! client, parse them (`import::parse`), run the dry-run, and render the
//! reconciliation report. `--commit` is the owner's explicit acceptance gesture:
//! it re-runs the dry-run fresh, accepts the report, and commits into the NEW
//! workbook through the `Boot`-built store (the legacy workbook is never
//! written). (import-design.md → "Run Mechanics")
//!
//! @spec IMPORT-RUN-008

use std::collections::BTreeMap;
use std::error::Error;

use config::{Jurisdiction, ResidencyEntry, ResidencyTimeline, TaxYear};
use import::parse::{parse_legacy_tabs, Grid, ParsedLegacy, PlatformAssignment};
use import::{
    dry_run, ClosedYear, DryRunReport, KnownCorporateAction, LegacyWorkbook, ShareVerdict, Verdict,
};
use pt_core::{Cents, Date};
use runtime::sheets::{GoogleSheetsApi, SheetsApi};

/// The gitignored owner-inputs file: one-time migration facts the legacy sheet
/// does not record (the legacy workbook id, platforms, splits, closed years,
/// the residency timeline).
pub const OWNER_INPUTS_FILE: &str = "import.local.json";

/// The owner inputs, parsed from [`OWNER_INPUTS_FILE`].
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct OwnerInputs {
    pub legacy_workbook_id: String,
    pub platforms: PlatformAssignment,
    pub corporate_actions: Vec<KnownCorporateAction>,
    pub closed_years: Vec<ClosedYear>,
    pub residency: Vec<ResidencyEntry>,
    pub founding_residency_asserted: bool,
    /// Sale-referenced legacy split DELTA-row ids → the ORIGINAL tranche the
    /// delta shares came from (e.g. AMZN `"31"` → `"2"`): the legacy sheet
    /// booked a split as a sellable +shares row; the new model rescales the
    /// original lot, so its sales remap there. (IMPORT-CORP-006)
    pub split_row_remap: BTreeMap<String, String>,
    /// Tranche ids whose legacy rows were RETROACTIVELY recorded in the
    /// post-split frame (qty and $/share already ×/÷ by the ratio) despite a
    /// pre-split date: the glue converts them back to the pre-split frame at
    /// the boundary (qty ÷ ratio, price × ratio — exact, dates untouched) so
    /// the replayed Split rescales them once, correctly. (IMPORT-CORP-007)
    pub post_split_framed_ids: Vec<String>,
    /// Owner-declared symbol exclusions (reported, never silent): e.g. unvested
    /// grants (TODO.md: grant tracking) and non-security holdings.
    pub excluded_symbols: Vec<String>,
    /// Owner-adjudicated divergences: symbol → the owner's stated reason for an
    /// otherwise-Unexplained dollar residual (a known-wrong legacy cell). The
    /// reason is recorded verbatim in the report and commit record.
    pub adjudicated_divergences: BTreeMap<String, String>,
}

/// Parse the owner-inputs JSON. Every failure is a loud, named error — a silent
/// default on a migration input could misclassify years of history.
pub fn parse_owner_inputs(json: &str) -> Result<OwnerInputs, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("{OWNER_INPUTS_FILE}: {e}"))?;
    let obj = v.as_object().ok_or("owner inputs must be a JSON object")?;

    let str_field = |name: &str| -> Result<String, String> {
        obj.get(name)
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .ok_or(format!("missing/invalid `{name}` (string)"))
    };

    let legacy_workbook_id = str_field("legacy_workbook_id")?;
    let default_platform = str_field("default_platform")?;
    let str_map = |name: &str| -> Result<BTreeMap<String, String>, String> {
        let mut out = BTreeMap::new();
        if let Some(m) = obj.get(name).and_then(|x| x.as_object()) {
            for (k, val) in m {
                let p = val.as_str().ok_or(format!("{name}.{k} must be a string"))?;
                out.insert(k.clone(), p.to_string());
            }
        }
        Ok(out)
    };
    let per_symbol = str_map("per_symbol_platforms")?;
    let id_prefixes = str_map("platform_prefixes")?;
    let split_row_remap = str_map("split_row_remap")?;
    let str_list = |name: &str| -> Vec<String> {
        obj.get(name)
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let post_split_framed_ids = str_list("post_split_framed_ids");
    let excluded_symbols = str_list("excluded_symbols");

    let mut corporate_actions = Vec::new();
    if let Some(arr) = obj.get("corporate_actions").and_then(|x| x.as_array()) {
        for (i, a) in arr.iter().enumerate() {
            let err = |w: &str| format!("corporate_actions[{i}]: {w}");
            corporate_actions.push(KnownCorporateAction {
                symbol: a
                    .get("symbol")
                    .and_then(|x| x.as_str())
                    .ok_or(err("missing `symbol`"))?
                    .to_string(),
                date: parse_iso_date(
                    a.get("date")
                        .and_then(|x| x.as_str())
                        .ok_or(err("missing `date`"))?,
                )
                .ok_or(err("`date` must be YYYY-MM-DD"))?,
                ratio_num: a
                    .get("num")
                    .and_then(|x| x.as_i64())
                    .ok_or(err("missing `num`"))?,
                ratio_den: a
                    .get("den")
                    .and_then(|x| x.as_i64())
                    .ok_or(err("missing `den`"))?,
            });
        }
    }

    let mut closed_years = Vec::new();
    if let Some(arr) = obj.get("closed_years").and_then(|x| x.as_array()) {
        for (i, c) in arr.iter().enumerate() {
            let err = |w: &str| format!("closed_years[{i}]: {w}");
            let juris = c
                .get("jurisdiction")
                .and_then(|x| x.as_str())
                .ok_or(err("missing `jurisdiction` (\"federal\" or a state code)"))?;
            closed_years.push(ClosedYear {
                jurisdiction: if juris.eq_ignore_ascii_case("federal") {
                    Jurisdiction::Federal
                } else {
                    Jurisdiction::State(juris.to_uppercase())
                },
                tax_year: TaxYear(
                    c.get("tax_year")
                        .and_then(|x| x.as_i64())
                        .ok_or(err("missing `tax_year`"))? as i32,
                ),
                legacy_actual_cents: Cents(
                    c.get("legacy_actual_cents")
                        .and_then(|x| x.as_i64())
                        .ok_or(err("missing `legacy_actual_cents` (integer cents)"))?,
                ),
                account_label: c
                    .get("account_label")
                    .and_then(|x| x.as_str())
                    .ok_or(err("missing `account_label`"))?
                    .to_string(),
            });
        }
    }

    let res_arr = obj
        .get("residency")
        .and_then(|x| x.as_array())
        .ok_or("missing `residency` (array; needs at least the founding entry)")?;
    let mut residency = Vec::new();
    for (i, r) in res_arr.iter().enumerate() {
        let err = |w: &str| format!("residency[{i}]: {w}");
        residency.push(ResidencyEntry {
            effective_date: parse_iso_date(
                r.get("effective_date")
                    .and_then(|x| x.as_str())
                    .ok_or(err("missing `effective_date`"))?,
            )
            .ok_or(err("`effective_date` must be YYYY-MM-DD"))?,
            state_code: r
                .get("state")
                .and_then(|x| x.as_str())
                .ok_or(err("missing `state`"))?
                .to_uppercase(),
        });
    }
    if residency.is_empty() {
        return Err("`residency` needs at least the founding entry".to_string());
    }

    Ok(OwnerInputs {
        legacy_workbook_id,
        platforms: PlatformAssignment {
            default_platform,
            id_prefixes,
            per_symbol,
        },
        corporate_actions,
        closed_years,
        residency,
        founding_residency_asserted: obj
            .get("founding_residency_asserted")
            .and_then(|x| x.as_bool())
            .unwrap_or(true),
        split_row_remap,
        post_split_framed_ids,
        excluded_symbols,
        adjudicated_divergences: str_map("adjudicated_divergences")?,
    })
}

/// `YYYY-MM-DD` → days since 1970-01-01 (days_from_civil; no float, no chrono).
pub fn parse_iso_date(s: &str) -> Option<Date> {
    let mut it = s.trim().split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
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

/// Apply the owner mappings to the parsed rows, in order: symbol EXCLUSIONS
/// (filtered out of actions/sales/positions, each recorded — reported, never
/// silent), split delta-row REMAP (a sale referencing a legacy split row sells
/// from the original tranche), and the post-split-frame REWRITE (a row recorded
/// retroactively in post-split units converts back to its pre-split frame:
/// qty ÷ ratio, $/share × ratio — exact integer math, the true date untouched —
/// so the replayed Split rescales it once, correctly). Returns the excluded-row
/// notes for the report.
///
/// @spec IMPORT-CORP-006, IMPORT-CORP-007
pub fn apply_owner_mappings(
    parsed: &mut ParsedLegacy,
    inputs: &OwnerInputs,
) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();

    // 1. Owner-declared exclusions (case-insensitive symbol match), recorded.
    let excluded: Vec<String> = inputs
        .excluded_symbols
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let is_excluded = |sym: &str| excluded.contains(&sym.to_ascii_lowercase());
    parsed.actions.retain(|a| {
        let keep = !is_excluded(&a.symbol);
        if !keep {
            notes.push(format!(
                "excluded {} {} ({}!row {}): owner-declared exclusion",
                a.symbol, a.tranche_id, a.coord.tab, a.coord.row
            ));
        }
        keep
    });
    parsed.sales.retain(|s| {
        let keep = !is_excluded(&s.symbol);
        if !keep {
            notes.push(format!(
                "excluded {} sale ({}!row {}): owner-declared exclusion",
                s.symbol, s.coord.tab, s.coord.row
            ));
        }
        keep
    });
    parsed.positions.retain(|p| {
        let keep = !is_excluded(&p.symbol);
        if !keep {
            notes.push(format!(
                "excluded {} Positions row: owner-declared exclusion",
                p.symbol
            ));
        }
        keep
    });

    // 2. Split delta-row remap: the sale sells from the ORIGINAL tranche.
    for s in &mut parsed.sales {
        if let Some(original) = inputs.split_row_remap.get(&s.tranche_id) {
            notes.push(format!(
                "sale {}!row {}: tranche {} (legacy split delta row) remapped to {}",
                s.coord.tab, s.coord.row, s.tranche_id, original
            ));
            s.tranche_id = original.clone();
        }
    }

    // 3. Post-split-frame rewrite. The relevant split is the symbol's earliest
    //    owner-input corporate action dated AFTER the row (the one the row's
    //    post-split units presume). Divisibility must be exact — a remainder
    //    means the frame assumption is wrong, which must be a loud error.
    let split_after = |symbol: &str, date: Date| -> Option<&KnownCorporateAction> {
        inputs
            .corporate_actions
            .iter()
            .filter(|ca| ca.symbol == symbol && ca.date.0 > date.0)
            .min_by_key(|ca| ca.date.0)
    };
    let rewrite_qty =
        |qty: pt_core::MicroShares, ca: &KnownCorporateAction| -> Option<pt_core::MicroShares> {
            let scaled = qty.0.checked_mul(ca.ratio_den)?;
            (scaled % ca.ratio_num == 0).then(|| pt_core::MicroShares(scaled / ca.ratio_num))
        };
    let rewrite_price = |dps: &str, ca: &KnownCorporateAction| -> Option<String> {
        // price × num / den, exact in cents.
        let cents = import::parse_dollars_to_cents(dps)?;
        let scaled = cents.0.checked_mul(ca.ratio_num)?;
        (scaled % ca.ratio_den == 0).then(|| {
            format!(
                "{}.{:02}",
                scaled / ca.ratio_den / 100,
                (scaled / ca.ratio_den % 100).abs()
            )
        })
    };
    for id in &inputs.post_split_framed_ids {
        for a in parsed.actions.iter_mut().filter(|a| &a.tranche_id == id) {
            let Some(ca) = split_after(&a.symbol, a.date) else {
                continue;
            };
            let qty = rewrite_qty(a.qty, ca).ok_or(format!(
                "{id}: qty not divisible by the {}:{} split",
                ca.ratio_num, ca.ratio_den
            ))?;
            let dps = rewrite_price(&a.dollars_per_share, ca)
                .ok_or(format!("{id}: $/share does not rescale exactly"))?;
            notes.push(format!(
                "{} ({}!row {}): post-split-framed row converted to pre-split frame ({} sh @ ${})",
                id,
                a.coord.tab,
                a.coord.row,
                qty.0 as f64 / 1e6,
                dps
            ));
            a.qty = qty;
            a.dollars_per_share = dps;
        }
        // Sales of the flagged tranche dated BEFORE the split share its frame.
        let mut sale_fixes = Vec::new();
        for s in parsed.sales.iter().filter(|s| &s.tranche_id == id) {
            if let Some(ca) = split_after(&s.symbol, s.date) {
                sale_fixes.push((s.coord.clone(), ca.clone()));
            }
        }
        for (coord, ca) in sale_fixes {
            let s = parsed.sales.iter_mut().find(|s| s.coord == coord).unwrap();
            let qty = rewrite_qty(s.qty, &ca).ok_or(format!(
                "{id} sale row {}: qty not divisible by the split",
                coord.row
            ))?;
            let dps = rewrite_price(&s.dollars_per_share, &ca).ok_or(format!(
                "{id} sale row {}: $/share does not rescale exactly",
                coord.row
            ))?;
            notes.push(format!(
                "sale {}!row {}: post-split-framed sale converted to pre-split frame ({} sh @ ${})",
                coord.tab,
                coord.row,
                qty.0 as f64 / 1e6,
                dps
            ));
            s.qty = qty;
            s.dollars_per_share = dps;
        }
    }
    Ok(notes)
}

/// Assemble the typed [`LegacyWorkbook`] from the parsed tabs + owner inputs.
/// Pure; the residency timeline is validated by `from_entries` (sorted,
/// founding-entry rules live in the import pre-pass).
pub fn assemble_workbook(
    parsed: &ParsedLegacy,
    inputs: &OwnerInputs,
) -> Result<LegacyWorkbook, String> {
    let residency = ResidencyTimeline::from_entries(inputs.residency.clone())
        .map_err(|e| format!("residency timeline: {e:?}"))?;
    Ok(LegacyWorkbook {
        actions: parsed.actions.clone(),
        sales: parsed.sales.clone(),
        positions: parsed.positions.clone(),
        corporate_actions: inputs.corporate_actions.clone(),
        closed_years: inputs.closed_years.clone(),
        residency,
        founding_residency_asserted: inputs.founding_residency_asserted,
        corrections: BTreeMap::new(),
        adjudicated: inputs.adjudicated_divergences.clone(),
    })
}

/// Fetch the three legacy tabs READ-ONLY. This client only ever issues
/// `values.get` reads; the import never writes to the legacy workbook.
pub fn fetch_legacy_grids(
    credentials_path: &str,
    legacy_workbook_id: &str,
) -> Result<(Grid, Grid, Grid), Box<dyn Error>> {
    let api = GoogleSheetsApi::from_credentials_file(legacy_workbook_id, credentials_path)?;
    let actions = api.read_range("'Stock Actions'!A:AC")?;
    let sales = api.read_range("'Stock Sales'!A:Q")?;
    let positions = api.read_range("'Positions'!A:W")?;
    Ok((actions, sales, positions))
}

/// Render the dry-run report as the terminal text the owner reviews.
pub fn render_report(report: &DryRunReport, parsed: &ParsedLegacy, notes: &[String]) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let recon = &report.reconstruction;
    let _ = writeln!(s, "== pt import — dry run (nothing written) ==");
    let _ = writeln!(
        s,
        "parsed: {} actions, {} sales, {} positions | reconstructed: {} ledger events, {} tax events",
        parsed.actions.len(),
        parsed.sales.len(),
        parsed.positions.len(),
        recon.ledger.len(),
        recon.tax.len(),
    );

    if !parsed.skipped.is_empty() {
        let _ = writeln!(s, "\nSkipped rows (deliberate, non-blocking):");
        for sk in &parsed.skipped {
            let _ = writeln!(
                s,
                "  - {}!row {}: {}",
                sk.coord.tab, sk.coord.row, sk.reason
            );
        }
    }
    if !notes.is_empty() {
        let _ = writeln!(
            s,
            "\nOwner-mapping notes (exclusions / remaps / frame rewrites):"
        );
        for n in notes {
            let _ = writeln!(s, "  - {n}");
        }
    }

    // BOTH malformed channels block: the parse boundary's (a required source
    // cell failed) and the reconstruction's. (IMPORT-RUN-011, IMPORT-RECON-006)
    let malformed_total = parsed.malformed.len() + recon.malformed.len();
    if malformed_total > 0 {
        let _ = writeln!(
            s,
            "\nMALFORMED SOURCE ROWS ({malformed_total} — block commit):"
        );
        for m in parsed.malformed.iter().chain(&recon.malformed) {
            let _ = writeln!(s, "  - {}!row {}: {}", m.coord.tab, m.coord.row, m.reason);
        }
    }

    let _ = writeln!(s, "\nPer-symbol reconciliation (reconstructed vs legacy):");
    let _ = writeln!(
        s,
        "  {:<7} {:>14} {:>14}  {:>13} {:>13}  {:<10} {}",
        "SYMBOL",
        "SHARES(new)",
        "SHARES(leg)",
        "REALIZED(new)",
        "REALIZED(leg)",
        "SHARES?",
        "DOLLARS?"
    );
    for sym in &report.symbols {
        let shares = |m: pt_core::MicroShares| format!("{:.3}", m.0 as f64 / 1e6);
        let money = |c: Cents| format!("${:.2}", c.0 as f64 / 100.0);
        let share_v = match &sym.share_verdict {
            ShareVerdict::Matched => "ok".to_string(),
            ShareVerdict::SnappedClosingAdjustment { adjustment } => {
                format!("snapped {:+.3}", adjustment.0 as f64 / 1e6)
            }
            ShareVerdict::Flagged { residual } => {
                format!("FLAGGED {:+.3}", residual.0 as f64 / 1e6)
            }
        };
        let dollar_v = match &sym.verdict {
            Verdict::Matched => "ok".to_string(),
            Verdict::IntendedDivergence {
                predicted_cents,
                explanation,
            } => {
                format!("intended ({}: {})", money(*predicted_cents), explanation)
            }
            Verdict::OwnerAdjudicated {
                residual_cents,
                reason,
            } => {
                format!(
                    "adjudicated (residual {}: {reason})",
                    money(*residual_cents)
                )
            }
            Verdict::Unexplained { residual_cents, .. } => {
                format!("UNEXPLAINED (residual {})", money(*residual_cents))
            }
        };
        let _ = writeln!(
            s,
            "  {:<7} {:>14} {:>14}  {:>13} {:>13}  {:<10} {}",
            sym.symbol,
            shares(sym.reconstructed_shares),
            shares(sym.legacy_shares),
            money(sym.reconstructed_realized_cents),
            money(sym.legacy_realized_cents),
            share_v,
            dollar_v,
        );
    }

    let _ = writeln!(s);
    if report.commit_allowed && parsed.malformed.is_empty() {
        let _ = writeln!(
            s,
            "COMMIT GATE: clear — review the lines above, then `pt import --commit` to accept and write."
        );
    } else {
        let _ = writeln!(
            s,
            "COMMIT GATE: BLOCKED — resolve the FLAGGED/UNEXPLAINED/malformed lines above; nothing can be written."
        );
    }
    s
}

/// The full import flow: inputs → fetch (read-only) → parse → dry-run →
/// (optionally) accept + commit into the new workbook. Returns the rendered
/// report and the process exit code.
pub fn run_import_flow(
    settings: &config::Settings,
    commit: bool,
) -> Result<(String, i32), Box<dyn Error>> {
    let json = std::fs::read_to_string(OWNER_INPUTS_FILE).map_err(|e| {
        format!("{OWNER_INPUTS_FILE}: {e} (the owner-inputs file the import reads; see import-design.md)")
    })?;
    let inputs = parse_owner_inputs(&json)?;

    let (actions, sales, positions) =
        fetch_legacy_grids(&settings.credentials_path, &inputs.legacy_workbook_id)?;
    let mut parsed = parse_legacy_tabs(&actions, &sales, &positions, &inputs.platforms);
    let notes = apply_owner_mappings(&mut parsed, &inputs)?;
    let wb = assemble_workbook(&parsed, &inputs)?;

    let report = dry_run(&wb, &parsed.marks)?;
    let mut out = render_report(&report, &parsed, &notes);

    if commit {
        if !report.commit_allowed || !parsed.malformed.is_empty() {
            out.push_str("\n--commit refused: the gate is blocked.\n");
            return Ok((out, 2));
        }
        // `--commit` (run after reviewing a dry run) IS the owner's explicit
        // acceptance gesture; the typed token still re-checks the gate inside.
        let accepted = report.accept();
        let boot = runtime::Boot::from_settings(settings, "import");
        let mut store = boot.store(runtime::StoreLockAdapter::new(boot.lock()))?;
        let commit_report = import::commit(&mut store, &accepted)?;
        out.push_str(&format!(
            "\nCOMMITTED: {} events appended, {} already present (resume-skipped).\n",
            commit_report.appended.len(),
            commit_report.skipped.len(),
        ));
    }
    Ok((out, 0))
}
