//! `form` — the entry-shell **render + input** layer (prefix `TUI-ENTRY`): the
//! pure, testable Model half of the composer interaction the `tui` shell's
//! crossterm loop will drive. It carries no truth and computes nothing; it holds
//! the field-navigation state machine (`TUI-ENTRY-FLOW-008`) the loop folds keys
//! into, and the field/label/error-slot/footer description (`TUI-ENTRY-FLOW-009`)
//! the VIEW renders, plus the lot-picker footer band (`TUI-ENTRY-LOT-006`).
//!
//! The forms in [`crate::entry`] (`BuyForm`, `SellForm`, …) own *what is composed
//! and validated*; this module owns *how the active form is navigated and laid
//! out*. A [`FormModel`] is the spine of an Entry context: it lists the form's
//! fields in tab order, tracks the focused field, the input sub-mode, per-field
//! dirtiness, the single inline error slot, and the footer accelerators — so the
//! shell can route a key to the active context without per-flow special-casing
//! (tui-design.md → "The active context owns the keymap").

use crate::entry::{
    BuyForm, FlowKind, InlineError, LotPicker, Phase, PickerState, ResidencyForm, SellForm,
    SplitForm, VestForm,
};
use crate::theme;
use pt_core::{Cents, Date, MicroShares};

// ===========================================================================
// Field-navigation state machine (TUI-ENTRY-FLOW-008). tab/shift-tab (and ↑/↓)
// move between fields; enter on the last field submits; esc cancels and pops the
// context (confirm-discard if dirty); a focused field enters text-input mode in
// which global single-key accelerators are suppressed and esc returns to
// field-navigation mode. The active context (top stack frame) owns the keymap.
// ===========================================================================

/// The input sub-mode of the active form (tui-design.md → "Modality"). In
/// [`InputMode::FieldNav`] the bracketed single-key accelerators are live and
/// tab/shift-tab move focus; in [`InputMode::TextInput`] a field is being typed
/// into, the shell's global accelerators are **suppressed**, and `esc` returns to
/// field-navigation mode. (TUI-ENTRY-FLOW-008)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum InputMode {
    /// Navigating between fields; accelerators bound. (TUI-ENTRY-FLOW-008)
    #[default]
    FieldNav,
    /// Typing into the focused field; accelerators suppressed. (TUI-ENTRY-FLOW-008)
    TextInput,
}

/// What a single key-driven transition asks the shell to do next. The transition
/// stays IN the model (pure); the shell performs the side effect (popping the
/// context, running the submit). (tui-design.md → "Errors and submits ride the
/// Model"; TUI-ENTRY-FLOW-008)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FormAction {
    /// The transition was absorbed (focus moved, a mode change) — re-render only.
    Consumed,
    /// `enter` on the last field: the shell should run the submit for this form.
    /// (TUI-ENTRY-FLOW-008)
    Submit,
    /// `esc` in field-navigation mode with **no** dirty field: pop the context
    /// immediately. (TUI-ENTRY-FLOW-008)
    Cancel,
    /// `esc` in field-navigation mode with a dirty field: the shell must raise a
    /// confirm-discard prompt before popping. (TUI-ENTRY-FLOW-008)
    ConfirmDiscard,
}

/// One field in a form, in tab order. Carries its tracked-caps label, the rendered
/// value, and whether it is currently dirty (edited from its seeded default). The
/// inline error slot attaches to a field by index, not stored here, so the single
/// slot is shared. (TUI-ENTRY-FLOW-009)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Field {
    /// The field label, stored as authored; rendered tracked-caps. (TUI-ENTRY-FLOW-009)
    pub label: String,
    /// The current rendered value of the field.
    pub value: String,
    /// `true` once the owner has edited the field away from its seeded default —
    /// the `esc`-cancel confirm-discard gate keys off any dirty field.
    /// (TUI-ENTRY-FLOW-008)
    pub dirty: bool,
}

impl Field {
    /// A clean field with a label + seeded value.
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Field { label: label.into(), value: value.into(), dirty: false }
    }
}

