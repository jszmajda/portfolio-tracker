//! `views` — the read-only rendering side of the TUI (prefix `TUI-VIEW`). Renders
//! `ledger-core`'s `Snapshot`, `reports`' analytics, and `tax`'s accruals/reserves
//! in the Ledger language, with filter / sort / drill-down / grouping. It mutates
//! nothing and computes nothing — every number comes from a kernel or `reports`;
//! where an action is appropriate it *launches* the matching `entry` flow, passing
//! only the selection's **identity**. (views-design.md)

use std::collections::BTreeMap;

use config::{Jurisdiction, TaxYear};
use ledger_core::Symbol;
use pt_core::Cents;
use reports::{compose, realized_history_by_year, Composition, PricedTotalState};
use tax::{Accrual, AccrualKey, AccrualState, AnnualRow};

use crate::port::ViewState;
use crate::theme::{self, LifecycleStop, StepperState};

// ===========================================================================
// The five screens (facets). (views-design.md → "Screens")
// ===========================================================================

/// Which read-only screen is showing. Drill-down pushes/pops these on the stack.
/// (views-design.md → "Screens" / "Navigation")
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Screen {
    /// Positions / Composition — the landing screen. (TUI-VIEW-POS-*)
    Positions,
    /// Open Lots — the drill-down target from a position. (TUI-VIEW-LOT-001)
    OpenLots,
    /// History / value-over-time. (TUI-VIEW-HIST-*)
    History,
    /// Tax & Reserves. (TUI-VIEW-TAX-*)
    TaxReserves,
    /// Realized — calendar realized-gain history. (TUI-VIEW-REAL-001)
    Realized,
}

impl Screen {
    /// The human screen title the design names — used in the panel title, the
    /// status line, and notices ("Open Lots", "Tax & Reserves") — never the
    /// Debug name. (views-design.md → "Screens")
    pub fn title(&self) -> &'static str {
        match self {
            Screen::Positions => "Positions",
            Screen::OpenLots => "Open Lots",
            Screen::History => "History",
            Screen::TaxReserves => "Tax & Reserves",
            Screen::Realized => "Realized",
        }
    }
}

/// How Positions/Composition is pivoted: by symbol (default) or by platform.
/// (TUI-VIEW-POS-002)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Grouping {
    #[default]
    BySymbol,
    ByPlatform,
}

/// A sortable column. A row sorts to the **tail** only when the *sort key itself*
/// is degraded for that row (in both directions). (TUI-VIEW-NAV-002)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortKey {
    #[default]
    Label,
    MarketValue,
    Unrealized,
    SharePct,
}

/// Ascending / descending; degraded keys always tail regardless. (TUI-VIEW-NAV-002)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}

/// A user filter by a single facet — **visibility only**: it changes which rows
/// show, never the share-% / totals denominator (those stay over the whole priced
/// portfolio). (TUI-VIEW-NAV-001)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Filter {
    /// No filter.
    #[default]
    None,
    /// By symbol substring.
    Symbol(String),
    /// By platform substring.
    Platform(String),
    /// By term (LT/ST) — open-lots / accruals.
    Term(tax::Term),
    /// By lifecycle state — accruals.
    State(String),
    /// By tax year — accruals / realized.
    TaxYear(i32),
}

impl Filter {
    /// A human filter-state label fragment (`symbol AMZN`).
    pub fn describe(&self) -> String {
        match self {
            Filter::None => "all".to_string(),
            Filter::Symbol(s) => format!("symbol {s}"),
            Filter::Platform(p) => format!("platform {p}"),
            Filter::Term(t) => format!("term {t:?}"),
            Filter::State(s) => format!("state {s}"),
            Filter::TaxYear(y) => format!("year {y}"),
        }
    }
}

/// The contextual scope set by drilling (a breadcrumb chip, cleared on ascend),
/// distinct from the per-screen retained user filter. (TUI-VIEW-NAV-003)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Scope {
    /// No contextual scope (top of a screen's stack).
    #[default]
    All,
    /// Scoped to a symbol (e.g. drilling Position → its Lots): breadcrumb `AMZN ›`.
    Symbol(Symbol),
    /// Scoped to a lot (a lot's realized history).
    Lot(String),
    /// Scoped to an accrual (an accrual → its sale → the realized gain).
    Accrual(AccrualKey),
}

impl Scope {
    /// The breadcrumb chip text (`AMZN ›`), empty for `All`. (TUI-VIEW-NAV-003)
    pub fn breadcrumb(&self) -> String {
        match self {
            Scope::All => String::new(),
            Scope::Symbol(s) => format!("{s} ›"),
            Scope::Lot(l) => format!("lot {l} ›"),
            Scope::Accrual(k) => format!("{}/{} ›", k.sale_id, k.lot_id),
        }
    }
}

/// The per-screen navigation state: retained filter + sort (kept across ascend),
/// plus the contextual scope and the focus identity. (TUI-VIEW-NAV-003/004)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NavState {
    pub screen: Screen,
    /// The user filter typed on THIS screen (retained across ascend). (TUI-VIEW-NAV-003)
    pub filter: Filter,
    pub sort_key: SortKey,
    pub sort_dir: SortDir,
    /// The contextual scope from drilling (breadcrumb, cleared on ascend).
    pub scope: Scope,
    /// The grouping toggle (Positions only). (TUI-VIEW-POS-002)
    pub grouping: Grouping,
    /// The focused row **identity** (symbol / lot id / accrual key), anchored
    /// across refresh / re-sort — NOT a row index. (TUI-VIEW-NAV-004)
    pub focus: RowIdentity,
    /// The stable scroll offset (rows). (TUI-VIEW-NAV-004)
    pub scroll: usize,
}

impl NavState {
    /// A fresh nav state for a screen (no filter/scope, default sort).
    pub fn new(screen: Screen) -> Self {
        NavState {
            screen,
            filter: Filter::None,
            sort_key: SortKey::default(),
            sort_dir: SortDir::default(),
            scope: Scope::All,
            grouping: Grouping::default(),
            focus: RowIdentity::None,
            scroll: 0,
        }
    }
}

