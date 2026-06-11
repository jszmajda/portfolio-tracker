//! The **Ledger** theme — role tokens, the truecolor dark-only palette, the fixed
//! glyph set, and the cross-screen display conventions (freshness / degraded /
//! `[est]` / integrity-block) that `entry` and `views` apply **identically**, so
//! the same fact never renders two ways. (tui-design.md → "Cross-Screen Display
//! Conventions" / "Color Conventions" / "Interface Palette & Motifs")
//!
//! Screens reference **role tokens**, never raw colours, so an alternate palette
//! is a drop-in. Color is **semantic and redundant**: every colour-coded
//! distinction also carries a sign / glyph / word, so the UI reads correctly in
//! monochrome, when piped, under `NO_COLOR`, and for colorblind users.

use ratatui::style::{Color, Modifier, Style};

// ===========================================================================
// Role tokens (tui-design.md → "Color Conventions"). Screens key off these, never
// raw colours. The palette degrades truecolor -> 256 -> 16 -> none.
// ===========================================================================

/// A semantic/chrome role. Each maps to a colour via [`Palette`] AND carries a
/// redundant non-colour signal (a glyph/word/sign) the caller always emits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// Positive P&L / delta / return — sage; paired with leading `+` and `▲`.
    Gain,
    /// Negative P&L / delta / return — terracotta; paired with leading `−`/`▼`.
    Loss,
    /// Zero / no change — default fg; paired with `·`.
    Flat,
    /// Figures served from cache while offline — warm dim; paired with `⟲ as-of`.
    Stale,
    /// A value with no mark — warm dim; paired with `‡` / `—`.
    Degraded,
    /// Post-tax / unrealized-tax figures — verdigris; paired with `[est]`.
    Estimate,
    /// Needs attention (brackets stale, a confirm prompt, lock held) — amber; `⚠`.
    Warn,
    /// Integrity failure / untrustworthy data — escalated red; `✗` + a message.
    Error,
    /// Masthead, headers, rules, the status line — gilt.
    Accent,
    /// Primary text (newsprint).
    Fg,
    /// Secondary / chrome dim.
    FgDim,
    /// Tracked-caps labels, faint chrome.
    FgFaint,
}

/// How many colour levels the destination terminal supports; the theme degrades
/// gracefully truecolor → 256 → 16 → none. `None` disables colour and falls back
/// to the always-on redundant glyphs/words. (tui-design.md → "Honor the terminal")
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ColorDepth {
    /// 24-bit truecolor — the shipped "Ledger" palette.
    #[default]
    TrueColor,
    /// 256-colour — the nearest palette index.
    Indexed256,
    /// 16-colour ANSI.
    Ansi16,
    /// No colour: `NO_COLOR`, `--no-color`, or a non-tty (piped) destination — the
    /// glyphs/words carry all meaning. (tui-design.md → "Honor the terminal")
    None,
}

/// The accrual-lifecycle **minting** ramp position: a glance reads how far each
/// tax dollar has travelled. Coloured Accrued faint → Allocated verdigris → Moved
/// gilt → Paid sage. (tui-design.md → "Color Conventions")
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LifecycleStop {
    Accrued,
    Allocated,
    Moved,
    Paid,
}

/// The dark-only "Ledger" palette: warm ink paper, a single gilt accent,
/// sage/terracotta semantics. Resolves a [`Role`] to a `ratatui` [`Color`] at the
/// active [`ColorDepth`]. (tui-design.md → "Palette (truecolor, 'Ledger')")
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Palette {
    pub depth: ColorDepth,
}

impl Palette {
    /// The truecolor palette (the default shipped theme).
    pub fn ledger() -> Self {
        Palette {
            depth: ColorDepth::TrueColor,
        }
    }

    /// The palette at a given colour depth.
    pub fn at(depth: ColorDepth) -> Self {
        Palette { depth }
    }