/// The render + input model of one active entry form — the spine of an Entry
/// context. It lists the form's fields in tab order, tracks the focused field, the
/// input sub-mode, the single inline error/advisory slot, the write-loop phase, and
/// the footer accelerators. The transition methods are pure and testable; the shell
/// folds keys into them and renders the result. (TUI-ENTRY-FLOW-008/009)
// @spec TUI-ENTRY-FLOW-008, TUI-ENTRY-FLOW-009
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FormModel {
    /// Which flow this form drives (fixes the title + the confirm-gating).
    pub kind: FlowKind,
    /// The fields, in tab order. Always non-empty for a real form.
    pub fields: Vec<Field>,
    /// The focused field index (the gilt focus caret + `bg-focus`). (TUI-ENTRY-FLOW-009)
    pub focus: usize,
    /// Field-navigation vs text-input. (TUI-ENTRY-FLOW-008)
    pub input_mode: InputMode,
    /// The single inline error/advisory slot, attached to the field it sits beneath
    /// — shared by inline validation (`TUI-ENTRY-FLOW-001`) and a submit-time
    /// disagreement (`TUI-ENTRY-FLOW-002`). (TUI-ENTRY-FLOW-009)
    pub error: Option<FieldError>,
    /// The current write-loop phase (drives the footer accelerators: a `Retry`
    /// shows `[r]`, a `ConfirmNewSymbol` shows `[c]`, a gated `Confirming` shows
    /// `[enter]`). (TUI-ENTRY-FLOW-009)
    pub phase: Phase,
}

/// The inline error/advisory slot's content + the field index it renders beneath.
/// (TUI-ENTRY-FLOW-009)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FieldError {
    /// The field this error sits directly beneath. (TUI-ENTRY-FLOW-009)
    pub field: usize,
    /// The error/advisory text (the verified kernel/config error's words, or a
    /// composer-local advisory).
    pub message: String,
    /// `true` for a `warn`-role advisory (a Move shortfall, a Pay mismatch); `false`
    /// for an `error`-role validation rejection. (TUI-ENTRY-FLOW-001/002)
    pub advisory: bool,
}

impl FormModel {
    /// A fresh form for a flow with its fields in tab order, focus on the first
    /// field, field-navigation mode, no error, editing. (TUI-ENTRY-FLOW-008/009)
    pub fn new(kind: FlowKind, fields: Vec<Field>) -> Self {
        FormModel {
            kind,
            fields,
            focus: 0,
            input_mode: InputMode::FieldNav,
            error: None,
            phase: Phase::Editing,
        }
    }

    /// The flow title rendered in the panel border (`Buy`, `Sell AMZN`, …).
    /// (TUI-ENTRY-FLOW-009)
    pub fn title(&self) -> String {
        flow_title(self.kind)
    }

    /// `true` when any field is dirty — the `esc`-cancel confirm-discard gate.
    /// (TUI-ENTRY-FLOW-008)
    pub fn is_dirty(&self) -> bool {
        self.fields.iter().any(|f| f.dirty)
    }

    /// `true` when the focused field is the last in tab order (`enter` here is
    /// submit, not advance). (TUI-ENTRY-FLOW-008)
    pub fn on_last_field(&self) -> bool {
        self.focus + 1 >= self.fields.len()
    }

    // --- Field navigation (TUI-ENTRY-FLOW-008) -----------------------------

    /// `tab` / `↓` — move focus to the next field, wrapping to the first; a no-op
    /// while a field is in text-input mode (the keystroke is the field's). Only
    /// fires in field-navigation mode. (TUI-ENTRY-FLOW-008)
    pub fn focus_next(&mut self) -> FormAction {
        if self.input_mode == InputMode::TextInput {
            return FormAction::Consumed;
        }
        if !self.fields.is_empty() {
            self.focus = (self.focus + 1) % self.fields.len();
        }
        FormAction::Consumed
    }

    /// `shift-tab` / `↑` — move focus to the previous field, wrapping to the last.
    /// A no-op in text-input mode. (TUI-ENTRY-FLOW-008)
    pub fn focus_prev(&mut self) -> FormAction {
        if self.input_mode == InputMode::TextInput {
            return FormAction::Consumed;
        }
        if !self.fields.is_empty() {
            self.focus = (self.focus + self.fields.len() - 1) % self.fields.len();
        }
        FormAction::Consumed
    }

    /// Enter **text-input mode** on the focused field — the shell suppresses its
    /// global single-key accelerators so a typed character is never intercepted as
    /// a command. A no-op when already in text-input mode. (TUI-ENTRY-FLOW-008)
    pub fn enter_field(&mut self) -> FormAction {
        self.input_mode = InputMode::TextInput;
        FormAction::Consumed
    }

    /// `esc` from text-input mode — leave the field back to field-navigation mode
    /// (NOT a flow cancel). The shell's accelerators are bound again.
    /// (TUI-ENTRY-FLOW-008)
    fn leave_field(&mut self) -> FormAction {
        self.input_mode = InputMode::FieldNav;
        FormAction::Consumed
    }