/// A row's stable identity — focus and scroll anchor to this across a refresh or
/// re-sort, never to the row index. (TUI-VIEW-NAV-004)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum RowIdentity {
    #[default]
    None,
    Symbol(Symbol),
    Platform(String),
    Lot(String),
    Accrual(AccrualKey),
    Year(i32),
}

// ===========================================================================
// The filter-state header (views-design.md → "Navigation"; TUI-VIEW-NAV-001).
// `showing 3 of 12 · 18% of book` — so a non-summing %-column reads honestly.
// ===========================================================================

/// The filter-state header: shown / total rows and the visible subset's share of
/// the priced book (so a %-column that does not sum to 100 reads as a deliberate
/// subset, not a bug). Totals/% are over the WHOLE priced portfolio. (TUI-VIEW-NAV-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FilterHeader {
    pub shown: usize,
    pub total: usize,
    /// The visible rows' summed share of the priced book in `ppm`; `None` when the
    /// priced book is degenerate (shares are n/a). (TUI-VIEW-NAV-001/POS-003)
    pub shown_share_ppm: Option<i64>,
    /// `true` on the screen that HAS a share column (Positions): a degenerate
    /// priced book renders the explicit `n/a` (in `reports`' wording —
    /// TUI-VIEW-POS-003). On screens with no share column the `of book` segment
    /// is omitted entirely rather than rendering a misleading `n/a`.
    pub share_relevant: bool,
    pub filter: Filter,
    pub scope: Scope,
}

impl FilterHeader {
    /// The header line text (`showing 3 of 12 · 18% of book`). (TUI-VIEW-NAV-001)
    pub fn text(&self) -> String {
        let share_seg = match self.shown_share_ppm {
            Some(ppm) => format!(" · {} of book", theme::percent_ppm(ppm)),
            None if self.share_relevant => " · n/a of book".to_string(),
            None => String::new(),
        };
        let scope = self.scope.breadcrumb();
        let scope_prefix = if scope.is_empty() { String::new() } else { format!("{scope} ") };
        format!("{scope_prefix}showing {} of {}{share_seg}", self.shown, self.total)
    }
}

// ===========================================================================
// Positions / Composition (TUI-VIEW-POS-*).
// ===========================================================================

/// One rendered Positions/Composition row. Figures are `None` when degraded —
/// never a fabricated zero. The `degraded` flag and `share_na` distinguish the
/// `‡` marker from the `n/a` (priced-total ≤ 0) case. (TUI-VIEW-POS-001/003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PositionRow {
    pub identity: RowIdentity,
    pub label: String,
    /// The company display name from `config`'s symbol→display-name map — the
    /// ticker itself when unmapped, empty on a platform group row.
    /// (TUI-VIEW-POS-009)
    pub name: String,
    /// The share count (symbol pivot only; `None` for a platform group row,
    /// whose subtotal has no single share count). (TUI-VIEW-POS-001)
    pub shares: Option<pt_core::MicroShares>,
    /// The current mark price (symbol pivot only; `None` when unpriced or for a
    /// platform group row). (TUI-VIEW-POS-001)
    pub price: Option<Cents>,
    /// Market value at the current mark; `None` when degraded (unpriced).
    pub market_value: Option<Cents>,
    /// Total cost basis — reconstructable, so it renders even on a degraded row
    /// (from `reports`' composition). (TUI-VIEW-POS-004)
    pub basis: Cents,
    /// Pre-tax unrealized; `None` when degraded.
    pub unrealized_pretax: Option<Cents>,
    /// Pre-tax unrealized as % of basis in `ppm` (from `reports`); `None` renders
    /// the dash (degraded) or `n/a` (basis ≤ 0 — see `gain_pct_na`).
    /// (TUI-VIEW-POS-004)
    pub gain_pct_ppm: Option<i64>,
    /// `true` when the gain-% cell must render `n/a` (a priced row whose basis is
    /// ≤ 0), distinct from the degraded `‡`/dash. (TUI-VIEW-POS-004)
    pub gain_pct_na: bool,
    /// Post-tax `Net` `[est]` — the row's post-tax VALUE: market value less the
    /// estimated unrealized tax, computed as basis + `reports`' net-of-tax
    /// unrealized (one semantics with the band's `NET (POST-TAX)` and `summary`'s
    /// headline Net). Zero estimated tax (price at or below basis) makes it equal
    /// the market value. `None` when degraded. (TUI-VIEW-POS-012)
    pub net_post_tax: Option<Cents>,
    /// The day change in $ — `reports`' per-symbol value delta between the latest
    /// prior captured History point (zero-key garbage never selected) and the
    /// current values; `None` (the dash) until a prior capture exists / when the
    /// symbol has no priced value at either endpoint / when degraded / on a
    /// platform group row. (TUI-VIEW-POS-005)
    pub day_delta: Option<Cents>,
    /// The day change in % (`ppm`, relative to the prior value, from `reports`);
    /// `None` when the $ delta is absent or the prior value is no base.
    /// (TUI-VIEW-POS-005)
    pub day_delta_ppm: Option<i64>,
    /// Share % of the priced portfolio in `ppm`; `None` when degraded.
    pub share_ppm: Option<i64>,
    /// The trend strip's captured ramp — the symbol's value trend over the most
    /// recent strip-width captured days; the renderer dot-pads it to the fixed
    /// strip. Empty when nothing is captured yet. (TUI-VIEW-POS-013)
    pub sparkline: String,
    /// The sparkline's net-direction tint role (gain/loss/flat). (tui-design.md →
    /// "Sparklines")
    pub sparkline_role: theme::Role,
    /// `true` when degraded (unpriced or no tax estimate) — the `‡` marker.
    pub degraded: bool,
    /// `true` when the share column must render `n/a` (priced total ≤ 0), in
    /// `reports`' wording — distinct from the degraded `‡`. (TUI-VIEW-POS-003)
    pub share_na: bool,
    /// `true` when degraded specifically by a missing tax estimate (the word
    /// "degraded", not "unpriced"). (tui-design.md → "Degradation")
    pub tax_degraded: bool,
}