    /// The truecolor RGB for a role (the canonical "Ledger" hex values).
    fn truecolor(role: Role) -> Color {
        match role {
            Role::Gain => Color::Rgb(0x6F, 0xA8, 0x6B),     // sage
            Role::Loss => Color::Rgb(0xC5, 0x59, 0x4B),     // terracotta
            Role::Flat => Color::Rgb(0xE8, 0xE1, 0xD3),     // fg
            Role::Stale => Color::Rgb(0x9A, 0x8F, 0x7A),    // warm dim
            Role::Degraded => Color::Rgb(0x9A, 0x8F, 0x7A), // warm dim
            Role::Estimate => Color::Rgb(0x5F, 0xA8, 0x9E), // verdigris
            Role::Warn => Color::Rgb(0xE0, 0x8A, 0x3C),     // amber
            Role::Error => Color::Rgb(0xE5, 0x54, 0x4A),    // escalated red
            Role::Accent => Color::Rgb(0xD4, 0xA8, 0x2C),   // gilt
            Role::Fg => Color::Rgb(0xE8, 0xE1, 0xD3),
            Role::FgDim => Color::Rgb(0x9A, 0x8F, 0x7A),
            Role::FgFaint => Color::Rgb(0x5C, 0x54, 0x44),
        }
    }

    /// The ANSI-16 fallback for a role (the 16-colour degrade rung).
    fn ansi16(role: Role) -> Color {
        match role {
            Role::Gain => Color::Green,
            Role::Loss => Color::Red,
            Role::Estimate => Color::Cyan,
            Role::Warn => Color::Yellow,
            Role::Error => Color::LightRed,
            Role::Accent => Color::Yellow,
            Role::Stale | Role::Degraded | Role::FgDim => Color::DarkGray,
            Role::FgFaint => Color::DarkGray,
            Role::Flat | Role::Fg => Color::Gray,
        }
    }

    /// The resolved colour for a role at the active depth. `ColorDepth::None`
    /// resolves to [`Color::Reset`] (no colour — the glyphs carry meaning). The
    /// `Indexed256` rung maps the truecolor RGB to the nearest xterm-256 palette
    /// index (the design's truecolor → 256 → 16 degrade), so a true-256 terminal is
    /// never sent 24-bit escapes. (tui-design.md → "degrade gracefully truecolor →
    /// 256 → 16 colours")
    // @spec TUI-VIEW-NAV-009
    pub fn color(&self, role: Role) -> Color {
        match self.depth {
            ColorDepth::TrueColor => Self::truecolor(role),
            ColorDepth::Indexed256 => match Self::truecolor(role) {
                Color::Rgb(r, g, b) => Color::Indexed(rgb_to_xterm256(r, g, b)),
                other => other,
            },
            ColorDepth::Ansi16 => Self::ansi16(role),
            ColorDepth::None => Color::Reset,
        }
    }

    /// A `ratatui` [`Style`] for a role: the colour at the active depth, plus the
    /// accent/error emphasis a motif uses. Under [`ColorDepth::None`] only the
    /// non-colour modifiers (bold for accent) survive.
    pub fn style(&self, role: Role) -> Style {
        let mut s = Style::default();
        if self.depth != ColorDepth::None {
            s = s.fg(self.color(role));
        }
        match role {
            Role::Accent => s.add_modifier(Modifier::BOLD),
            Role::Error => s.add_modifier(Modifier::BOLD),
            Role::Degraded | Role::Stale | Role::FgFaint => s.add_modifier(Modifier::DIM),
            _ => s,
        }
    }

    /// The colour for an accrual-lifecycle stop on the minting ramp.
    /// (tui-design.md → "Accrual lifecycle uses a minting progress ramp")
    pub fn lifecycle_color(&self, stop: LifecycleStop) -> Color {
        self.color(lifecycle_role(stop))
    }

    /// The focused-row background — the `bg-focus` chrome token (`#2A2618`): the
    /// row carrying the `▎` focus caret renders over this. Degrades with the
    /// ladder; at 16/none the caret glyph alone carries focus. (tui-design.md →
    /// "Focus caret" / "Palette")
    pub fn focus_bg(&self) -> Option<Color> {
        match self.depth {
            ColorDepth::TrueColor => Some(Color::Rgb(0x2A, 0x26, 0x18)),
            ColorDepth::Indexed256 => Some(Color::Indexed(rgb_to_xterm256(0x2A, 0x26, 0x18))),
            ColorDepth::Ansi16 | ColorDepth::None => None,
        }
    }
}