    /// `enter` — on the last field this is **submit**; otherwise advance to the next
    /// field. In text-input mode `enter` commits the field (leaves text-input) and
    /// then applies the same rule. (TUI-ENTRY-FLOW-008)
    pub fn enter(&mut self) -> FormAction {
        if self.input_mode == InputMode::TextInput {
            self.input_mode = InputMode::FieldNav;
        }
        if self.on_last_field() {
            FormAction::Submit
        } else {
            self.focus += 1;
            FormAction::Consumed
        }
    }

    /// `esc` — in text-input mode it returns to field-navigation mode; in
    /// field-navigation mode it cancels the flow and pops the context, gated by a
    /// confirm-discard when any field is dirty. The shell performs the pop / raises
    /// the confirm. (TUI-ENTRY-FLOW-008)
    pub fn escape(&mut self) -> FormAction {
        if self.input_mode == InputMode::TextInput {
            return self.leave_field();
        }
        if self.is_dirty() {
            FormAction::ConfirmDiscard
        } else {
            FormAction::Cancel
        }
    }

    /// Type a character into the focused field (only meaningful in text-input mode);
    /// marks the field dirty. (TUI-ENTRY-FLOW-008)
    pub fn type_char(&mut self, c: char) {
        if self.input_mode != InputMode::TextInput {
            return;
        }
        if let Some(f) = self.fields.get_mut(self.focus) {
            f.value.push(c);
            f.dirty = true;
        }
    }

    /// Backspace in the focused field (text-input mode); marks it dirty.
    /// (TUI-ENTRY-FLOW-008)
    pub fn backspace(&mut self) {
        if self.input_mode != InputMode::TextInput {
            return;
        }
        if let Some(f) = self.fields.get_mut(self.focus) {
            f.value.pop();
            f.dirty = true;
        }
    }

    // --- The single inline error/advisory slot (TUI-ENTRY-FLOW-001/002/009) ---

    /// Place the single inline error beneath a field, in the `error` role — shared
    /// by inline validation (`TUI-ENTRY-FLOW-001`) and a submit-time disagreement
    /// (`TUI-ENTRY-FLOW-002`). (TUI-ENTRY-FLOW-009)
    pub fn set_error(&mut self, field: usize, err: &InlineError) {
        self.error = Some(FieldError { field, message: err.text(), advisory: false });
    }

    /// Place a `warn`-role advisory beneath a field (a Move shortfall, a Pay
    /// jurisdiction/year mismatch) — the same single slot, never blocking.
    /// (TUI-ENTRY-TAX-002/003; TUI-ENTRY-FLOW-009)
    pub fn set_advisory(&mut self, field: usize, message: impl Into<String>) {
        self.error = Some(FieldError { field, message: message.into(), advisory: true });
    }

    /// Clear the inline slot (a clean re-validation). (TUI-ENTRY-FLOW-001)
    pub fn clear_error(&mut self) {
        self.error = None;
    }

    /// Record the write-loop phase a submit returned (so the footer offers the
    /// matching accelerator + the inline slot shows a returned-control notice).
    /// (TUI-ENTRY-FLOW-002/004/005)
    pub fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
    }

    // --- The footer accelerator hint line (TUI-ENTRY-FLOW-009) -------------

    /// The active bracketed single-key accelerators for the footer hint line — the
    /// keys live in the **current** phase/flow, bound only while no field is in
    /// text-input mode (in text-input mode only the field's own `esc`/`enter` are
    /// live, so the accelerator hints are suppressed). (TUI-ENTRY-FLOW-009)
    pub fn footer_accelerators(&self) -> Vec<&'static str> {
        if self.input_mode == InputMode::TextInput {
            // Accelerators are suppressed; only the field's escape/commit apply.
            return vec!["[esc] field", "[enter] commit"];
        }
        let mut keys = Vec::new();
        match &self.phase {
            // A returned-control submit offers a retry. (TUI-ENTRY-FLOW-004/005)
            Phase::Retry(_) => keys.push("[r] retry"),
            // The new-symbol guard offers a confirm-new-position. (TUI-ENTRY-ACT-006)
            Phase::ConfirmNewSymbol(_) => keys.push("[c] confirm"),
            // A gated confirm step. (TUI-ENTRY-FLOW-006)
            Phase::Confirming => keys.push("[enter] confirm"),
            _ => {
                // The lot picker offers FIFO-fill; a multi-select offers [space].
                if self.kind == FlowKind::Sell {
                    keys.push("[F] FIFO");
                }
                if matches!(self.kind, FlowKind::Pay) {
                    keys.push("[space] toggle");
                }
                keys.push("[enter] submit");
            }
        }
        keys.push("[esc] cancel");
        keys
    }

    /// The footer hint line text — the active accelerators joined `·`-segmented
    /// (the same chrome separator the status line uses). (TUI-ENTRY-FLOW-009)
    pub fn footer_hint(&self) -> String {
        self.footer_accelerators().join("   ")
    }
}