/// The composition caveat that rides the Positions header: the degraded-symbol
/// count + the priced-basis fraction. (TUI-VIEW-POS-003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CompositionCaveat {
    pub degraded_count: usize,
    /// The priced-coverage fraction in `ppm`; `None` when total basis is 0.
    pub priced_coverage_ppm: Option<i64>,
    /// `true` when the priced total ≤ 0 (positions exist but all unpriced, or none)
    /// — the share column is `n/a`. (TUI-VIEW-POS-003)
    pub shares_na: bool,
    /// Distinguishes "no positions" from "positions exist but all unpriced".
    pub no_positions: bool,
}

impl CompositionCaveat {
    /// The caveat line text for the header band. (TUI-VIEW-POS-003)
    pub fn text(&self) -> String {
        let cov = match self.priced_coverage_ppm {
            Some(ppm) => theme::percent_ppm(ppm),
            None => "n/a".to_string(),
        };
        format!("{} degraded · priced basis {}", self.degraded_count, cov)
    }
}

/// Build the per-symbol or per-platform Positions/Composition rows from the view
/// state, with each row's share % of the priced portfolio + a sparkline. Figures
/// degrade (never zeroed); the share column is `n/a` when the priced total ≤ 0.
/// (TUI-VIEW-POS-001/002/003)
pub fn composition(view: &ViewState) -> Composition {
    compose(&view.snapshot, &view.marks, &view.estimates)
}

/// The composition caveat for the header. (TUI-VIEW-POS-003)
pub fn composition_caveat(comp: &Composition) -> CompositionCaveat {
    let (shares_na, no_positions) = match comp.priced_total_state {
        PricedTotalState::Positive => (false, false),
        PricedTotalState::NoPositions => (true, true),
        PricedTotalState::PositionsExistButUnpriced => (true, false),
    };
    CompositionCaveat {
        degraded_count: comp.degraded_count,
        priced_coverage_ppm: comp.priced_coverage_ppm.map(|p| p.0),
        shares_na,
        no_positions,
    }
}

/// Render the Positions/Composition rows (the visible, filtered, sorted set), the
/// caveat, and the filter-state header. `views` filter is **visibility-only**:
/// totals + share % stay over the whole priced portfolio; the header states the
/// subset. (TUI-VIEW-POS-001/002/003, TUI-VIEW-NAV-001/002)
pub fn positions_rows(view: &ViewState, nav: &NavState) -> (Vec<PositionRow>, CompositionCaveat, FilterHeader) {
    let comp = composition(view);
    let caveat = composition_caveat(&comp);
    let day_deltas = per_symbol_day_deltas(view);

    // Per-symbol value trend sparklines from the History series.
    let trends = per_symbol_trends(view);

    let src = match nav.grouping {
        Grouping::BySymbol => &comp.by_symbol,
        Grouping::ByPlatform => &comp.by_platform,
    };

    let mut all: Vec<PositionRow> = src
        .iter()
        .map(|r| {
            let degraded = r.degraded.is_some();
            let tax_degraded = matches!(r.degraded, Some(reports::DegradeCause::NoTaxEstimate));
            let identity = match nav.grouping {
                Grouping::BySymbol => RowIdentity::Symbol(r.label.clone()),
                Grouping::ByPlatform => RowIdentity::Platform(r.label.clone()),
            };
            // Shares + the mark price render on the symbol pivot only (a platform
            // group's subtotal has no single share count / price). Both come from
            // the kernels — the Snapshot's position qty and the cycle's mark —
            // never computed here. (TUI-VIEW-POS-001)
            let (shares, price) = match nav.grouping {
                Grouping::BySymbol => (
                    view.snapshot
                        .positions
                        .get(&r.label)
                        .map(|p| p.total_qty)
                        .filter(|q| q.0 != 0),
                    view.marks.get(&r.label).map(|m| m.price_cents),
                ),
                Grouping::ByPlatform => (None, None),
            };
            // The day change renders on the symbol pivot only (a platform group
            // has no single per-symbol series). (TUI-VIEW-POS-005)
            let day_change = if matches!(nav.grouping, Grouping::BySymbol) {
                day_deltas.get(&r.label)
            } else {
                None
            };
            // The company-name cell: the display-name map on the symbol pivot
            // (ticker fallback for an unmapped symbol); blank on a platform
            // group row — the label already names the platform.
            // (TUI-VIEW-POS-009)
            let name = match nav.grouping {
                Grouping::BySymbol => view.display_names.resolve(&r.label),
                Grouping::ByPlatform => String::new(),
            };
            PositionRow {
                identity,
                label: r.label.clone(),
                name,
                shares,
                price,
                market_value: r.market_value_cents,
                basis: r.total_basis_cents,
                unrealized_pretax: r.unrealized_pretax_cents,
                gain_pct_ppm: r.unrealized_pct_of_basis_ppm.map(|p| p.0),
                // n/a (not the degraded dash) when the row is priced but its
                // basis is ≤ 0 — `reports`' n/a case. (TUI-VIEW-POS-004)
                gain_pct_na: !degraded
                    && r.unrealized_pct_of_basis_ppm.is_none()
                    && r.total_basis_cents.0 <= 0,
                // Net = the post-tax VALUE (market value − estimated unrealized
                // tax) = basis + reports' net-of-tax unrealized — the same
                // derivation as the summary band's NET (POST-TAX), so one word
                // means one thing. A clamped-to-zero estimate (price at or below
                // basis) leaves Net equal to the market value — never a re-tax
                // of an already-W-2-taxed vest FMV. (TUI-VIEW-POS-012)
                // @spec TUI-VIEW-POS-012
                net_post_tax: r
                    .unrealized_net_of_tax_cents
                    .map(|n| Cents(r.total_basis_cents.0 + n.0)),
                day_delta: day_change.and_then(|d| d.delta_value_cents),
                day_delta_ppm: day_change.and_then(|d| d.delta_pct_ppm).map(|p| p.0),
                share_ppm: r.share_ppm.map(|p| p.0),
                sparkline: trends.get(&r.label).map(|(s, _)| s.clone()).unwrap_or_default(),
                sparkline_role: trends
                    .get(&r.label)
                    .map(|(_, role)| *role)
                    .unwrap_or(theme::Role::Flat),
                degraded,
                share_na: caveat.shares_na && !degraded,
                tax_degraded,
            }
        })
        .collect();

    let total = all.len();
    sort_position_rows(&mut all, nav.sort_key, nav.sort_dir);

    // Apply the user filter (visibility-only) — and the contextual scope. The
    // filter is grouping-aware: each facet applies only to the dimension the active
    // grouping exposes, so it hides whole group rows whose subtotal STAYS WHOLE
    // (the grouped rows are aggregate subtotals), never garbage-matching a facet
    // against the wrong label. (TUI-VIEW-POS-002, TUI-VIEW-NAV-001)
    let visible: Vec<PositionRow> = all
        .into_iter()
        .filter(|r| position_visible(r, &nav.filter, &nav.scope, nav.grouping))
        .collect();

    let shown = visible.len();
    let shown_share_ppm = if caveat.shares_na {
        None
    } else {
        Some(visible.iter().filter_map(|r| r.share_ppm).sum())
    };
    let header = FilterHeader {
        shown,
        total,
        shown_share_ppm,
        share_relevant: true,
        filter: nav.filter.clone(),
        scope: nav.scope.clone(),
    };
    (visible, caveat, header)
}