/// The ROLE a lifecycle stop renders in — the *minting* ramp as role tokens:
/// Accrued faint → Allocated verdigris → Moved gilt → Paid sage. The Tax screen
/// colours each accrual row by this so a glance reads how far each tax dollar has
/// travelled; the stepper dots + word are the redundant signal. (tui-design.md →
/// "Color Conventions" / "Lifecycle stepper")
pub fn lifecycle_role(stop: LifecycleStop) -> Role {
    match stop {
        LifecycleStop::Accrued => Role::FgFaint,
        LifecycleStop::Allocated => Role::Estimate,
        LifecycleStop::Moved => Role::Accent,
        LifecycleStop::Paid => Role::Gain,
    }
}

/// Map a 24-bit RGB to the nearest xterm-256 palette index — the truecolor → 256
/// degrade rung. Considers both the 6×6×6 colour cube (indices 16–231) and the
/// 24-step grayscale ramp (232–255), returning whichever is closer (squared
/// Euclidean distance), so a near-gray Ledger token degrades to the gray ramp
/// rather than a muddy cube cell. (tui-design.md → "degrade ... truecolor → 256")
fn rgb_to_xterm256(r: u8, g: u8, b: u8) -> u8 {
    // The cube uses six levels with these channel values.
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    fn nearest_level(c: u8) -> usize {
        let mut best = 0usize;
        let mut best_d = i32::MAX;
        for (i, &lv) in LEVELS.iter().enumerate() {
            let d = (c as i32 - lv as i32).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    }
    let (ri, gi, bi) = (nearest_level(r), nearest_level(g), nearest_level(b));
    let cube_index = 16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8;
    let (cr, cg, cb) = (LEVELS[ri] as i32, LEVELS[gi] as i32, LEVELS[bi] as i32);
    let cube_dist = (r as i32 - cr).pow(2) + (g as i32 - cg).pow(2) + (b as i32 - cb).pow(2);

    // The grayscale ramp: 24 steps from 8 to 238 in increments of 10 (indices
    // 232–255).
    let gray_avg = (r as i32 + g as i32 + b as i32) / 3;
    let gray_step = ((gray_avg - 8).clamp(0, 238) + 5) / 10;
    let gray_step = gray_step.clamp(0, 23);
    let gray_val = 8 + gray_step * 10;
    let gray_index = 232 + gray_step as u8;
    let gray_dist =
        (r as i32 - gray_val).pow(2) + (g as i32 - gray_val).pow(2) + (b as i32 - gray_val).pow(2);

    if gray_dist < cube_dist {
        gray_index
    } else {
        cube_index
    }
}

// ===========================================================================
// The fixed glyph set (tui-design.md → "Glyph set (fixed)"). One glyph, one
// meaning, everywhere — the always-on redundant signal that makes colour
// optional.
// ===========================================================================

/// The delta-up glyph (`gain`).
pub const GLYPH_UP: char = '▲';
/// The delta-down glyph (`loss`).
pub const GLYPH_DOWN: char = '▼';
/// The flat (zero / no change) glyph.
pub const GLYPH_FLAT: char = '·';
/// The degraded (no-mark) glyph.
pub const GLYPH_DEGRADED: char = '‡';
/// The em-dash a degraded/unpriced figure renders instead of `0`.
pub const GLYPH_DASH: char = '—';
/// The warn glyph.
pub const GLYPH_WARN: char = '⚠';
/// The error / integrity-block glyph.
pub const GLYPH_ERROR: char = '✗';
/// The stale (offline cache) glyph.
pub const GLYPH_STALE: char = '⟲';
/// Status connected dot.
pub const GLYPH_STATUS_ON: char = '●';
/// Status off / degraded dot.
pub const GLYPH_STATUS_OFF: char = '○';
/// A filled lifecycle stepper dot.
pub const GLYPH_STEP_FILLED: char = '◉';
/// An empty lifecycle stepper dot.
pub const GLYPH_STEP_EMPTY: char = '○';
/// The focus-caret left gutter.
pub const GLYPH_FOCUS: char = '▎';
/// The flow arrow.
pub const GLYPH_FLOW: char = '→';
/// The status-line lock glyph — write-lock **held** (a cron `summary`). Catalogued
/// here so the one glyph has one meaning everywhere. (tui-design.md → "Status line")
pub const GLYPH_LOCK_HELD: char = '🔒';
/// The status-line lock glyph — write-lock **free** (the interactive session may
/// acquire it). (tui-design.md → "Status line")
pub const GLYPH_LOCK_FREE: char = '🔓';

/// The sparkline ramp (eight rungs), tinted by net direction with the latest cell
/// brightened (a "today" tick). (tui-design.md → "Sparklines")
pub const SPARK_RAMP: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The fixed cell width of a per-row trend strip, and the dim dot that pads its
/// not-yet-captured trading days — a timeline filling in, never a one-block
/// stub. (tui-design.md → "Sparklines"; views-design.md → "Trend strip")
pub const SPARK_STRIP_W: usize = 12;
pub const SPARK_DOT: char = '·';

// ===========================================================================
// Money / share / decimal formatting (tui-design.md → "Money columns").
// Integer-exact, no float — every figure is `pt_core::Cents` / `MicroShares`.
// View money is whole-dollar (cents round half-to-even away): glance cells
// compact at magnitude (`$842` → `$123.5k` → `$2.65m`), reconciliation figures
// (the summary band, tax amounts/reserves) stay exact whole dollars, and
// per-share money keeps its two-decimal cents (a $0.04 OTC mark vanishes
// without them). `entry`'s editable fields keep cents (they re-parse what they
// show).
// ===========================================================================

use pt_core::{Cents, MicroShares, SHARE_SCALE};

/// Group an unsigned integer with thousands separators (`1284` → `1,284`).
pub fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::new();
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Format `Cents` as a `$`-prefixed dollar string with thousands separators and
/// two decimals (`$1,284.30`) — **per-share money and `entry` fields only** (the
/// places cents stay significant). View columns use [`money_whole`] /
/// [`money_compact`]. (tui-design.md → "Money columns")
pub fn money(c: Cents) -> String {
    let sign = if c.0 < 0 { "-" } else { "" };
    let abs = c.0.unsigned_abs();
    format!("{sign}${}.{:02}", group_thousands(abs / 100), abs % 100)
}

/// Format `Cents` as **exact whole dollars** (`$1,284,300`), rounding the cents
/// half-to-even away — the reconciliation form (the summary band totals, tax
/// amounts/reserves), never abbreviated. (tui-design.md → "Money columns";
/// TUI-VIEW-NAV-016)
pub fn money_whole(c: Cents) -> String {
    let sign = if c.0 < 0 { "-" } else { "" };
    let dollars = pt_core::round_half_to_even(c.0.unsigned_abs() as i128, 100) as u64;
    format!("{sign}${}", group_thousands(dollars))
}

/// [`money_whole`] with the leading `+`/`−` the gain/loss roles pair with
/// (`+$8,420` / `−$410`). Uses the typographic minus to match [`signed_money`].
pub fn signed_money_whole(c: Cents) -> String {
    let sign = if c.0 < 0 { "\u{2212}" } else { "+" };
    let dollars = pt_core::round_half_to_even(c.0.unsigned_abs() as i128, 100) as u64;
    format!("{sign}${}", group_thousands(dollars))
}

/// Format `Cents` in the **compact whole-dollar form** the glance cells use:
/// below $1,000 whole dollars (`$842`), below $1m tenths of a thousand
/// (`$123.5k`), at and above $1m hundredths of a million (`$2.65m`) — all
/// rounded half-to-even. Per-share money never compacts. (tui-design.md →
/// "Money columns"; TUI-VIEW-NAV-016)
// @spec TUI-VIEW-NAV-016
pub fn money_compact(c: Cents) -> String {
    let sign = if c.0 < 0 { "-" } else { "" };
    format!("{sign}{}", compact_abs_dollars(c.0.unsigned_abs()))
}

/// [`money_compact`] with the leading `+`/`−` the gain/loss roles pair with
/// (`+$3.1k` / `−$410`). Uses the typographic minus to match [`signed_money`].
pub fn signed_money_compact(c: Cents) -> String {
    let sign = if c.0 < 0 { "\u{2212}" } else { "+" };
    format!("{sign}{}", compact_abs_dollars(c.0.unsigned_abs()))
}

/// The unsigned compact form: whole dollars below $1k, tenths of k below $1m,
/// hundredths of m above — each band rounded half-to-even from the cents, with
/// a band promoted when its rounding carries it over the threshold ($999,950+
/// reads `$1.00m`, never `$1000.0k`). (TUI-VIEW-NAV-016)
fn compact_abs_dollars(abs_cents: u64) -> String {
    let dollars = pt_core::round_half_to_even(abs_cents as i128, 100) as u64;
    if dollars < 1_000 {
        return format!("${dollars}");
    }
    // Tenths of a thousand: 0.1k = $100 = 10,000 cents.
    let tenths_k = pt_core::round_half_to_even(abs_cents as i128, 10_000) as u64;
    if tenths_k < 10_000 {
        return format!("${}.{}k", tenths_k / 10, tenths_k % 10);
    }
    // Hundredths of a million: 0.01m = $10,000 = 1,000,000 cents.
    let hundredths_m = pt_core::round_half_to_even(abs_cents as i128, 1_000_000) as u64;
    format!(
        "${}.{:02}m",
        group_thousands(hundredths_m / 100),
        hundredths_m % 100
    )
}

/// Format `Cents` as a **signed** dollar string with the leading `+`/`−` the
/// gain/loss roles always pair with (`+$8,420.00` / `−$410.00`). Uses the typographic
/// minus `−` so the redundant sign matches the design's `+/−` cue.
pub fn signed_money(c: Cents) -> String {
    let sign = if c.0 < 0 { "−" } else { "+" };
    let abs = c.0.unsigned_abs();
    format!("{sign}${}.{:02}", group_thousands(abs / 100), abs % 100)
}

/// Format `MicroShares` as a share count, dropping the micro-scale and trimming
/// trailing fractional zeros (`834`, `1.5`).
pub fn shares(q: MicroShares) -> String {
    let whole = q.0 / SHARE_SCALE;
    let frac = (q.0 % SHARE_SCALE).unsigned_abs();
    if frac == 0 {
        whole.to_string()
    } else {
        let frac_str = format!("{frac:06}");
        format!("{whole}.{}", frac_str.trim_end_matches('0'))
    }
}

/// Format `MicroShares` as a **thousands-grouped** share count for ledger columns
/// (`24,150`, `1,250.5`) — the display variant of [`shares`]; the ungrouped form
/// stays the editable-field seed (a comma would break re-parsing). (tui-design.md →
/// "Money columns")
pub fn shares_grouped(q: MicroShares) -> String {
    let sign = if q.0 < 0 { "-" } else { "" };
    let abs = q.0.unsigned_abs();
    let whole = abs / (SHARE_SCALE as u64);
    let frac = abs % (SHARE_SCALE as u64);
    if frac == 0 {
        format!("{sign}{}", group_thousands(whole))
    } else {
        let frac_str = format!("{frac:06}");
        format!(
            "{sign}{}.{}",
            group_thousands(whole),
            frac_str.trim_end_matches('0')
        )
    }
}

/// Format a `ppm` value as a **signed** percent string with one decimal — the
/// `+`/`−` the gain/loss roles always pair with (`+0.6%` / `−0.6%`; zero stays
/// the bare `0.0%`). Uses the typographic minus to match [`signed_money`].
pub fn signed_percent_ppm(ppm: i64) -> String {
    use std::cmp::Ordering;
    match ppm.cmp(&0) {
        Ordering::Greater => format!("+{}", percent_ppm(ppm)),
        Ordering::Less => format!("\u{2212}{}", percent_ppm(-ppm)),
        Ordering::Equal => percent_ppm(0),
    }
}

/// Format a `ppm` value as a percent string with one decimal (`660_000` → `66.0%`).
pub fn percent_ppm(ppm: i64) -> String {
    let sign = if ppm < 0 { "-" } else { "" };
    let abs = ppm.unsigned_abs();
    // ppm/10_000 = percent ×100; render to one decimal place.
    let tenths = abs / 1_000; // tenths of a percent
    format!("{sign}{}.{}%", tenths / 10, tenths % 10)
}

// ===========================================================================
// Cross-screen convention helpers (tui-design.md → "Cross-Screen Display
// Conventions"). Applied IDENTICALLY in entry and views.
// ===========================================================================

/// How a figure that may be unqualified, degraded, stale, or estimated should be
/// labelled — the "no number without its qualifier" tenet. Every screen renders a
/// figure through this so the markers travel with the figure.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Qualifier {
    /// Live-and-exact: no marker (an unqualified number means live-and-exact).
    None,
    /// Served from cache while offline — `⟲ as-of <epoch>` in the `stale` role.
    Stale { quote_epoch: String },
    /// No **mark** for the figure — the `‡` marker (never `0`), `degraded` role,
    /// the word "unpriced".
    Unpriced,
    /// A required **upstream input** is missing (a degraded tax estimate) — the
    /// `‡`/`—` marker, `degraded` role, the word "degraded" (NOT "unpriced", so a
    /// priced-but-tax-degraded symbol is never mislabelled). (tui-design.md →
    /// "Degradation")
    Degraded,
    /// A post-tax / unrealized-tax estimate — `[est]`, `estimate` role.
    Estimate,
    /// An estimate under stale brackets — `[est, brackets stale]` (the same wording
    /// `summary` uses). (tui-design.md → "Estimates")
    EstimateStaleBrackets,
    /// An estimate under cold-start `NoBracketsAvailable` — `n/a (no brackets)`.
    NoBrackets,
}