/// The panel title for a flow. (TUI-ENTRY-FLOW-009)
pub fn flow_title(kind: FlowKind) -> String {
    match kind {
        FlowKind::Buy => "Buy",
        FlowKind::Vest => "Vest",
        FlowKind::Sell => "Sell",
        FlowKind::Split => "Split",
        FlowKind::Reversal => "Reverse an event",
        FlowKind::Allocate => "Allocate accrual",
        FlowKind::Move => "Move accrual",
        FlowKind::Pay => "Pay",
        FlowKind::Override => "Override accrual",
        FlowKind::ResidencyEdit => "Residency move",
        FlowKind::TaxRuleEdit => "Tax rules",
        FlowKind::PlatformAliasEdit => "Platforms & aliases",
    }
    .to_string()
}

// ===========================================================================
// The lot-picker footer band (TUI-ENTRY-LOT-006). The picker renders as a Table
// of open lots; the running total / [est] preview / empty-state / inline error sit
// in a footer band BELOW the table body, never painted over a data row.
// ===========================================================================

/// The lot-picker footer band lines + their roles — the running `allocated N / M`,
/// the `[est]` gain/tax preview, the explicit empty/insufficient state, and the
/// inline error, laid out in a band beneath the table body (never over a data row).
/// (TUI-ENTRY-LOT-006)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PickerFooter {
    /// The footer lines, each with its render role.
    pub lines: Vec<(String, theme::Role)>,
}

/// Build the lot-picker footer band: the explicit empty/insufficient state OR the
/// running `allocated N / M` (with the ✓ when complete), the `[est]` gain/tax
/// preview, and the inline error — all beneath the table body. The footer is what
/// replaces a data row, so the running total/preview/state/error never paint over a
/// lot. (TUI-ENTRY-LOT-006; entry-design.md → "The lot picker renders as a Table …
/// footer band")
// @spec TUI-ENTRY-LOT-006
pub fn picker_footer(
    picker: &LotPicker,
    preview: Option<&crate::entry::GainTaxPreview>,
    error: Option<&InlineError>,
) -> PickerFooter {
    let mut lines: Vec<(String, theme::Role)> = Vec::new();

    // The running total / explicit state. An empty/insufficient state replaces the
    // running `N / M`. (TUI-ENTRY-LOT-004/006)
    let state = picker.state();
    match &state {
        PickerState::NoOpenLots | PickerState::Insufficient { .. } => {
            lines.push((state.message(&picker.symbol, &picker.platform), theme::Role::Warn));
        }
        PickerState::Complete => {
            lines.push((state.message(&picker.symbol, &picker.platform), theme::Role::Accent));
        }
        PickerState::UnderAllocated { .. } => {
            lines.push((state.message(&picker.symbol, &picker.platform), theme::Role::Fg));
        }
    }

    // The [est] gain/tax preview — `[est]`-flagged, degraded (`‡`, never a zero)
    // when the mark is unavailable. (TUI-ENTRY-LOT-005/006)
    if let Some(p) = preview {
        lines.push(preview_line(p));
    }

    // The inline error sits in the footer band, never over a data row.
    // (TUI-ENTRY-LOT-006)
    if let Some(e) = error {
        lines.push((e.text(), theme::Role::Error));
    }

    PickerFooter { lines }
}

/// The `[est]` gain/tax preview footer line + role: `est. gain $X · est. tax $Y
/// [est]`, or the degraded `‡` form (never a zero). (TUI-ENTRY-LOT-005/006)
pub fn preview_line(p: &crate::entry::GainTaxPreview) -> (String, theme::Role) {
    if p.degraded {
        return (
            format!("est. gain {0} · est. tax {0}  {1} degraded", theme::GLYPH_DASH, theme::GLYPH_DEGRADED),
            theme::Role::Degraded,
        );
    }
    let gain = p
        .est_gain
        .map(theme::signed_money)
        .unwrap_or_else(|| theme::GLYPH_DASH.to_string());
    let tax = match p.est_tax {
        Some(t) => format!("{} [est]", theme::money(t)),
        // No tax estimate under cold-start brackets — the convention wording.
        None => theme::estimate_qualifier(config::BracketState::NoBracketsAvailable).marker(),
    };
    (format!("est. gain {gain} · est. tax {tax}"), theme::Role::Estimate)
}