/// Whether a position row is visible under the filter + scope (visibility-only).
/// The match is **grouping-aware**: a facet applies only to the dimension the
/// active grouping exposes in the row label (the row IS an aggregate subtotal under
/// `ByPlatform`). A facet inapplicable to the active pivot — a symbol filter while
/// grouped by platform, a platform filter while grouped by symbol — leaves the
/// group rows visible (the subtotal stays whole), never matching the facet against
/// the wrong label. (TUI-VIEW-POS-002, TUI-VIEW-NAV-001)
fn position_visible(row: &PositionRow, filter: &Filter, scope: &Scope, grouping: Grouping) -> bool {
    let scope_ok = match scope {
        Scope::Symbol(s) => row.label.eq_ignore_ascii_case(s),
        _ => true,
    };
    if !scope_ok {
        return false;
    }
    match (grouping, filter) {
        (_, Filter::None) => true,
        // By symbol: the row label is a symbol — only a symbol filter narrows it.
        (Grouping::BySymbol, Filter::Symbol(s)) => row.label.to_uppercase().contains(&s.to_uppercase()),
        // By platform: the row label is a platform — only a platform filter narrows
        // it; the whole-group subtotal stays whole either way.
        (Grouping::ByPlatform, Filter::Platform(p)) => row.label.to_uppercase().contains(&p.to_uppercase()),
        // Any facet inapplicable to the active pivot keeps the group visible (the
        // subtotal stays whole rather than rebasing or garbage-matching).
        _ => true,
    }
}

/// Sort position rows: a row with a **degraded sort key** always sorts to the tail
/// (in both directions); otherwise by the key with a stable label tie-break.
/// (TUI-VIEW-NAV-002)
pub fn sort_position_rows(rows: &mut [PositionRow], key: SortKey, dir: SortDir) {
    rows.sort_by(|a, b| {
        let (ka, da) = position_sort_key(a, key);
        let (kb, db) = position_sort_key(b, key);
        // Degraded-on-the-sort-key rows always tail, regardless of direction.
        match (da, db) {
            (true, false) => return std::cmp::Ordering::Greater,
            (false, true) => return std::cmp::Ordering::Less,
            _ => {}
        }
        let ord = ka.cmp(&kb);
        let ord = match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        };
        // Stable tie-break on label (ascending, direction-independent).
        ord.then_with(|| a.label.cmp(&b.label))
    });
}

/// The comparable sort value for a row on a key, plus whether the key is degraded
/// for that row (degraded → tail). For `Label` the key is never degraded.
fn position_sort_key(row: &PositionRow, key: SortKey) -> (i64, bool) {
    match key {
        SortKey::Label => (0, false),
        SortKey::MarketValue => match row.market_value {
            Some(c) => (c.0, false),
            None => (0, true),
        },
        SortKey::Unrealized => match row.unrealized_pretax {
            Some(c) => (c.0, false),
            None => (0, true),
        },
        SortKey::SharePct => match row.share_ppm {
            Some(p) => (p, false),
            None => (0, true),
        },
    }
}

/// Per-symbol day change ($ and %): `reports`' per-symbol value delta between
/// the **latest prior captured** History point (strictly before today's
/// trading-day key; a zero-key point is garbage — *no priced day* — and is
/// never selected) and the **current** values (the series point `reports`
/// builds over the live snapshot/marks — today's actual move, not the last two
/// captures). Honesty is **per symbol**: `reports` yields no delta for a symbol
/// lacking a priced value at either endpoint, so an incomplete prior degrades
/// only the rows it actually lacks, never the whole column. Empty until a prior
/// capture exists or when no symbol is priced. `views` computes nothing — both
/// endpoints and the delta/percent come from `reports`.
/// (TUI-VIEW-POS-001, TUI-VIEW-POS-005)
// @spec TUI-VIEW-POS-005
fn per_symbol_day_deltas(view: &ViewState) -> BTreeMap<Symbol, reports::PerSymbolValueDelta> {
    let Some(today) = view.trading_day_key else {
        return BTreeMap::new();
    };
    let Some(prior) = view
        .trading_day_calendar
        .iter()
        .rev()
        .filter(|k| **k < today && k.0 .0 != 0)
        .find_map(|k| view.history_points.get(k))
    else {
        return BTreeMap::new();
    };
    let current = reports::build_series_point(
        &view.snapshot,
        &view.marks,
        &view.estimates,
        today,
        0,
        today.0,
    );
    reports::per_symbol_value_delta(prior, &current)
        .into_iter()
        .map(|d| (d.symbol.clone(), d))
        .collect()
}