impl Qualifier {
    /// The marker text that travels with the figure (empty for `None` — an
    /// unqualified number is live-and-exact). (tui-design.md → tenet "No number
    /// without its qualifier")
    pub fn marker(&self) -> String {
        match self {
            Qualifier::None => String::new(),
            Qualifier::Stale { quote_epoch } => format!("{GLYPH_STALE} as-of {quote_epoch}"),
            Qualifier::Unpriced => format!("{GLYPH_DEGRADED} unpriced"),
            Qualifier::Degraded => format!("{GLYPH_DEGRADED} degraded"),
            Qualifier::Estimate => "[est]".to_string(),
            Qualifier::EstimateStaleBrackets => "[est, brackets stale]".to_string(),
            Qualifier::NoBrackets => "n/a (no brackets)".to_string(),
        }
    }

    /// The role this qualifier renders in.
    pub fn role(&self) -> Role {
        match self {
            Qualifier::None => Role::Fg,
            Qualifier::Stale { .. } => Role::Stale,
            Qualifier::Unpriced | Qualifier::Degraded => Role::Degraded,
            Qualifier::Estimate | Qualifier::EstimateStaleBrackets | Qualifier::NoBrackets => {
                Role::Estimate
            }
        }
    }
}

/// Map a `config::BracketState` to the estimate qualifier `summary` and the TUI
/// share: `Verified` → `[est]`, `Stale` → `[est, brackets stale]`,
/// `NoBracketsAvailable` → `n/a (no brackets)`. (tui-design.md → "Estimates")
pub fn estimate_qualifier(state: config::BracketState) -> Qualifier {
    match state {
        config::BracketState::Verified => Qualifier::Estimate,
        config::BracketState::Stale => Qualifier::EstimateStaleBrackets,
        config::BracketState::NoBracketsAvailable => Qualifier::NoBrackets,
    }
}