// ===========================================================================
// Per-flow field builders (TUI-ENTRY-FLOW-009; TUI-ENTRY-ACT/TAX/CFG). Each turns
// a composed entry form into the ordered [`Field`] list the render layer lays out,
// so EVERY FlowKind renders through the one form-layout path. The field order is
// the tab order. These read the form's CURRENT values; rendering is a projection.
// ===========================================================================

/// Render a value field common across forms.
fn field(label: &str, value: impl Into<String>) -> Field {
    Field::new(label, value)
}

/// Render a money field (`$X.YY`).
fn money_field(label: &str, c: Cents) -> Field {
    Field::new(label, theme::money(c))
}

/// Render a share-count field.
fn shares_field(label: &str, q: MicroShares) -> Field {
    Field::new(label, theme::shares(q))
}

/// Render a date field (the integer day key — the binary will format it).
fn date_field(label: &str, d: Date) -> Field {
    Field::new(label, d.0.to_string())
}

/// An optional tracking-code field (blank when absent).
fn optional_field(label: &str, v: &Option<String>) -> Field {
    Field::new(label, v.clone().unwrap_or_default())
}

impl FormModel {
    /// A Buy composer's form: symbol, qty, unit price, date, fees, platform,
    /// tracking code (no lot picker). (TUI-ENTRY-ACT-001; TUI-ENTRY-FLOW-009)
    pub fn for_buy(f: &BuyForm) -> Self {
        FormModel::new(
            FlowKind::Buy,
            vec![
                field("Symbol", f.symbol.clone()),
                shares_field("Qty", f.qty),
                money_field("Unit price", f.unit_price),
                date_field("Date", f.date),
                money_field("Fees", f.fees),
                field("Platform", f.platform.clone()),
                optional_field("Tracking code", &f.tracking_code),
            ],
        )
    }

    /// A Vest composer's form: symbol, qty, FMV/share, date, platform, tracking code
    /// (no fee field). (TUI-ENTRY-ACT-002; TUI-ENTRY-FLOW-009)
    pub fn for_vest(f: &VestForm) -> Self {
        FormModel::new(
            FlowKind::Vest,
            vec![
                field("Symbol", f.symbol.clone()),
                shares_field("Qty", f.qty),
                money_field("FMV/share", f.fmv_per_share),
                date_field("Date", f.date),
                field("Platform", f.platform.clone()),
                optional_field("Tracking code", &f.tracking_code),
            ],
        )
    }

    /// A Split composer's form: symbol, ratio num:den, date. (TUI-ENTRY-ACT-004;
    /// TUI-ENTRY-FLOW-009)
    pub fn for_split(f: &SplitForm) -> Self {
        FormModel::new(
            FlowKind::Split,
            vec![
                field("Symbol", f.symbol.clone()),
                field("Ratio", format!("{}:{}", f.ratio_num, f.ratio_den)),
                date_field("Date", f.date),
            ],
        )
    }

    /// A Sell composer's form: symbol, qty, unit price, date, fees, platform,
    /// tracking code, accrues-to-state. The lot picker rides on the [`EntryContext`]
    /// as a separate Table (`TUI-ENTRY-LOT-006`), not as a field. (TUI-ENTRY-ACT-003;
    /// TUI-ENTRY-FLOW-009)
    pub fn for_sell(f: &SellForm) -> Self {
        FormModel::new(
            FlowKind::Sell,
            vec![
                field("Symbol", f.symbol.clone()),
                shares_field("Qty", f.qty),
                money_field("Unit price", f.unit_price),
                date_field("Date", f.date),
                money_field("Fees", f.fees),
                field("Platform", f.platform.clone()),
                field("Accrues to", f.accrues_to_state.clone().unwrap_or_default()),
                optional_field("Tracking code", &f.tracking_code),
            ],
        )
    }

    /// A Residency-move composer's form: effective date, state. (TUI-ENTRY-CFG-001;
    /// TUI-ENTRY-FLOW-009)
    pub fn for_residency(f: &ResidencyForm) -> Self {
        FormModel::new(
            FlowKind::ResidencyEdit,
            vec![
                date_field("Effective date", f.effective_date),
                field("State", f.state.clone()),
            ],
        )
    }
}