/// Per-symbol value points across the trading-day calendar (split-neutral values),
/// for sparklines + the day-delta. Sourced from the captured History points'
/// `per_symbol_value_cents`. (TUI-VIEW-POS-001 / TUI-VIEW-HIST-001)
fn per_symbol_value_points(view: &ViewState) -> BTreeMap<Symbol, Vec<i64>> {
    let mut out: BTreeMap<Symbol, Vec<i64>> = BTreeMap::new();
    for key in &view.trading_day_calendar {
        if let Some(pt) = view.history_points.get(key) {
            for (sym, v) in &pt.per_symbol_value_cents {
                out.entry(sym.clone()).or_default().push(v.0);
            }
        }
    }
    out
}

/// Per-symbol trend-strip ramps + their net-direction tint role (the gain/loss
/// tint the spans seam renders, with the latest captured cell brightened by the
/// renderer). The series is **windowed to the most recent strip-width captured
/// days** — the renderer right-pads the remainder with dim dots, so the strip is
/// a fixed-width timeline filling in. (TUI-VIEW-POS-013)
// @spec TUI-VIEW-POS-013
fn per_symbol_trends(view: &ViewState) -> BTreeMap<Symbol, (String, theme::Role)> {
    per_symbol_value_points(view)
        .into_iter()
        .map(|(s, pts)| {
            let window = &pts[pts.len().saturating_sub(theme::SPARK_STRIP_W)..];
            let role = theme::series_role(window);
            (s, (theme::sparkline(window), role))
        })
        .collect()
}

// ===========================================================================
// Open Lots (TUI-VIEW-LOT-001) — the drill-down target from a position.
// ===========================================================================

/// One Open Lots row. (TUI-VIEW-LOT-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LotRow {
    pub identity: RowIdentity,
    pub lot_id: String,
    pub symbol: Symbol,
    /// The company display name (ticker fallback). (TUI-VIEW-LOT-003)
    pub name: String,
    pub acquire_date: pt_core::Date,
    pub source: ledger_core::LotSource,
    pub term: tax::Term,
    pub remaining_qty: pt_core::MicroShares,
    pub basis: Cents,
    pub basis_per_share: Cents,
    pub platform: String,
    pub tracking_code: Option<String>,
}

/// Render the Open Lots rows for the view, scoped to a symbol when drilled in, and
/// filtered (visibility-only). (TUI-VIEW-LOT-001, TUI-VIEW-NAV-001/003)
pub fn lot_rows(view: &ViewState, nav: &NavState) -> (Vec<LotRow>, FilterHeader) {
    let today = view.trading_day_key.map(|k| k.0).unwrap_or_default();
    let mut all: Vec<LotRow> = view
        .snapshot
        .open_lots
        .iter()
        .map(|ol| {
            let lot = &ol.lot;
            let term = tax::classify_term(lot.acquire_date, today);
            // basis/share = remaining_basis ÷ remaining_qty (whole-share basis).
            let bps = if lot.remaining_qty.0 != 0 {
                Cents(pt_core::round_half_to_even(
                    (lot.remaining_basis_cents.0 as i128) * (pt_core::SHARE_SCALE as i128),
                    lot.remaining_qty.0 as i128,
                ) as i64)
            } else {
                Cents(0)
            };
            LotRow {
                identity: RowIdentity::Lot(lot.id.clone()),
                lot_id: lot.id.clone(),
                // The company-name cell (ticker fallback). (TUI-VIEW-LOT-003)
                name: view.display_names.resolve(&lot.symbol),
                symbol: lot.symbol.clone(),
                acquire_date: lot.acquire_date,
                source: lot.source,
                term,
                remaining_qty: lot.remaining_qty,
                basis: lot.remaining_basis_cents,
                basis_per_share: bps,
                platform: lot.platform.clone(),
                tracking_code: lot.tracking_code.clone(),
            }
        })
        .collect();
    let total = all.len();
    all.sort_by(|a, b| a.lot_id.cmp(&b.lot_id));
    let visible: Vec<LotRow> = all
        .into_iter()
        .filter(|r| lot_visible(r, &nav.filter, &nav.scope))
        .collect();
    let header = FilterHeader {
        shown: visible.len(),
        total,
        shown_share_ppm: None,
        share_relevant: false,
        filter: nav.filter.clone(),
        scope: nav.scope.clone(),
    };
    (visible, header)
}

fn lot_visible(row: &LotRow, filter: &Filter, scope: &Scope) -> bool {
    let scope_ok = match scope {
        Scope::Symbol(s) => row.symbol.eq_ignore_ascii_case(s),
        Scope::Lot(l) => &row.lot_id == l,
        _ => true,
    };
    if !scope_ok {
        return false;
    }
    match filter {
        Filter::None => true,
        Filter::Symbol(s) => row.symbol.to_uppercase().contains(&s.to_uppercase()),
        Filter::Platform(p) => row.platform.to_uppercase().contains(&p.to_uppercase()),
        Filter::Term(t) => row.term == *t,
        _ => true,
    }
}

// ===========================================================================
// History / value-over-time (TUI-VIEW-HIST-*).
// ===========================================================================

/// A History cell: a captured value, an explicit **gap** (blank cell, never
/// interpolated), or an **incomplete** day (a `‡`-tinted cell). (TUI-VIEW-HIST-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HistoryCell {
    /// A complete captured trading-day point.
    Value { key: pt_core::Date, value: Cents },
    /// An in-band gap — a trading day with no capture: a blank cell, never a
    /// fabricated flat value. (TUI-VIEW-HIST-001)
    Gap { key: pt_core::Date },
    /// An incomplete capture (a degraded symbol that day): a `‡`-tinted cell.
    /// (TUI-VIEW-HIST-001)
    Incomplete { key: pt_core::Date, value: Cents },
}