/// Render a delta figure with its always-paired sign + glyph: `▲ +$8,420.00`
/// (gain), `▼ −$410.00` (loss), `· $0.00` (flat) — the two-decimal (cents)
/// form, for the places cents stay significant. The role is chosen to match.
/// (tui-design.md → "Color is redundant, never sole")
pub fn delta_text(c: Cents) -> (String, Role) {
    use std::cmp::Ordering;
    match c.0.cmp(&0) {
        Ordering::Greater => (format!("{GLYPH_UP} {}", signed_money(c)), Role::Gain),
        Ordering::Less => (format!("{GLYPH_DOWN} {}", signed_money(c)), Role::Loss),
        Ordering::Equal => (format!("{GLYPH_FLAT} {}", money(c)), Role::Flat),
    }
}

/// [`delta_text`] in **exact whole dollars** (`▲ +$8,420` / `· $0`) — the
/// summary band's reconciliation deltas. (TUI-VIEW-NAV-016, TUI-VIEW-POS-010)
pub fn delta_text_whole(c: Cents) -> (String, Role) {
    use std::cmp::Ordering;
    match c.0.cmp(&0) {
        Ordering::Greater => (format!("{GLYPH_UP} {}", signed_money_whole(c)), Role::Gain),
        Ordering::Less => (
            format!("{GLYPH_DOWN} {}", signed_money_whole(c)),
            Role::Loss,
        ),
        Ordering::Equal => (format!("{GLYPH_FLAT} {}", money_whole(c)), Role::Flat),
    }
}

/// [`delta_text`] in the **compact whole-dollar form** (`▲ +$3.1k` / `▼ −$410`)
/// — the glance columns' deltas. (TUI-VIEW-NAV-016)
pub fn delta_text_compact(c: Cents) -> (String, Role) {
    use std::cmp::Ordering;
    match c.0.cmp(&0) {
        Ordering::Greater => (
            format!("{GLYPH_UP} {}", signed_money_compact(c)),
            Role::Gain,
        ),
        Ordering::Less => (
            format!("{GLYPH_DOWN} {}", signed_money_compact(c)),
            Role::Loss,
        ),
        Ordering::Equal => (format!("{GLYPH_FLAT} {}", money_compact(c)), Role::Flat),
    }
}

/// Render a four-dot accrual **lifecycle stepper** for a state, coloured by the
/// minting ramp: `◉◉◉○ Moved`, `◉◉◉◉ Paid`. A de-minimis auto-settled accrual
/// shows a distinct `✓ settled` (NOT a partial stepper); an orphaned one a `⚠
/// undone — needs unwind` badge. (tui-design.md → "Lifecycle stepper")
pub fn stepper(state: &StepperState) -> String {
    match state {
        StepperState::AutoSettled => "✓ settled".to_string(),
        StepperState::Orphaned => format!("{GLYPH_WARN} undone — needs unwind"),
        StepperState::Lifecycle(stage) => {
            format!("{} {}", stepper_dots(*stage), stepper_label(*stage))
        }
    }
}