/// The rendered History: the cell series (split-neutral total value) plus the
/// degenerate-state classification. (TUI-VIEW-HIST-001/002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HistoryView {
    pub cells: Vec<HistoryCell>,
    /// A direction-tinted sparkline over the complete points (suppressed when the
    /// series is all-incomplete). (TUI-VIEW-HIST-001/002)
    pub sparkline: String,
    pub state: HistoryState,
}

/// The degenerate History states rendered explicitly. (TUI-VIEW-HIST-002)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HistoryState {
    /// A normal multi-point series with a chart + table.
    Normal,
    /// 0 points — `no history yet — captures begin on the first priced run`.
    NoHistory,
    /// 1 point — a single tick, no trend.
    SinglePoint,
    /// Every captured day is incomplete — the chart is suppressed, the table is
    /// flagged. (TUI-VIEW-HIST-002)
    AllIncomplete,
}

impl HistoryState {
    /// The explicit message for a degenerate state (empty for `Normal`).
    pub fn message(&self) -> String {
        match self {
            HistoryState::Normal => String::new(),
            HistoryState::NoHistory => {
                "no history yet — captures begin on the first priced run".to_string()
            }
            HistoryState::SinglePoint => "single point — no trend yet".to_string(),
            HistoryState::AllIncomplete => {
                "all captures incomplete — chart suppressed".to_string()
            }
        }
    }
}

/// Render the value-over-time History across the trading-day calendar: captured
/// points, explicit gaps for uncaptured days, `‡`-tinted incomplete days; with the
/// degenerate states classified. (TUI-VIEW-HIST-001/002)
pub fn history_view(view: &ViewState, _nav: &NavState) -> HistoryView {
    let mut cells: Vec<HistoryCell> = Vec::new();
    let mut complete_points: Vec<i64> = Vec::new();
    let mut captured = 0usize;
    let mut incomplete_count = 0usize;

    for key in &view.trading_day_calendar {
        match view.history_points.get(key) {
            None => cells.push(HistoryCell::Gap { key: key.0 }),
            Some(pt) => {
                captured += 1;
                if pt.incomplete {
                    incomplete_count += 1;
                    cells.push(HistoryCell::Incomplete {
                        key: key.0,
                        value: pt.total_market_value_cents,
                    });
                } else {
                    complete_points.push(pt.total_market_value_cents.0);
                    cells.push(HistoryCell::Value {
                        key: key.0,
                        value: pt.total_market_value_cents,
                    });
                }
            }
        }
    }

    let state = if captured == 0 {
        HistoryState::NoHistory
    } else if incomplete_count == captured {
        HistoryState::AllIncomplete
    } else if captured == 1 {
        HistoryState::SinglePoint
    } else {
        HistoryState::Normal
    };

    let sparkline = if matches!(state, HistoryState::AllIncomplete | HistoryState::NoHistory) {
        String::new()
    } else {
        theme::sparkline(&complete_points)
    };

    HistoryView { cells, sparkline, state }
}

// ===========================================================================
// Tax & Reserves (TUI-VIEW-TAX-*).
// ===========================================================================

/// One Tax & Reserves accrual row, with its lifecycle stepper. (TUI-VIEW-TAX-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AccrualRow {
    pub identity: RowIdentity,
    pub key: AccrualKey,
    pub applied: Option<Cents>,
    pub stepper: StepperState,
    /// `true` when this accrual is selectable for an `entry` action (NOT
    /// auto-settled or orphaned). (tui-design.md → "Auto-settled & orphan accruals")
    pub actionable: bool,
}

/// A reserve-balance summary line for a `(jurisdiction, tax_year)`. (TUI-VIEW-TAX-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReserveLine {
    pub jurisdiction: Jurisdiction,
    pub tax_year: TaxYear,
    pub accrued: Cents,
    pub moved: Cents,
    pub paid: Cents,
    pub outstanding: Cents,
    pub shortfall: Cents,
}

/// The provenance caveat the Tax & Reserves screen carries: estimated tax on
/// realized gains, by jurisdiction — NOT the gains themselves (see Realized).
/// (TUI-VIEW-TAX-001)
pub const TAX_PROVENANCE: &str =
    "estimated tax on realized gains, by jurisdiction — not the gains themselves (see Realized)";

/// Whether an accrual is orphaned (its sale was reversed — `TAX-VERIF-007`). An
/// orphan is flagged via the reserve's `has_backless_entry`, but at the per-accrual
/// level we detect it as: the accrual key appears in `tax`'s orphan warnings.
fn accrual_stepper(acc: &Accrual, orphans: &std::collections::BTreeSet<AccrualKey>) -> StepperState {
    if orphans.contains(&acc.key) {
        return StepperState::Orphaned;
    }
    if acc.de_minimis {
        return StepperState::AutoSettled;
    }
    let stop = match &acc.state {
        AccrualState::Accrued => LifecycleStop::Accrued,
        AccrualState::Allocated { .. } => LifecycleStop::Allocated,
        AccrualState::Moved { .. } => LifecycleStop::Moved,
        AccrualState::Paid { .. } => LifecycleStop::Paid,
    };
    StepperState::Lifecycle(stop)
}

/// Render the Tax & Reserves accrual rows grouped by `(jurisdiction, tax_year)`,
/// with the lifecycle stepper, plus the reserve balance lines. (TUI-VIEW-TAX-001)
pub fn tax_rows(view: &ViewState, nav: &NavState) -> (Vec<AccrualRow>, Vec<ReserveLine>, FilterHeader) {
    let orphans = orphan_keys(view);
    let mut all: Vec<AccrualRow> = view
        .accruals
        .iter()
        .map(|acc| {
            let stepper = accrual_stepper(acc, &orphans);
            let actionable = !matches!(stepper, StepperState::AutoSettled);
            AccrualRow {
                identity: RowIdentity::Accrual(acc.key.clone()),
                key: acc.key.clone(),
                applied: acc.applied_cents,
                stepper,
                actionable,
            }
        })
        .collect();
    // Group ordering: by (jurisdiction, tax_year), then sale/lot id.
    all.sort_by(|a, b| {
        (a.key.jurisdiction.clone(), a.key.tax_year, a.key.sale_id.clone(), a.key.lot_id.clone())
            .cmp(&(
                b.key.jurisdiction.clone(),
                b.key.tax_year,
                b.key.sale_id.clone(),
                b.key.lot_id.clone(),
            ))
    });
    let total = all.len();
    let visible: Vec<AccrualRow> = all
        .into_iter()
        .filter(|r| accrual_visible(r, &nav.filter, &nav.scope))
        .collect();

    let reserves = reserve_lines(view);

    let header = FilterHeader {
        shown: visible.len(),
        total,
        shown_share_ppm: None,
        share_relevant: false,
        filter: nav.filter.clone(),
        scope: nav.scope.clone(),
    };
    (visible, reserves, header)
}

fn accrual_visible(row: &AccrualRow, filter: &Filter, scope: &Scope) -> bool {
    let scope_ok = match scope {
        Scope::Accrual(k) => &row.key == k,
        _ => true,
    };
    if !scope_ok {
        return false;
    }
    match filter {
        Filter::None => true,
        Filter::TaxYear(y) => row.key.tax_year.0 == *y,
        Filter::State(s) => match &row.key.jurisdiction {
            Jurisdiction::State(st) => st.eq_ignore_ascii_case(s),
            Jurisdiction::Federal => s.eq_ignore_ascii_case("federal"),
        },
        _ => true,
    }
}

/// The reserve balance lines from `tax`'s annual rows. (TUI-VIEW-TAX-001)
pub fn reserve_lines(view: &ViewState) -> Vec<ReserveLine> {
    view.annual_rows
        .iter()
        .map(|r: &AnnualRow| ReserveLine {
            jurisdiction: r.jurisdiction.clone(),
            tax_year: r.tax_year,
            accrued: r.accrued_cents,
            moved: r.moved_cents,
            paid: r.paid_cents,
            outstanding: r.outstanding_cents,
            shortfall: r.shortfall_cents,
        })
        .collect()
}

/// The orphan accrual keys (sales reversed after Move/Pay). Computed via `tax`'s
/// `compute_accruals_with_warnings`. (TUI-VIEW-TAX-001, TAX-VERIF-007)
fn orphan_keys(view: &ViewState) -> std::collections::BTreeSet<AccrualKey> {
    view.orphan_warnings.iter().map(|w| w.accrual_key.clone()).collect()
}

// ===========================================================================
// Realized — calendar realized-gain history (TUI-VIEW-REAL-001).
// ===========================================================================

/// One Realized history row (a calendar period + its totals). (TUI-VIEW-REAL-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RealizedRow {
    pub identity: RowIdentity,
    pub label: String,
    pub year: i32,
    pub proceeds: Cents,
    pub basis: Cents,
    pub gain: Cents,
}

/// The provenance caveat the Realized screen carries: raw realized gains,
/// calendar-grouped — NOT tax (see Tax & Reserves). (TUI-VIEW-REAL-001)
pub const REALIZED_PROVENANCE: &str =
    "raw realized gains, calendar-grouped — not tax (see Tax & Reserves)";

/// Render Realized rows by calendar year (labelled "calendar"), filterable by tax
/// year (visibility-only). (TUI-VIEW-REAL-001, TUI-VIEW-NAV-001)
pub fn realized_rows(view: &ViewState, nav: &NavState) -> (Vec<RealizedRow>, FilterHeader) {
    let mut all: Vec<RealizedRow> = realized_history_by_year(&view.snapshot)
        .into_iter()
        .map(|r| {
            let year = match r.period {
                reports::CalendarPeriod::Year(y) => y,
                reports::CalendarPeriod::Range { .. } => 0,
            };
            RealizedRow {
                identity: RowIdentity::Year(year),
                label: r.label,
                year,
                proceeds: r.proceeds_cents,
                basis: r.basis_cents,
                gain: r.gain_cents,
            }
        })
        .collect();
    let total = all.len();
    all.sort_by(|a, b| a.year.cmp(&b.year));
    let visible: Vec<RealizedRow> = all
        .into_iter()
        .filter(|r| match &nav.filter {
            Filter::TaxYear(y) => r.year == *y,
            _ => true,
        })
        .collect();
    let header = FilterHeader {
        shown: visible.len(),
        total,
        shown_share_ppm: None,
        share_relevant: false,
        filter: nav.filter.clone(),
        scope: nav.scope.clone(),
    };
    (visible, header)
}

// ===========================================================================
// Empty states (views-design.md → "Empty States"; TUI-VIEW-NAV-006). Six calm,
// distinct empty states, none using the `✗` integrity treatment.
// ===========================================================================

/// A calm empty state, distinct from the integrity-error block. (TUI-VIEW-NAV-006)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EmptyState {
    /// No positions (fresh / pre-import — the whole-book state).
    NoPositions,
    /// No open lots — every lot has been fully sold (distinct from NoPositions,
    /// the whole-book pre-import state). (TUI-VIEW-NAV-006)
    NoOpenLots,
    /// No tax accruals yet.
    NoAccruals,
    /// No history yet.
    NoHistory,
    /// No realized gains yet — nothing has been sold (distinct from NoPositions,
    /// which the Realized screen must never reuse). (TUI-VIEW-NAV-006)
    NoRealizedGains,
    /// A filter matched zero rows.
    FilterMatchedZero,
}

impl EmptyState {
    /// The calm message (never the `✗` integrity treatment). (TUI-VIEW-NAV-006)
    pub fn message(&self) -> String {
        match self {
            EmptyState::NoPositions => {
                "no positions yet — add activity in Entry, or run import".to_string()
            }
            EmptyState::NoOpenLots => {
                "no open lots — every lot has been fully sold".to_string()
            }
            EmptyState::NoAccruals => {
                "no tax accruals yet — they appear when you record a sale".to_string()
            }
            EmptyState::NoHistory => {
                "no history yet — captures begin on the first priced run".to_string()
            }
            EmptyState::NoRealizedGains => {
                "no realized gains yet — they appear when you sell a lot".to_string()
            }
            EmptyState::FilterMatchedZero => "no rows match — [clear filter]".to_string(),
        }
    }
}