/// The stepper's four-dot run alone (`◉◉◉○`) — the segment that carries the
/// lifecycle ramp colour on the spans seam, while the row text stays fg.
/// (tui-design.md → "Lifecycle stepper")
pub fn stepper_dots(stage: LifecycleStop) -> String {
    let filled = match stage {
        LifecycleStop::Accrued => 1,
        LifecycleStop::Allocated => 2,
        LifecycleStop::Moved => 3,
        LifecycleStop::Paid => 4,
    };
    let mut dots = String::new();
    for i in 0..4 {
        dots.push(if i < filled {
            GLYPH_STEP_FILLED
        } else {
            GLYPH_STEP_EMPTY
        });
    }
    dots
}

/// The stepper's state word — the redundant non-colour signal beside the dots.
pub fn stepper_label(stage: LifecycleStop) -> &'static str {
    match stage {
        LifecycleStop::Accrued => "Accrued",
        LifecycleStop::Allocated => "Allocated",
        LifecycleStop::Moved => "Moved",
        LifecycleStop::Paid => "Paid",
    }
}

/// What an accrual's stepper renders: an ordinary lifecycle stage, a de-minimis
/// auto-settled `✓ settled`, or an orphaned `⚠ undone`. (tui-design.md →
/// "Auto-settled & orphan accruals")
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StepperState {
    Lifecycle(LifecycleStop),
    /// De-minimis auto-settled — a distinct settled state, excluded from
    /// actionable selection (NOT a stuck mid-lifecycle stepper).
    AutoSettled,
    /// Orphaned (its sale was reversed after Move/Pay — `TAX-VERIF-007`).
    Orphaned,
}

/// Build a direction-tinted sparkline from a value series, brightening the last
/// cell (a "today" tick). Empty series → empty string. (tui-design.md →
/// "Sparklines") The `gain`/`loss` tint is by net direction (last − first).
/// Cells scale between the series min and max; a **zero-span** series (a single
/// point, or all points equal) renders the MID rung — a flat line mid-strip,
/// never a bottom-scraping floor that reads as a crash.
pub fn sparkline(values: &[i64]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let min = *values.iter().min().unwrap();
    let max = *values.iter().max().unwrap();
    if max == min {
        return std::iter::repeat(SPARK_RAMP[3])
            .take(values.len())
            .collect();
    }
    let span = max - min;
    values
        .iter()
        .map(|v| {
            let rung = (((v - min) * 7) / span).clamp(0, 7) as usize;
            SPARK_RAMP[rung]
        })
        .collect()
}

/// The net direction (for tinting a sparkline / a series): `gain` when the last
/// point exceeds the first, `loss` when below, `flat` when equal/degenerate.
pub fn series_role(values: &[i64]) -> Role {
    match (values.first(), values.last()) {
        (Some(a), Some(b)) if b > a => Role::Gain,
        (Some(a), Some(b)) if b < a => Role::Loss,
        _ => Role::Flat,
    }
}