/// Classify the empty state for a screen given its visible-row count, whether a
/// filter is active, and whether the whole book is empty (pre-import), or `None`
/// when there are rows. A filter that matched zero is distinct from a screen with
/// no underlying data; an Open Lots screen whose book has positions but every lot
/// closed is distinct from the whole-book pre-import state. (TUI-VIEW-NAV-006)
pub fn empty_state(
    screen: &Screen,
    total: usize,
    shown: usize,
    filter_active: bool,
    book_empty: bool,
) -> Option<EmptyState> {
    if shown > 0 {
        return None;
    }
    if filter_active && total > 0 {
        return Some(EmptyState::FilterMatchedZero);
    }
    Some(match screen {
        Screen::Positions => EmptyState::NoPositions,
        // The whole-book pre-import state shows "no positions"; a book whose every
        // lot has been fully sold shows the distinct no-open-lots state.
        Screen::OpenLots => {
            if book_empty {
                EmptyState::NoPositions
            } else {
                EmptyState::NoOpenLots
            }
        }
        Screen::TaxReserves => EmptyState::NoAccruals,
        Screen::History => EmptyState::NoHistory,
        Screen::Realized => EmptyState::NoRealizedGains,
    })
}

// ===========================================================================
// Focus anchoring (TUI-VIEW-NAV-004). A refresh / re-sort keeps the selection on
// the same row IDENTITY (not index); a vanished identity falls to the nearest row.
// ===========================================================================

/// The ordered, visible row identities for a screen under its nav state — the list
/// focus re-anchors against on a refresh / re-sort. Mirrors each screen's row
/// ordering + filtering so the re-anchor sees exactly the rows the owner sees.
/// (TUI-VIEW-NAV-004)
pub fn screen_identities(view: &ViewState, nav: &NavState) -> Vec<RowIdentity> {
    match nav.screen {
        Screen::Positions => positions_rows(view, nav).0.iter().map(|r| r.identity.clone()).collect(),
        Screen::OpenLots => lot_rows(view, nav).0.iter().map(|r| r.identity.clone()).collect(),
        Screen::TaxReserves => tax_rows(view, nav).0.iter().map(|r| r.identity.clone()).collect(),
        Screen::Realized => realized_rows(view, nav).0.iter().map(|r| r.identity.clone()).collect(),
        // History has no per-row identity focus (it is a chart + table of days).
        Screen::History => Vec::new(),
    }
}

/// Re-anchor focus to the same row **identity** after a refresh / re-sort over a
/// new ordered identity list. If the focused identity survived, focus stays on it
/// (its new index returned); if it vanished, focus falls to the **nearest** row
/// (the old index clamped into the new list). Returns the new focus identity +
/// index. (TUI-VIEW-NAV-004)
pub fn reanchor_focus(
    focus: &RowIdentity,
    old_index: usize,
    new_order: &[RowIdentity],
) -> (RowIdentity, usize) {
    if new_order.is_empty() {
        return (RowIdentity::None, 0);
    }
    if let Some(i) = new_order.iter().position(|id| id == focus) {
        return (focus.clone(), i);
    }
    // The focused identity vanished: fall to the nearest row (clamp the old index).
    let i = old_index.min(new_order.len() - 1);
    (new_order[i].clone(), i)
}

// ===========================================================================
// Drill-down (TUI-VIEW-NAV-003). enter descends, esc ascends. Drilling sets a
// contextual scope (breadcrumb, cleared on ascend); the per-screen user filter +
// sort are retained across ascend, composing on top of the scope.
// ===========================================================================

/// The drill target a row descends into, with the contextual scope it sets.
/// Position → its Lots; an accrual → its sale → the realized gain. (TUI-VIEW-NAV-003)
pub fn drill_target(from: &Screen, identity: &RowIdentity) -> Option<(Screen, Scope)> {
    match (from, identity) {
        (Screen::Positions, RowIdentity::Symbol(s)) => {
            Some((Screen::OpenLots, Scope::Symbol(s.clone())))
        }
        (Screen::OpenLots, RowIdentity::Lot(l)) => {
            Some((Screen::Realized, Scope::Lot(l.clone())))
        }
        (Screen::TaxReserves, RowIdentity::Accrual(k)) => {
            Some((Screen::Realized, Scope::Accrual(k.clone())))
        }
        _ => None,
    }
}

// ===========================================================================
// Refresh re-resolution (TUI-VIEW-NAV-005). On a refresh / a concurrent entry
// append, each stacked frame's anchor identity is re-resolved; a dangling frame
// (a drilled-into lot/accrual that no longer exists or has closed) is replaced
// with a calm returning-to-parent notice, not a stale/empty render.
// ===========================================================================

/// Whether a stacked frame's contextual scope still resolves against the current
/// view (so its drilled-into anchor still exists). (TUI-VIEW-NAV-005)
pub fn scope_resolves(scope: &Scope, view: &ViewState) -> bool {
    match scope {
        Scope::All => true,
        Scope::Symbol(s) => view.snapshot.positions.get(s).map(|p| p.total_qty.0 != 0).unwrap_or(false),
        Scope::Lot(l) => view.snapshot.open_lots.iter().any(|ol| &ol.lot.id == l),
        Scope::Accrual(k) => view.accruals.iter().any(|a| &a.key == k),
    }
}

/// The calm notice replacing a dangling drilled frame on refresh, naming the
/// parent it returns to. (TUI-VIEW-NAV-005)
pub fn dangling_notice(scope: &Scope, parent: &Screen) -> String {
    let what = match scope {
        Scope::Lot(_) => "this lot was closed",
        Scope::Accrual(_) => "this accrual no longer exists",
        _ => "this item is gone",
    };
    format!("{what} by a newer entry — returning to {}", parent.title())
}
