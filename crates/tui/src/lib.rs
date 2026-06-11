//! `tui` — the ratatui terminal application: the only way activity is entered and
//! the primary way the portfolio is read day to day.
//!
//! It is a **sub-HLD** with two leaf prefixes — `TUI-ENTRY` (the write-path
//! composer flows) and `TUI-VIEW` (the read-only screens) — sharing the app shell
//! and the cross-screen display conventions (freshness / degraded / `[est]` /
//! integrity-block / confirmations) and the gilt **Ledger** role-token theme. It
//! runs **through `runtime`** (the [`port`] seam): runtime drives the replay cycle,
//! holds the `Snapshot` + cached marks the screens render, owns the Sheets-access
//! layer + the advisory write-lock, and is where interactive submits acquire the
//! lock. (docs/intent/tui/tui-design.md + entry/ + views/)
//!
//! App structure is a hand-rolled **Model–Update–View** loop over `ratatui` +
//! `crossterm`. It sits OUTSIDE the `verus!{}` boundary; `cargo test` is the gate.
//! All money is integer `pt_core::Cents` / `MicroShares`; no float past the input
//! boundary. Tests drive the Model-Update-View logic + rendering against a FAKE
//! runtime ([`testkit::FakeRuntime`]): they assert on rendered buffers
//! ([`render_to_buffer`], via ratatui's `TestBackend`) and state transitions, never
//! a live TTY or Sheets.

pub mod entry;
pub mod form;
pub mod port;
pub mod theme;
pub mod views;

pub mod testkit;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Paragraph, Row, Table, Widget,
};

use entry::{Candidate, Composer, LotPicker, Phase};
use form::{FormAction, FormModel, InputMode};
use port::{RuntimePort, ViewState};
use pt_core::{Date, MicroShares};
use theme::{Palette, Role};
use views::{NavState, Screen};

// ===========================================================================
// App shell (tui-design.md → "App Shell & Navigation"). A keyboard-driven shell
// with a top-level switch between Entry and Views, plus a persistent status line.
// Screens are pushed/popped on a stack; nothing is mutated by navigation alone.
// ===========================================================================

/// The app identity rendered in the masthead — tracked uppercase gilt, the
/// ledger's spine. (tui-design.md → "Masthead")
pub const MASTHEAD: &str = "L E D G E R";

/// The top-level mode: **Entry** (mutation through the write-path loop) or **Views**
/// (read-only rendering). `[tab]` switches. (tui-design.md → "App Shell")
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// Read-only screens. (TUI-VIEW-*)
    #[default]
    Views,
    /// Write-path composer flows. (TUI-ENTRY-*)
    Entry,
}

/// One frame on the screen stack: a `views` screen plus its retained per-screen
/// navigation state (filter / sort / scope / focus). Drill-down pushes; ascend
/// pops. Nothing is mutated by navigation alone. (tui-design.md → "App Shell";
/// TUI-VIEW-NAV-003)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    pub nav: NavState,
}

impl Frame {
    /// A fresh top-level frame for a screen.
    pub fn new(screen: Screen) -> Self {
        Frame { nav: NavState::new(screen) }
    }
}

/// One Entry context on the entry stack — the active composer form plus, for a
/// Sell, the lot picker it drives. The **top frame is the active keymap context**
/// (tui-design.md → "The active context owns the keymap"): the shell routes keys
/// to its [`FormModel`]; `esc` in field-navigation mode pops it. A Sell launches a
/// nested lot-picker context on top of its form. (TUI-ENTRY-FLOW-008)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EntryContext {
    /// The composer form's render + input model. (TUI-ENTRY-FLOW-008/009)
    pub form: FormModel,
    /// The typed composer — the source of truth for *what is composed*, re-parsed
    /// from the edited form fields at submit. `None` for a context built render-only
    /// (the legacy `form`/`with_picker` constructors the render tests use); the live
    /// loop opens contexts WITH a composer so its submit can re-parse + compose.
    /// (TUI-ENTRY-FLOW-008)
    pub composer: Option<Composer>,
    /// The lot picker, when this context is a Sell's specific-ID picker. The picker
    /// renders as a Table with a footer band; `None` for the plain composer forms.
    /// When a `composer` is present the picker lives ON the composer's `SellForm`;
    /// this mirror is kept in sync for rendering. (TUI-ENTRY-LOT-006)
    pub picker: Option<LotPicker>,
    /// The focused picker row index (the table caret), when a picker is present.
    /// (TUI-ENTRY-LOT-006)
    pub picker_focus: usize,
    /// The live gain/tax preview rendered in the picker footer, when computed.
    /// (TUI-ENTRY-LOT-005/006)
    pub preview: Option<entry::GainTaxPreview>,
    /// The composed event **frozen at first submit** — re-submitted byte-identical on
    /// `[r]etry`, re-deriving no field (no fresh `today`, no re-resolved alias)
    /// between attempts, so the content-hash `EventId` is identical across attempts
    /// and the retry is recognised as the same event. Cleared when the owner edits a
    /// field after a returned-control submit (a new entry, new content, new id).
    /// (TUI-ENTRY-FLOW-010)
    pub frozen: Option<Candidate>,
}

impl EntryContext {
    /// A plain composer context (no lot picker, no typed composer) — the render-only
    /// constructor the render tests use. (TUI-ENTRY-FLOW-009)
    pub fn form(form: FormModel) -> Self {
        EntryContext {
            form,
            composer: None,
            picker: None,
            picker_focus: 0,
            preview: None,
            frozen: None,
        }
    }

    /// A Sell context with a lot picker rendered as a Table (render-only). (TUI-ENTRY-LOT-006)
    pub fn with_picker(form: FormModel, picker: LotPicker) -> Self {
        EntryContext {
            form,
            composer: None,
            picker: Some(picker),
            picker_focus: 0,
            preview: None,
            frozen: None,
        }
    }

    /// A live composer context built from a typed [`Composer`] — the form is seeded
    /// from the composer (via the `form::for_*` builders) and the Sell's lot picker
    /// is mirrored for rendering. This is the constructor the live loop opens so the
    /// submit can re-parse the edited fields back into the composer + freeze the
    /// composed event. (TUI-ENTRY-FLOW-008)
    pub fn composing(composer: Composer) -> Self {
        let form = match &composer {
            Composer::Buy(f) => FormModel::for_buy(f),
            Composer::Vest(f) => FormModel::for_vest(f),
            Composer::Sell(f) => FormModel::for_sell(f),
            Composer::Split(f) => FormModel::for_split(f),
            Composer::Residency(f) => FormModel::for_residency(f),
        };
        let picker = match &composer {
            Composer::Sell(f) => Some(f.picker.clone()),
            _ => None,
        };
        EntryContext {
            form,
            composer: Some(composer),
            picker,
            picker_focus: 0,
            preview: None,
            frozen: None,
        }
    }

    /// The active lot picker to render/operate — the composer's `SellForm` picker
    /// when this is a live Sell context, else the render-only mirror. The composer's
    /// picker is the source of truth for `compose()`. (TUI-ENTRY-LOT-001/006)
    pub fn picker_ref(&self) -> Option<&LotPicker> {
        match &self.composer {
            Some(Composer::Sell(f)) => Some(&f.picker),
            _ => self.picker.as_ref(),
        }
    }

    /// The active lot picker, mutably (the composer's picker when present). Edits
    /// through this update the picker `compose()` reads. (TUI-ENTRY-LOT-002/003)
    pub fn picker_mut(&mut self) -> Option<&mut LotPicker> {
        match &mut self.composer {
            Some(Composer::Sell(f)) => Some(&mut f.picker),
            _ => self.picker.as_mut(),
        }
    }
}

/// The TUI's Model — the screen stack, the mode, and the theme. The view state +
/// the write path live behind the [`RuntimePort`]; the Model never owns truth, it
/// renders the port's `ViewState` and drives submits through it. (tui-design.md →
/// "App Shell"; the Model–Update–View loop)
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Model {
    pub mode: Mode,
    /// The screen stack — the bottom is the landing Positions screen; drilling
    /// pushes. Always non-empty. (TUI-VIEW-NAV-003)
    pub stack: Vec<Frame>,
    /// The Entry-mode context stack — the active composer form(s). Empty in Views
    /// mode and until a flow is opened. The **top frame is the active keymap
    /// context** (`esc` pops it). (TUI-ENTRY-FLOW-008)
    pub entry: Vec<EntryContext>,
    /// `true` while the modal help overlay is up — pure chrome: the flag changes
    /// what renders over the current screen and which keys the shell routes, and
    /// nothing else. (TUI-VIEW-NAV-011)
    pub help_open: bool,
    /// Each screen's retained landing-frame nav (filter / sort / grouping),
    /// saved when a screen switch replaces the stack so returning to a screen
    /// finds it as it was left — the same per-screen retention ascend honors.
    /// (TUI-VIEW-NAV-014)
    pub screen_nav: Vec<NavState>,
    pub palette: Palette,
}

impl Default for Model {
    fn default() -> Self {
        Model::new()
    }
}

impl Model {
    /// A fresh Model: Views mode, the Positions landing screen, the Ledger palette,
    /// no open Entry context.
    pub fn new() -> Self {
        Model {
            mode: Mode::Views,
            stack: vec![Frame::new(Screen::Positions)],
            entry: Vec::new(),
            help_open: false,
            screen_nav: Vec::new(),
            palette: Palette::ledger(),
        }
    }

    /// Switch directly to a Views screen (the `1`–`5` keys): replace the screen
    /// stack with the target's landing frame. A drilled scope belongs to the
    /// drill that set it and dies with the switch, but each screen's own filter /
    /// sort / grouping are **retained across switches** — the landing frame's nav
    /// is saved on the way out and restored on the way back. Navigation alone
    /// mutates nothing durable. (TUI-VIEW-NAV-014)
    // @spec TUI-VIEW-NAV-014
    pub fn switch_screen(&mut self, screen: Screen) {
        if self.stack.len() == 1 && self.current().nav.screen == screen {
            return;
        }
        // Retain the landing frame's nav for its screen (drilled frames carry
        // contextual scopes that are meaningless on another screen's stack).
        let bottom = self.stack.first().expect("the screen stack is never empty").nav.clone();
        self.screen_nav.retain(|n| n.screen != bottom.screen);
        self.screen_nav.push(bottom);
        let nav = match self.screen_nav.iter().position(|n| n.screen == screen) {
            Some(i) => self.screen_nav.remove(i),
            None => NavState::new(screen),
        };
        self.stack = vec![Frame { nav }];
    }

    /// Toggle the modal help overlay (`?` opens it outside text-input mode;
    /// `esc`/`?` dismiss it). Chrome only: no other Model state moves, nothing is
    /// launched, nothing durable is touched. (TUI-VIEW-NAV-011)
    // @spec TUI-VIEW-NAV-011
    pub fn toggle_help(&mut self) {
        self.help_open = !self.help_open;
    }

    /// The current (top-of-stack) frame.
    pub fn current(&self) -> &Frame {
        self.stack.last().expect("the screen stack is never empty")
    }

    /// The current frame, mutably.
    pub fn current_mut(&mut self) -> &mut Frame {
        self.stack.last_mut().expect("the screen stack is never empty")
    }

    /// Toggle the top-level mode (Entry ⇄ Views) — `[tab]`. Navigation alone
    /// mutates nothing durable. (tui-design.md → "App Shell")
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            Mode::Views => Mode::Entry,
            Mode::Entry => Mode::Views,
        };
    }

    /// Drill down: push a new frame for the drill target, setting its contextual
    /// scope (a breadcrumb). The user filter + sort are per-screen and retained on
    /// ascend; the contextual scope is cleared on ascend. (TUI-VIEW-NAV-003)
    pub fn drill(&mut self, identity: &views::RowIdentity) -> bool {
        let screen = self.current().nav.screen.clone();
        if let Some((target, scope)) = views::drill_target(&screen, identity) {
            let mut nav = NavState::new(target);
            nav.scope = scope;
            self.stack.push(Frame { nav });
            true
        } else {
            false
        }
    }

    /// Ascend: pop the current frame (the contextual scope is cleared with it; the
    /// parent's retained filter/sort survive). Never pops the landing frame.
    /// `esc`. (TUI-VIEW-NAV-003)
    pub fn ascend(&mut self) -> bool {
        if self.stack.len() > 1 {
            self.stack.pop();
            true
        } else {
            false
        }
    }

    /// Refresh the model against the port: re-read marks, then re-resolve the
    /// stacked anchors (dropping dangling frames with a calm notice — TUI-VIEW-NAV-005)
    /// and re-anchor each surviving frame's focus to its row **identity** over the
    /// re-read ordering (so a re-sort under new marks never moves the selection out
    /// from under the owner; a vanished identity falls to the nearest row —
    /// TUI-VIEW-NAV-004). Returns any dropped frames' notices. The refresh order is
    /// re-read → re-resolve stack → re-anchor focus → (re-render by the caller).
    // @spec TUI-VIEW-NAV-008
    pub fn refresh<P: RuntimePort>(&mut self, port: &mut P) -> Vec<String> {
        port.refresh();
        let notices = self.reresolve_stack(port.view());
        let view = port.view();
        for frame in &mut self.stack {
            let new_order = views::screen_identities(view, &frame.nav);
            let (focus, scroll) = views::reanchor_focus(&frame.nav.focus, frame.nav.scroll, &new_order);
            frame.nav.focus = focus;
            frame.nav.scroll = scroll;
        }
        notices
    }

    /// Re-resolve each stacked frame's anchor identity on a refresh / a concurrent
    /// `entry` append: a frame whose drilled-into scope no longer resolves is
    /// **dropped** (replaced by returning to its parent with a calm notice).
    /// Returns any dropped frames' notices, top-down. (TUI-VIEW-NAV-005)
    pub fn reresolve_stack(&mut self, view: &ViewState) -> Vec<String> {
        let mut notices = Vec::new();
        // Walk top-down; drop dangling frames (keep the landing frame).
        while self.stack.len() > 1 {
            let top = self.stack.last().unwrap();
            if views::scope_resolves(&top.nav.scope, view) {
                break;
            }
            // Dangling: the drilled-into lot/accrual is gone. Replace with a calm
            // returning-to-parent notice. (TUI-VIEW-NAV-005)
            let parent = &self.stack[self.stack.len() - 2].nav.screen;
            notices.push(views::dangling_notice(&top.nav.scope, parent));
            self.stack.pop();
        }
        notices
    }

    // =======================================================================
    // The Entry input/keybinding layer (TUI-ENTRY-FLOW-008). The event loop calls
    // these on the active Entry context (the top of the entry stack). All logic is
    // IN the Model — pure + testable; the pt binary wires crossterm to these.
    // =======================================================================

    /// The active Entry context (top of the entry stack), if any. The **top frame
    /// owns the keymap**. (TUI-ENTRY-FLOW-008)
    // @spec TUI-ENTRY-FLOW-008
    pub fn entry_top(&self) -> Option<&EntryContext> {
        self.entry.last()
    }

    /// The active Entry context, mutably.
    pub fn entry_top_mut(&mut self) -> Option<&mut EntryContext> {
        self.entry.last_mut()
    }

    /// Open an Entry context (push it on the entry stack) and switch to Entry mode.
    /// (TUI-ENTRY-FLOW-008)
    pub fn open_entry(&mut self, ctx: EntryContext) {
        self.mode = Mode::Entry;
        self.entry.push(ctx);
    }

    /// `tab` / `↓` — move focus to the next field of the active form. A no-op when
    /// no Entry context is open. (TUI-ENTRY-FLOW-008)
    pub fn entry_focus_next(&mut self) -> FormAction {
        match self.entry.last_mut() {
            Some(ctx) => ctx.form.focus_next(),
            None => FormAction::Consumed,
        }
    }

    /// `shift-tab` / `↑` — move focus to the previous field of the active form.
    /// (TUI-ENTRY-FLOW-008)
    pub fn entry_focus_prev(&mut self) -> FormAction {
        match self.entry.last_mut() {
            Some(ctx) => ctx.form.focus_prev(),
            None => FormAction::Consumed,
        }
    }

    /// Enter **text-input mode** on the focused field — the shell suppresses its
    /// global single-key accelerators. (TUI-ENTRY-FLOW-008)
    pub fn entry_enter_field(&mut self) -> FormAction {
        match self.entry.last_mut() {
            Some(ctx) => ctx.form.enter_field(),
            None => FormAction::Consumed,
        }
    }

    /// Type a character into the focused field of the active composer — and **drop
    /// any frozen candidate**: an edit after a returned-control submit is a NEW entry
    /// (new content, new id), never a byte-identical retry. (TUI-ENTRY-FLOW-008/010)
    // @spec TUI-ENTRY-FLOW-010
    pub fn entry_type_char(&mut self, c: char) {
        if let Some(ctx) = self.entry.last_mut() {
            ctx.form.type_char(c);
            ctx.frozen = None;
        }
    }

    /// Backspace in the focused field of the active composer — also drops the frozen
    /// candidate (an edit invalidates the freeze). (TUI-ENTRY-FLOW-008/010)
    // @spec TUI-ENTRY-FLOW-010
    pub fn entry_backspace(&mut self) {
        if let Some(ctx) = self.entry.last_mut() {
            ctx.form.backspace();
            ctx.frozen = None;
        }
    }

    /// `enter` — submit on the last field, otherwise advance. Returns the action so
    /// the shell runs the submit. (TUI-ENTRY-FLOW-008)
    pub fn entry_enter(&mut self) -> FormAction {
        match self.entry.last_mut() {
            Some(ctx) => ctx.form.enter(),
            None => FormAction::Consumed,
        }
    }

    /// `esc` — leave text-input mode, OR cancel the flow and **pop the context**
    /// (confirm-discard first if any field is dirty). On a clean cancel this method
    /// pops the context itself and returns [`FormAction::Cancel`]; on a dirty
    /// field it returns [`FormAction::ConfirmDiscard`] WITHOUT popping (the shell
    /// raises the confirm, then calls [`Model::entry_discard`] on confirmation).
    /// (TUI-ENTRY-FLOW-008)
    pub fn entry_escape(&mut self) -> FormAction {
        let action = match self.entry.last_mut() {
            Some(ctx) => ctx.form.escape(),
            None => return FormAction::Consumed,
        };
        if action == FormAction::Cancel {
            self.entry_pop();
        }
        action
    }

    /// Discard the active context after a confirm-discard prompt (the dirty-field
    /// `esc` path): pop it. (TUI-ENTRY-FLOW-008)
    pub fn entry_discard(&mut self) {
        self.entry_pop();
    }

    /// Pop the active Entry context; leaving the entry stack empty drops back to
    /// Views mode. (TUI-ENTRY-FLOW-008)
    fn entry_pop(&mut self) {
        self.entry.pop();
        if self.entry.is_empty() {
            self.mode = Mode::Views;
        }
    }

    /// Whether the active Entry context is in text-input mode — the shell consults
    /// this to **suppress its global single-key accelerators** (so a typed key is
    /// never intercepted as a command). (TUI-ENTRY-FLOW-008)
    pub fn entry_in_text_input(&self) -> bool {
        self.entry
            .last()
            .map(|c| c.form.input_mode == InputMode::TextInput)
            .unwrap_or(false)
    }

    // =======================================================================
    // Flow launch (TUI-ENTRY-FLOW-008). Open a typed composer context from the live
    // loop — the missing half of the keymap: a key sequence from the landing screen
    // now reaches an OPEN composer with its per-flow fields seeded.
    // =======================================================================

    /// Open a live composer context from a typed [`entry::Composer`] — switch to
    /// Entry mode and push the context (form seeded from the composer, the Sell's lot
    /// picker mirrored). The live submit re-parses the edited fields back into this
    /// composer + freezes the composed event. (TUI-ENTRY-FLOW-008)
    // @spec TUI-ENTRY-FLOW-008
    pub fn open_composer(&mut self, composer: Composer) {
        self.open_entry(EntryContext::composing(composer));
    }

    /// Open a Buy composer for `typed_symbol`, seeded with the flow defaults through
    /// the port (date today, platform from `config`, symbol via the alias table).
    /// (TUI-ENTRY-ACT-001; TUI-ENTRY-FLOW-007/008)
    pub fn open_buy<P: RuntimePort>(
        &mut self,
        port: &P,
        lot_id: String,
        typed_symbol: &str,
        platforms: &config::PlatformList,
        aliases: &config::AliasMap,
    ) {
        let form = entry::BuyForm::with_defaults(port, lot_id, typed_symbol, platforms, aliases);
        self.open_composer(Composer::Buy(form));
    }

    /// Open a Vest composer. (TUI-ENTRY-ACT-002; TUI-ENTRY-FLOW-007/008)
    pub fn open_vest<P: RuntimePort>(
        &mut self,
        port: &P,
        lot_id: String,
        typed_symbol: &str,
        platforms: &config::PlatformList,
        aliases: &config::AliasMap,
    ) {
        let form = entry::VestForm::with_defaults(port, lot_id, typed_symbol, platforms, aliases);
        self.open_composer(Composer::Vest(form));
    }

    /// Open a Sell composer with its lot picker built live from the snapshot.
    /// (TUI-ENTRY-ACT-003; TUI-ENTRY-FLOW-007/008)
    pub fn open_sell<P: RuntimePort>(
        &mut self,
        port: &P,
        sale_id: String,
        typed_symbol: &str,
        residency: &config::ResidencyTimeline,
        platforms: &config::PlatformList,
        aliases: &config::AliasMap,
    ) {
        let form =
            entry::SellForm::with_defaults(port, sale_id, typed_symbol, residency, platforms, aliases);
        self.open_composer(Composer::Sell(form));
    }

    /// Open a Split composer. (TUI-ENTRY-ACT-004; TUI-ENTRY-FLOW-008)
    pub fn open_split(&mut self, typed_symbol: &str, date: Date) {
        let form = entry::SplitForm {
            symbol: typed_symbol.to_string(),
            ratio_num: 1,
            ratio_den: 1,
            date,
        };
        self.open_composer(Composer::Split(form));
    }

    // =======================================================================
    // The lot-picker keymap (TUI-ENTRY-LOT-001/002/003/006). The picker context is
    // active whenever the top composer carries a picker; these advance the focus
    // caret, fill FIFO, and set a take on the active row — the missing live operation
    // of the signature interaction.
    // =======================================================================

    /// `true` when the active Entry context has an operable lot picker (a Sell).
    /// (TUI-ENTRY-LOT-006)
    pub fn entry_has_picker(&self) -> bool {
        self.entry.last().map(|c| c.picker_ref().is_some()).unwrap_or(false)
    }

    /// `true` when the active composer is holding on a new-symbol guard (`[c]` is
    /// live to confirm the new position). (TUI-ENTRY-ACT-006)
    pub fn entry_phase_confirm_new_symbol(&self) -> bool {
        self.entry
            .last()
            .map(|c| matches!(c.form.phase, Phase::ConfirmNewSymbol(_)))
            .unwrap_or(false)
    }

    /// Move the picker focus caret down one row (capped at the last row); a no-op
    /// when no picker is open. (TUI-ENTRY-LOT-006)
    pub fn picker_focus_next(&mut self) {
        if let Some(ctx) = self.entry.last_mut() {
            let len = ctx.picker_ref().map(|p| p.lots.len()).unwrap_or(0);
            if len > 0 {
                ctx.picker_focus = (ctx.picker_focus + 1).min(len - 1);
            }
        }
    }

    /// Move the picker focus caret up one row (capped at row 0). (TUI-ENTRY-LOT-006)
    pub fn picker_focus_prev(&mut self) {
        if let Some(ctx) = self.entry.last_mut() {
            ctx.picker_focus = ctx.picker_focus.saturating_sub(1);
        }
    }

    /// FIFO-fill the active picker (oldest-first up to the sale qty) — the `[F]`
    /// accelerator. (TUI-ENTRY-LOT-001)
    pub fn picker_fill_fifo(&mut self) {
        if let Some(ctx) = self.entry.last_mut() {
            if let Some(p) = ctx.picker_mut() {
                p.fill_fifo();
            }
        }
    }

    /// Set the take on the active (focused) picker row, capped at its remaining
    /// (a greyed `rem 0` row stays unallocatable). (TUI-ENTRY-LOT-002/004)
    pub fn picker_set_take_on_focus(&mut self, take: MicroShares) {
        if let Some(ctx) = self.entry.last_mut() {
            let focus = ctx.picker_focus;
            if let Some(p) = ctx.picker_mut() {
                if let Some(lot_id) = p.lots.get(focus).map(|l| l.lot_id.clone()) {
                    p.set_take(&lot_id, take);
                }
            }
        }
    }

    // =======================================================================
    // The live submit driver (TUI-ENTRY-FLOW-002/003/004/005/010, ACT-006,
    // CONFIG-RESIDENCY-005). Re-parse the edited fields into the typed composer,
    // compose (or reuse the FROZEN candidate on a retry), submit through the port,
    // and fold the returned Phase back onto the form — clearing the context on
    // confirmed durability.
    // =======================================================================

    /// Run the live submit for the active composer through `port` (the wiring of
    /// `FormAction::Submit`). Re-parse the edited field strings into the typed
    /// composer; on a parse error pin it beside the offending field (writing
    /// nothing). Otherwise compose — or, when the prior submit returned control (a
    /// `Retry`/`ConfirmNewSymbol` phase with a frozen candidate), **reuse the frozen
    /// byte-identical candidate** so the content-hash id is stable across attempts
    /// (TUI-ENTRY-FLOW-010). Submit through the port (gated by the new-symbol guard
    /// for Buy/Vest — ACT-006 — and the residency guard for Sell —
    /// CONFIG-RESIDENCY-005), then fold the returned `Phase` onto the form; a
    /// `Confirmed` clears the context (TUI-ENTRY-FLOW-003). `aliases` resolves the
    /// new-symbol guard. (TUI-ENTRY-FLOW-002/003/004/005/010)
    // @spec TUI-ENTRY-FLOW-002, TUI-ENTRY-FLOW-003, TUI-ENTRY-FLOW-004, TUI-ENTRY-FLOW-005, TUI-ENTRY-FLOW-010, TUI-ENTRY-ACT-006, CONFIG-RESIDENCY-005
    pub fn entry_submit<P: RuntimePort>(&mut self, port: &mut P, aliases: &config::AliasMap) {
        let Some(ctx) = self.entry.last_mut() else { return };
        let Some(composer) = ctx.composer.as_mut() else { return };

        // A frozen candidate from a prior returned-control submit re-submits
        // byte-identical — no re-parse, no re-compose, no re-derived field.
        // (TUI-ENTRY-FLOW-010)
        if let Some(frozen) = ctx.frozen.clone() {
            let phase = submit_candidate(port, &frozen);
            apply_phase(ctx, phase);
            if ctx.form.phase == Phase::Confirmed {
                self.entry_pop();
            }
            return;
        }

        // A fresh submit: re-parse the edited fields into the composer first. A bad
        // field pins the inline error beneath it and writes nothing. (TUI-ENTRY-FLOW-008)
        let fields = ctx.form.fields.clone();
        if let Err((field, err)) = composer.apply_fields(&fields) {
            ctx.form.set_error(field, &err);
            ctx.form.set_phase(Phase::Rejected(err));
            return;
        }

        // Gate + submit through the port, mapping the outcome to a Phase. The Buy/Vest
        // new-symbol guard (ACT-006) and the Sell residency guard (CONFIG-RESIDENCY-005)
        // run inside the per-flow `entry::submit_*`.
        let phase = match composer {
            Composer::Buy(f) => entry::submit_buy(port, f, aliases),
            Composer::Vest(f) => entry::submit_vest(port, f, aliases),
            Composer::Sell(f) => entry::submit_sell(port, f),
            Composer::Split(f) => entry::submit_ledger(port, &f.compose()),
            // Residency is a config edit, not an event-log append — composed/validated
            // by `config`, handled by its own submit path; here it confirms inertly.
            Composer::Residency(_) => Phase::Confirmed,
        };

        // FREEZE the composed candidate when the submit returned control (a retry /
        // new-symbol hold) so the next attempt re-submits it byte-identical
        // (TUI-ENTRY-FLOW-010). A clean Confirmed clears; a Rejected (kernel
        // disagreement) is a NEW entry on the next edit, so it is not frozen.
        match &phase {
            Phase::Retry(_) | Phase::ConfirmNewSymbol(_) => {
                ctx.frozen = composer.compose();
            }
            _ => ctx.frozen = None,
        }
        apply_phase(ctx, phase);
        if ctx.form.phase == Phase::Confirmed {
            self.entry_pop();
        }
    }

    /// Confirm a held new-symbol guard (`[c]`) and re-submit the frozen Buy/Vest as a
    /// deliberate new position. (TUI-ENTRY-ACT-006)
    pub fn entry_confirm_new_symbol<P: RuntimePort>(&mut self, port: &mut P, aliases: &config::AliasMap) {
        if let Some(ctx) = self.entry.last_mut() {
            if let Some(composer) = ctx.composer.as_mut() {
                match composer {
                    Composer::Buy(f) => f.confirmed_new_symbol = true,
                    Composer::Vest(f) => f.confirmed_new_symbol = true,
                    _ => {}
                }
            }
            // The held guard is not a frozen-content retry (confirming opens a new
            // position): drop the freeze and resubmit fresh.
            ctx.frozen = None;
        }
        self.entry_submit(port, aliases);
    }
}

/// Submit a composed candidate through the port, mapping the outcome to a `Phase`.
/// (TUI-ENTRY-FLOW-002/003/004/005)
fn submit_candidate<P: RuntimePort>(port: &mut P, candidate: &Candidate) -> Phase {
    match candidate {
        Candidate::Ledger(e) => entry::submit_ledger(port, e),
        Candidate::Tax(e) => entry::submit_tax(port, e),
    }
}

/// Fold a submit `Phase` onto the form: record the phase (so the footer offers the
/// matching `[r]`/`[c]`/`[enter]` accelerator) and, on a `Rejected`, pin the kernel
/// error beneath the focused field. A `Confirmed`/`Retry`/`ConfirmNewSymbol` leaves
/// the entry intact for the caller to clear or retry. (TUI-ENTRY-FLOW-002/004/005)
fn apply_phase(ctx: &mut EntryContext, phase: Phase) {
    if let Phase::Rejected(err) = &phase {
        let field = ctx.form.focus;
        ctx.form.set_error(field, err);
    }
    ctx.form.set_phase(phase);
}

// ===========================================================================
// The status line + masthead motifs (tui-design.md → "Status line" / "Masthead").
// A `·`-segmented bottom line: connection dot, last sync, lock state, current
// screen — chrome in fg-faint/accent.
// ===========================================================================

/// The persistent status line state. (tui-design.md → "Status line")
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StatusLine {
    /// `true` when connected (live); `false` when offline (stale). The connection
    /// dot renders `●` connected / `○` stale. (tui-design.md → "Status line")
    pub connected: bool,
    /// `true` when the advisory write-lock is held (a cron `summary`) — the `🔒`
    /// state, surfaced so a write rejection is not a surprise. (TUI-VIEW-NAV-007)
    pub lock_held: bool,
    /// The last-sync label: the priced trading day as a calendar date (`as-of
    /// 2026-06-09`), or `no priced day yet` — never a raw key int. (TUI-VIEW-NAV-012)
    pub last_sync: String,
    /// The wall-clock `HH:MM` of the run/refresh that produced the view, threaded
    /// by the binary; the segment is omitted when unknown. (TUI-VIEW-NAV-013)
    pub updated: Option<String>,
    /// The current mode + screen label.
    pub screen: String,
    /// `true` when a bracket-staleness reminder is active (the `⚠` brackets-stale
    /// nudge). (entry-design.md → bracket staleness reminder)
    pub brackets_stale: bool,
}

impl StatusLine {
    /// Build the status line from the view state + the current mode/screen.
    // @spec TUI-VIEW-NAV-012, TUI-VIEW-NAV-013
    pub fn from_view(view: &ViewState, mode: Mode, screen: &Screen) -> Self {
        let connected = matches!(view.connection, port::Connection::Live);
        let last_sync = view
            .as_of_calendar
            .clone()
            .map(|cal| format!("as-of {cal}"))
            .unwrap_or_else(|| "no priced day yet".to_string());
        StatusLine {
            connected,
            lock_held: false,
            last_sync,
            updated: view.updated_hhmm.clone(),
            screen: format!("{mode:?} · {}", screen.title()),
            brackets_stale: !view.staleness.is_empty(),
        }
    }

    /// The rendered `·`-segmented status line text. (tui-design.md → "Status line")
    pub fn text(&self) -> String {
        let dot = if self.connected { theme::GLYPH_STATUS_ON } else { theme::GLYPH_STATUS_OFF };
        let lock = if self.lock_held {
            format!("{} lock held", theme::GLYPH_LOCK_HELD)
        } else {
            theme::GLYPH_LOCK_FREE.to_string()
        };
        let conn = if self.connected { "connected" } else { "stale (offline)" };
        // The `updated HH:MM` wall-clock segment, omitted (never fabricated)
        // when no run time is available. (TUI-VIEW-NAV-013)
        let updated = self
            .updated
            .as_ref()
            .map(|t| format!(" · updated {t}"))
            .unwrap_or_default();
        let mut s = format!(
            "{dot} {conn} · sync {}{updated} · {lock} · {}",
            self.last_sync, self.screen
        );
        if self.brackets_stale {
            s.push_str(&format!(" · {} brackets stale", theme::GLYPH_WARN));
        }
        s
    }
}

// ===========================================================================
// Rendering to a Buffer (the testable view layer). The Model–Update–View loop's
// VIEW projects the port's ViewState + the Model onto a ratatui Buffer; tests use
// a `TestBackend` Buffer and assert on rendered cells, never a live TTY.
// (tui-design.md → "ratatui + crossterm, immediate-mode")
// ===========================================================================

/// Project the current Model + the port's `ViewState` onto a fresh `ratatui`
/// [`Buffer`] of the given `width`×`height` — the VIEW half of the loop, fully
/// testable with no TTY. Renders the masthead, the active screen (or the loud
/// integrity block / a calm empty state), and the status line, all in the Ledger
/// language. (tui-design.md → "App Shell" / "Cross-Screen Display Conventions")
pub fn render_to_buffer<P: RuntimePort>(model: &Model, port: &P, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(model, port, area, &mut buf);
    buf
}

/// Render the Model onto an existing buffer area (the immediate-mode draw). The
/// masthead band at the top, the active region in the middle, the status line at
/// the bottom. (tui-design.md → "Density" / "Masthead" / "Status line")
pub fn render<P: RuntimePort>(model: &Model, port: &P, area: Rect, buf: &mut Buffer) {
    let pal = &model.palette;
    let view = port.view();

    // --- Masthead band: tracked uppercase gilt at the top, with the right-aligned
    // as-of / quote-epoch — the header band's freshness, always the same spot.
    // Offline renders the stale `⟲ as-of` per the conventions. (tui-design.md →
    // "Masthead" / "Header band" / "Freshness")
    let half = area.width / 2;
    let masthead_area = Rect::new(area.x, area.y, half, 1);
    Paragraph::new(MASTHEAD)
        .style(pal.style(Role::Accent))
        .render(masthead_area, buf);
    let (as_of, as_of_role) = header_as_of(view);
    let as_of_area = Rect::new(area.x + half, area.y, area.width - half, 1);
    Paragraph::new(as_of)
        .alignment(ratatui::layout::Alignment::Right)
        .style(pal.style(as_of_role))
        .render(as_of_area, buf);

    // --- Status line: the `·`-segmented bottom line; the connection dot carries
    // its role (warn when stale) beside the redundant word. (tui-design.md →
    // "Status line")
    if area.height >= 2 {
        let status = StatusLine::from_view(view, model.mode, &model.current().nav.screen);
        let status_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let text = status.text();
        let mut chars = text.chars();
        let dot: String = chars.next().map(|c| c.to_string()).unwrap_or_default();
        let rest: String = chars.collect();
        let dot_role = if status.connected { Role::Accent } else { Role::Warn };
        let line = ratatui::text::Line::from(vec![
            ratatui::text::Span::styled(dot, pal.style(dot_role)),
            ratatui::text::Span::styled(rest, pal.style(Role::FgFaint)),
        ]);
        Paragraph::new(line).render(status_area, buf);

        // The active context's key hints ride the status line's right edge —
        // bracketed keys in accent, words in fg-faint, only keys the context
        // binds; suppressed entirely in text-input mode (the field owns every
        // keystroke). (views-design.md → "Status-line key hints")
        // @spec TUI-VIEW-NAV-010
        let hints = key_hints(model);
        if !hints.is_empty() {
            let line = hint_line(&hints, pal);
            let hint_w: usize = line.width();
            let status_w = char_w(&text);
            if status_w + 2 + hint_w <= area.width as usize {
                Paragraph::new(line)
                    .alignment(ratatui::layout::Alignment::Right)
                    .render(status_area, buf);
            }
        }
    }

    // --- The active region (between masthead and status line).
    let body_top = area.y + 1;
    let body_height = area.height.saturating_sub(2);
    if body_height == 0 {
        return;
    }
    let body = Rect::new(area.x, body_top, area.width, body_height);

    // Integrity error blocks the screen with the loud `✗` treatment — refuse to
    // render derived numbers. (tui-design.md → "Integrity errors")
    if let Some(err) = &view.integrity {
        render_integrity_block(pal, err, body, buf);
    } else {
        match model.mode {
            Mode::Views => render_views(model, view, pal, body, buf),
            Mode::Entry => render_entry_shell(model, pal, body, buf),
        }
    }

    // The modal help overlay rides OVER the current screen — chrome only.
    // (views-design.md → "Help Overlay")
    if model.help_open {
        render_help_overlay(pal, body, buf);
    }
}

// ===========================================================================
// Status-line key hints + the help overlay (views-design.md → "Status-line key
// hints" / "Help Overlay"). The hints show only keys the active context binds;
// `?` opens the modal overlay with the full keymap grouped by context.
// ===========================================================================

/// The active context's key hints — `(key, word)` pairs for the status line's
/// right edge, hinting ONLY keys the active context binds, and updating as the
/// context changes (Views landing/drilled, the idle Entry panel, an open
/// composer, a lot picker). Empty while a field is in text-input mode (the field
/// owns every keystroke) and reduced to the dismiss key while the help overlay is
/// up. (views-design.md → "Status-line key hints")
// @spec TUI-VIEW-NAV-010, TUI-VIEW-NAV-015
pub fn key_hints(model: &Model) -> Vec<(&'static str, &'static str)> {
    if model.entry_in_text_input() {
        return Vec::new();
    }
    if model.help_open {
        return vec![("esc", "close help")];
    }
    match model.mode {
        Mode::Entry => match model.entry_top() {
            // The idle Entry panel: the flow-launch keys.
            None => vec![
                ("b", "buy"),
                ("v", "vest"),
                ("s", "sell"),
                ("x", "split"),
                ("tab", "views"),
                ("?", "help"),
            ],
            Some(ctx) => {
                if ctx.picker_ref().is_some() {
                    vec![
                        ("↑/↓", "row"),
                        ("0-9", "take"),
                        ("F", "fifo"),
                        ("enter", "submit"),
                        ("esc", "cancel"),
                        ("?", "help"),
                    ]
                } else {
                    vec![
                        ("tab", "field"),
                        ("enter", "submit"),
                        ("esc", "cancel"),
                        ("?", "help"),
                    ]
                }
            }
        },
        Mode::Views => {
            // `1`–`5` are bound on EVERY Views frame (drilled or not), so the
            // between-screens hint is truthful everywhere in Views.
            // (TUI-VIEW-NAV-014/015)
            let mut hints =
                vec![("tab", "entry"), ("1-5", "screens"), ("r", "refresh"), ("?", "help")];
            if model.stack.len() > 1 {
                hints.push(("esc", "back"));
            } else {
                hints.push(("q", "quit"));
            }
            hints
        }
    }
}

/// Style the hints as a `ratatui` line: bracketed keys accent, words fg-faint.
/// (views-design.md → "Status-line key hints")
fn hint_line(hints: &[(&str, &str)], pal: &Palette) -> ratatui::text::Line<'static> {
    let mut spans: Vec<ratatui::text::Span<'static>> = Vec::new();
    for (i, (key, word)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(ratatui::text::Span::styled("  ".to_string(), pal.style(Role::FgFaint)));
        }
        spans.push(ratatui::text::Span::styled(format!("[{key}]"), pal.style(Role::Accent)));
        spans.push(ratatui::text::Span::styled(format!(" {word}"), pal.style(Role::FgFaint)));
    }
    ratatui::text::Line::from(spans)
}

/// The full keymap, grouped by context — the help overlay's content. The
/// **screens** group opens the overlay, naming each screen beside the key that
/// reaches it, so navigation is discoverable from the overlay alone; the lot
/// picker group names its host flow (`lot picker (inside a Sell)`) so the
/// context is identifiable without prior knowledge. (views-design.md → "Help
/// Overlay")
// @spec TUI-VIEW-NAV-015
pub fn help_keymap() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        (
            "screens",
            vec![
                ("1", "Positions"),
                ("2", "Open Lots"),
                ("3", "History"),
                ("4", "Tax & Reserves"),
                ("5", "Realized"),
            ],
        ),
        (
            "global",
            vec![
                ("tab", "switch entry ⇄ views"),
                ("r", "refresh marks"),
                ("?", "help (open/close)"),
                ("q", "quit — at the landing screen"),
            ],
        ),
        (
            "views",
            vec![
                ("esc", "ascend a drilled frame · quit at the landing screen"),
            ],
        ),
        (
            "entry",
            vec![
                ("b / v / s / x", "open Buy / Vest / Sell / Split (idle panel)"),
                ("tab / ↓", "next field"),
                ("shift-tab / ↑", "previous field"),
                ("enter", "advance · submit on the last field"),
                ("esc", "leave the field · cancel the flow"),
                ("c", "confirm a new symbol (held guard)"),
                ("typing", "edit the focused field"),
            ],
        ),
        (
            "lot picker (inside a Sell)",
            vec![
                ("↑ / ↓", "move the row caret"),
                ("0-9", "set a take on the active row"),
                ("F", "FIFO-fill the allocation"),
                ("enter", "submit the sell"),
                ("esc", "cancel"),
            ],
        ),
    ]
}

/// Render the modal help overlay: a calm centered panel over the current screen,
/// gilt title, a two-column key/action listing grouped by context. Chrome only —
/// it renders from the keymap alone and mutates nothing. (views-design.md →
/// "Help Overlay")
// @spec TUI-VIEW-NAV-011
fn render_help_overlay(pal: &Palette, area: Rect, buf: &mut Buffer) {
    let groups = help_keymap();
    let content_lines: u16 = groups
        .iter()
        .map(|(_, entries)| 1 + entries.len() as u16 + 1)
        .sum::<u16>()
        .saturating_sub(1); // no trailing blank after the last group
    let h = (content_lines + 2).min(area.height).max(3);
    let w = 64.min(area.width.saturating_sub(2)).max(20);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let panel = Rect::new(x, y, w, h);
    ratatui::widgets::Clear.render(panel, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(pal.style(Role::Accent))
        .title(ratatui::text::Span::styled(" Help — the keymap ", pal.style(Role::Accent)));
    let inner = block.inner(panel);
    block.render(panel, buf);

    // Two columns: the key (accent) in a fixed-width left column, the action (fg)
    // right — grouped under tracked-caps context headers.
    let key_col = 17usize;
    let mut lines: Vec<ratatui::text::Line> = Vec::new();
    for (i, (group, entries)) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(ratatui::text::Line::default());
        }
        lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
            group.to_uppercase(),
            pal.style(Role::FgFaint),
        )));
        for (key, action) in entries {
            let bracketed = format!("  [{key}]");
            let pad = key_col.saturating_sub(char_w(&bracketed)).max(1);
            lines.push(ratatui::text::Line::from(vec![
                ratatui::text::Span::styled(bracketed, pal.style(Role::Accent)),
                ratatui::text::Span::styled(" ".repeat(pad), pal.style(Role::Fg)),
                ratatui::text::Span::styled((*action).to_string(), pal.style(Role::Fg)),
            ]));
        }
    }
    Paragraph::new(lines).render(inner, buf);
}

/// The header band's right-aligned as-of text + role: the freshness context every
/// screen carries in the same spot, as a **calendar date** (`as-of 2026-06-09` —
/// the formatted string the binary threads through the view bundle; the TUI never
/// renders a raw trading-day key int). Offline renders the `⟲ as-of` stale marking
/// (never presented as live); a book with no priced trading day reads
/// `no priced day yet`. (tui-design.md → "Header band" / "Freshness")
// @spec TUI-VIEW-NAV-012
fn header_as_of(view: &port::ViewState) -> (String, Role) {
    match (&view.connection, &view.as_of_calendar) {
        (port::Connection::Offline, Some(cal)) => {
            (format!("{} as-of {cal}", theme::GLYPH_STALE), Role::Stale)
        }
        (port::Connection::Offline, None) => {
            (format!("{} no priced day yet", theme::GLYPH_STALE), Role::Stale)
        }
        (port::Connection::Live, Some(cal)) => (format!("as-of {cal}"), Role::FgDim),
        (port::Connection::Live, None) => ("no priced day yet".to_string(), Role::FgDim),
    }
}

/// Render the loud integrity block: `✗` + a blocking message in the `error` role,
/// no derived numbers. (tui-design.md → "Integrity errors")
fn render_integrity_block(pal: &Palette, err: &port::IntegrityError, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(pal.style(Role::Error))
        .title("INTEGRITY ERROR");
    let inner = block.inner(area);
    block.render(area, buf);
    let msg = format!("{} {}", theme::GLYPH_ERROR, err.message());
    Paragraph::new(msg).style(pal.style(Role::Error)).render(inner, buf);
}

/// Render the active `views` screen into the body, in a focused gilt-bordered
/// panel. The actual rows + headers are projected from the port's view state.
fn render_views(model: &Model, view: &ViewState, pal: &Palette, area: Rect, buf: &mut Buffer) {
    let nav = &model.current().nav;
    // The panel title: the design's screen name, with the drill breadcrumb chip
    // riding it (`Open Lots · AMZN ›`). (views-design.md; TUI-VIEW-NAV-003)
    let crumb = nav.scope.breadcrumb();
    let title = if crumb.is_empty() {
        format!(" {} ", nav.screen.title())
    } else {
        format!(" {} · {crumb} ", nav.screen.title())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        // The focused panel's border turns gilt. (tui-design.md → "Panels & rules")
        .border_style(pal.style(Role::Accent))
        .title(title);
    let inner = block.inner(area);
    block.render(area, buf);

    let lines = screen_lines(view, nav);
    for (i, line) in lines.iter().enumerate() {
        if (i as u16) >= inner.height {
            break;
        }
        let row = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
        // The focused row (the one carrying the `▎` caret) renders over the
        // bg-focus chrome token, across the whole row. (tui-design.md → "Focus caret")
        let mut row_style = ratatui::style::Style::default();
        if line.text().starts_with(theme::GLYPH_FOCUS) {
            if let Some(bg) = pal.focus_bg() {
                row_style = row_style.bg(bg);
            }
        }
        Paragraph::new(line.to_ratatui(pal)).style(row_style).render(row, buf);
    }
}

// ===========================================================================
// The spans seam (views-design.md → "Line Rendering"). Each screen line is an
// ordered run of styled segments — per-segment role + emphasis — so one line can
// carry the parent motifs without flattening them to a single colour: a faint
// tracked-caps label beside its bold figure, ramp-coloured stepper dots beside fg
// row text, a delta in its gain/loss role, a brightened sparkline today-tick.
// ===========================================================================

/// One styled segment of a rendered screen line. (views-design.md → "Line Rendering")
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Seg {
    pub text: String,
    pub role: Role,
    /// Render bold — the type-hierarchy emphasis on a tracked-caps figure, and the
    /// sparkline's brightened today-tick. (tui-design.md → "Tracked-caps labels" /
    /// "Sparklines")
    pub bold: bool,
}

impl Seg {
    /// A regular-weight segment in a role.
    pub fn new(text: impl Into<String>, role: Role) -> Self {
        Seg { text: text.into(), role, bold: false }
    }

    /// A bold segment in a role (figures beside tracked-caps labels; the today-tick).
    pub fn bold(text: impl Into<String>, role: Role) -> Self {
        Seg { text: text.into(), role, bold: true }
    }
}

/// One rendered screen line: an ordered run of styled segments. The flat
/// [`ScreenLine::text`] is what the terminal cells show; the segments carry the
/// per-column roles. (views-design.md → "Line Rendering")
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScreenLine {
    pub spans: Vec<Seg>,
}

impl ScreenLine {
    /// A single-role line (chrome, headers, notices, empty states).
    pub fn plain(text: impl Into<String>, role: Role) -> Self {
        ScreenLine { spans: vec![Seg::new(text, role)] }
    }

    /// A line from explicit segments.
    pub fn from_spans(spans: Vec<Seg>) -> Self {
        ScreenLine { spans }
    }

    /// The line's flat text — the concatenated segments.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// The role of the first segment whose text contains `needle` (the assertion
    /// seam for per-segment styling).
    pub fn span_role(&self, needle: &str) -> Option<Role> {
        self.spans.iter().find(|s| s.text.contains(needle)).map(|s| s.role)
    }

    /// The styled `ratatui` line for this screen line at a palette.
    pub fn to_ratatui(&self, pal: &Palette) -> ratatui::text::Line<'static> {
        let spans: Vec<ratatui::text::Span<'static>> = self
            .spans
            .iter()
            .map(|seg| {
                let mut style = pal.style(seg.role);
                if seg.bold {
                    style = style.add_modifier(ratatui::style::Modifier::BOLD);
                }
                ratatui::text::Span::styled(seg.text.clone(), style)
            })
            .collect();
        ratatui::text::Line::from(spans)
    }
}

/// A segment width in display chars (every Ledger glyph is one cell wide).
fn char_w(s: &str) -> usize {
    s.chars().count()
}

/// Right-align into `w` chars.
fn pad_left(s: &str, w: usize) -> String {
    format!("{:>width$}", s, width = w)
}

/// Left-align into `w` chars.
fn pad_right(s: &str, w: usize) -> String {
    format!("{:<width$}", s, width = w)
}

/// A column's width from its rendered cells — the max cell width over the visible
/// rows, clamped to `[min, max]`. (views-design.md → "Column sizing is data-driven")
fn column_width<'a>(cells: impl Iterator<Item = &'a str>, min: usize, max: usize) -> usize {
    cells.map(char_w).max().unwrap_or(0).clamp(min, max)
}

/// A column's width from its rendered cells PLUS its title — the title row is a
/// rendered cell of its column, so it participates in the data-driven width under
/// the same clamps and never shears the grid. (views-design.md → "Column title
/// rows")
fn column_width_titled<'a>(
    cells: impl Iterator<Item = &'a str>,
    title: &str,
    min: usize,
    max: usize,
) -> usize {
    cells
        .map(char_w)
        .chain(std::iter::once(char_w(title)))
        .max()
        .unwrap_or(0)
        .clamp(min, max)
}

/// Ellipsize to `w` chars — ONLY when the clamp forces it. (views-design.md →
/// "Column sizing is data-driven")
fn ellipsize(s: &str, w: usize) -> String {
    if char_w(s) <= w {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(w.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Decimal-align a numeric column: integer parts right-align and fractional parts
/// hang under one shared decimal point; a cell with no fraction pads through the
/// fraction width, so `24,150` and `1.5` align on the point. (views-design.md →
/// "Column sizing is data-driven"; tui-design.md → "Money columns")
fn decimal_align(cells: &[String]) -> Vec<String> {
    let split = |c: &str| -> (String, String) {
        match c.find('.') {
            Some(p) => (c[..p].to_string(), c[p..].to_string()),
            None => (c.to_string(), String::new()),
        }
    };
    let int_w = cells.iter().map(|c| char_w(&split(c).0)).max().unwrap_or(0);
    let frac_w = cells.iter().map(|c| char_w(&split(c).1)).max().unwrap_or(0);
    cells
        .iter()
        .map(|c| {
            let (i, f) = split(c);
            format!("{}{}", pad_left(&i, int_w), pad_right(&f, frac_w))
        })
        .collect()
}

/// The fixed-width trend strip as segments: the captured ramp with the **latest
/// captured cell brightened** (the "today" tick) over the direction tint, then
/// dim-dot padding out to the fixed strip width for the trading days not yet
/// captured — a timeline filling in. An empty ramp renders the full dot strip
/// (captures not yet begun), with no tick. (tui-design.md → "Sparklines";
/// views-design.md → "Trend strip")
// @spec TUI-VIEW-POS-013
fn trend_strip_segs(spark: &str, role: Role) -> Vec<Seg> {
    let mut chars: Vec<char> = spark.chars().collect();
    chars.truncate(theme::SPARK_STRIP_W);
    let pad = theme::SPARK_STRIP_W - chars.len();
    let mut out = Vec::new();
    if let Some(last) = chars.pop() {
        let body: String = chars.into_iter().collect();
        if !body.is_empty() {
            out.push(Seg::new(body, role));
        }
        out.push(Seg::bold(last.to_string(), role));
    }
    if pad > 0 {
        out.push(Seg::new(
            std::iter::repeat(theme::SPARK_DOT).take(pad).collect::<String>(),
            Role::FgFaint,
        ));
    }
    out
}

/// The screen lines for the active screen (the projected, qualified figures) on
/// the spans seam. This is the testable seam: the lines carry the qualifiers /
/// glyphs / per-segment roles the conventions require, and tests assert on them.
/// (TUI-VIEW-*)
pub fn screen_lines(view: &ViewState, nav: &NavState) -> Vec<ScreenLine> {
    let mut out: Vec<ScreenLine> = Vec::new();
    let book_empty = view.snapshot.positions.is_empty();
    let filter_active = !matches!(nav.filter, views::Filter::None);
    match nav.screen {
        Screen::Positions => {
            let (rows, caveat, header) = views::positions_rows(view, nav);
            out.push(ScreenLine::plain(header.text(), Role::FgFaint));
            out.push(ScreenLine::plain(caveat.text(), Role::FgFaint));
            if let Some(empty) = views::empty_state(
                &Screen::Positions,
                header.total,
                header.shown,
                filter_active,
                book_empty,
            ) {
                out.push(ScreenLine::plain(empty.message(), Role::FgDim));
                return out;
            }
            // The live-glance summary band — TOTAL VALUE + NET (POST-TAX) `[est]`
            // in tracked caps, under a double-entry rule. The totals come from
            // `reports`' series-point builder over the current snapshot + marks
            // (views computes nothing). A partial total (degraded symbols) carries
            // the `‡` marker; a fully-unpriced book renders the dash, never `0`.
            // (tui-design.md → "Tracked-caps labels" / "Panels & rules" / example)
            out.extend(positions_summary_band(view, &caveat));
            out.push(ScreenLine::plain(DOUBLE_RULE, Role::FgFaint));
            out.extend(position_lines(&rows, view, nav));
        }
        Screen::OpenLots => {
            let (rows, header) = views::lot_rows(view, nav);
            out.push(ScreenLine::plain(header.text(), Role::FgFaint));
            if let Some(empty) = views::empty_state(
                &Screen::OpenLots,
                header.total,
                header.shown,
                filter_active,
                book_empty,
            ) {
                out.push(ScreenLine::plain(empty.message(), Role::FgDim));
                return out;
            }
            out.extend(lot_lines(&rows, nav));
        }
        Screen::History => {
            let hv = views::history_view(view, nav);
            if !matches!(hv.state, views::HistoryState::Normal) {
                out.push(ScreenLine::plain(hv.state.message(), Role::FgDim));
            }
            // The multi-row block chart, tinted by NET DIRECTION (gain/loss —
            // never chrome) with the latest captured column brightened (the
            // today tick), min/max axis labels, and a date-span line — then a
            // table row per cell with the gap/incomplete conventions.
            // (tui-design.md → "Sparklines"; TUI-VIEW-HIST-001/002/003)
            out.extend(history_chart_lines(&hv));
            for cell in &hv.cells {
                out.push(history_cell_line(cell));
            }
        }
        Screen::TaxReserves => {
            out.push(ScreenLine::plain(views::TAX_PROVENANCE, Role::FgFaint));
            // The next IRS estimated-tax payment period (from `tax`'s quarterly
            // report), so the owner sees when the next remittance is due.
            // (TUI-VIEW-TAX-001)
            if let Some(period) = view.next_estimated_payment_period {
                out.push(ScreenLine::plain(
                    format!("next estimated payment: {period:?}"),
                    Role::Accent,
                ));
            }
            let (rows, reserves, header) = views::tax_rows(view, nav);
            out.push(ScreenLine::plain(header.text(), Role::FgFaint));
            if let Some(empty) = views::empty_state(
                &Screen::TaxReserves,
                header.total,
                header.shown,
                filter_active,
                book_empty,
            ) {
                out.push(ScreenLine::plain(empty.message(), Role::FgDim));
            }
            // The column title row above the accrual rows, aligned to the rows'
            // key / amount / stepper columns. (TUI-VIEW-TAX-003)
            // @spec TUI-VIEW-TAX-003
            if !rows.is_empty() {
                out.push(ScreenLine::from_spans(vec![
                    Seg::new("  ", Role::Fg), // the caret gutter
                    Seg::new(format!("{:<16} ", "SALE/LOT"), Role::FgFaint),
                    Seg::new(format!("{:>12}", "AMOUNT"), Role::FgFaint),
                    Seg::new(" │ ", Role::FgFaint),
                    Seg::new("STATUS", Role::FgFaint),
                ]));
            }
            // Accruals grouped by (jurisdiction, tax_year) — a tracked-caps group
            // header opens each group; the stepper DOTS carry the minting ramp
            // (Accrued faint → Allocated verdigris → Moved gilt → Paid sage) while
            // the row text stays fg; the stepper word is the redundant signal.
            // (TUI-VIEW-TAX-001; tui-design.md → "Color Conventions" / "Lifecycle
            // stepper"; views-design.md → "Line Rendering")
            let mut last_group: Option<(config::Jurisdiction, config::TaxYear)> = None;
            for r in rows {
                let group = (r.key.jurisdiction.clone(), r.key.tax_year);
                if last_group.as_ref() != Some(&group) {
                    out.push(ScreenLine::plain(
                        format!("{} {}", jurisdiction_label(&group.0).to_uppercase(), group.1 .0),
                        Role::FgFaint,
                    ));
                    last_group = Some(group);
                }
                let focused = r.identity == nav.focus;
                let key_label = format!("{}/{}", r.key.sale_id, r.key.lot_id);
                // Tax money is reconciliation money: exact whole dollars, never
                // abbreviated. (TUI-VIEW-TAX-004)
                // @spec TUI-VIEW-TAX-004
                let amount = r
                    .applied
                    .map(theme::money_whole)
                    .unwrap_or_else(|| theme::GLYPH_DASH.to_string());
                let mut spans = vec![
                    caret_seg(focused),
                    Seg::new(format!("{:<16} ", key_label), Role::Fg),
                    Seg::new(format!("{:>12}", amount), Role::Fg),
                    Seg::new(" │ ", Role::FgFaint),
                ];
                match &r.stepper {
                    theme::StepperState::Orphaned => {
                        spans.push(Seg::new(theme::stepper(&r.stepper), Role::Warn));
                    }
                    theme::StepperState::AutoSettled => {
                        spans.push(Seg::new(theme::stepper(&r.stepper), Role::FgDim));
                    }
                    theme::StepperState::Lifecycle(stop) => {
                        spans.push(Seg::new(theme::stepper_dots(*stop), theme::lifecycle_role(*stop)));
                        spans.push(Seg::new(format!(" {}", theme::stepper_label(*stop)), Role::Fg));
                    }
                }
                out.push(ScreenLine::from_spans(spans));
            }
            for res in reserves {
                out.push(ScreenLine::plain(
                    format!(
                        "RESERVE {} {} · accrued {} · moved {} · paid {} · outstanding {} · shortfall {}",
                        jurisdiction_label(&res.jurisdiction),
                        res.tax_year.0,
                        theme::money_whole(res.accrued),
                        theme::money_whole(res.moved),
                        theme::money_whole(res.paid),
                        theme::money_whole(res.outstanding),
                        theme::money_whole(res.shortfall),
                    ),
                    Role::FgDim,
                ));
            }
        }
        Screen::Realized => {
            out.push(ScreenLine::plain(views::REALIZED_PROVENANCE, Role::FgFaint));
            let (rows, header) = views::realized_rows(view, nav);
            out.push(ScreenLine::plain(header.text(), Role::FgFaint));
            if let Some(empty) = views::empty_state(
                &Screen::Realized,
                header.total,
                header.shown,
                filter_active,
                book_empty,
            ) {
                out.push(ScreenLine::plain(empty.message(), Role::FgDim));
                return out;
            }
            out.extend(realized_lines(&rows, nav));
        }
    }
    out
}

/// The rendered lines for the Realized rows — period · proceeds · basis · gain
/// in `│`-ruled, data-driven columns under a tracked-caps title row on the same
/// grid; the gain figure carries its gain/loss/flat role with the always-paired
/// sign/glyph as the redundant cue. (TUI-VIEW-REAL-001/002/003; views-design.md
/// → "Line Rendering" / "Column title rows")
// @spec TUI-VIEW-REAL-002, TUI-VIEW-REAL-003, TUI-VIEW-REAL-004
fn realized_lines(rows: &[views::RealizedRow], nav: &NavState) -> Vec<ScreenLine> {
    struct Cells {
        focused: bool,
        period: String,
        proceeds: String,
        basis: String,
        gain: String,
        gain_role: Role,
    }
    // Realized figures are glance money: the compact whole-dollar form.
    // (TUI-VIEW-REAL-004)
    let cells: Vec<Cells> = rows
        .iter()
        .map(|r| {
            let (gain, gain_role) = theme::delta_text_compact(r.gain);
            Cells {
                focused: r.identity == nav.focus,
                period: r.label.clone(),
                proceeds: theme::money_compact(r.proceeds),
                basis: theme::money_compact(r.basis),
                gain,
                gain_role,
            }
        })
        .collect();

    let period_w =
        column_width_titled(cells.iter().map(|c| c.period.as_str()), "PERIOD", 6, 24);
    let proceeds_w =
        column_width_titled(cells.iter().map(|c| c.proceeds.as_str()), "PROCEEDS", 3, 18);
    let basis_w = column_width_titled(cells.iter().map(|c| c.basis.as_str()), "BASIS", 3, 18);
    let gain_w = column_width_titled(cells.iter().map(|c| c.gain.as_str()), "GAIN", 3, 22);

    let tsep = Seg::new(" │ ", Role::FgFaint);
    let mut out = vec![ScreenLine::from_spans(vec![
        Seg::new("  ", Role::Fg), // the caret gutter
        Seg::new(pad_right("PERIOD", period_w), Role::FgFaint),
        tsep.clone(),
        Seg::new(pad_left("PROCEEDS", proceeds_w), Role::FgFaint),
        tsep.clone(),
        Seg::new(pad_left("BASIS", basis_w), Role::FgFaint),
        tsep,
        Seg::new(pad_left("GAIN", gain_w), Role::FgFaint),
    ])];
    out.extend(cells.iter().map(|c| {
        let sep = Seg::new(" │ ", Role::FgFaint);
        ScreenLine::from_spans(vec![
            caret_seg(c.focused),
            Seg::new(pad_right(&ellipsize(&c.period, period_w), period_w), Role::Fg),
            sep.clone(),
            Seg::new(pad_left(&c.proceeds, proceeds_w), Role::Fg),
            sep.clone(),
            Seg::new(pad_left(&c.basis, basis_w), Role::Fg),
            sep,
            // The gain figure carries its gain/loss/flat role; the sign/glyph is
            // the redundant cue. (TUI-VIEW-REAL-003)
            Seg::new(pad_left(&c.gain, gain_w), c.gain_role),
        ])
    }));
    out
}

/// The double-entry rule under a section header (`═`), clipped to the panel
/// width by the renderer. (tui-design.md → "Panels & rules")
const DOUBLE_RULE: &str = "════════════════════════════════════════════════════════════════════════════════════════════════════════════════════════";

/// The left-gutter focus caret segment for a row: `▎` in accent when focused, a
/// blank gutter otherwise so the columns align. (tui-design.md → "Focus caret")
fn caret_seg(focused: bool) -> Seg {
    if focused {
        Seg::new(format!("{} ", theme::GLYPH_FOCUS), Role::Accent)
    } else {
        Seg::new("  ", Role::Fg)
    }
}

/// A jurisdiction's display label (`Federal` / the state code).
fn jurisdiction_label(j: &config::Jurisdiction) -> String {
    match j {
        config::Jurisdiction::Federal => "Federal".to_string(),
        config::Jurisdiction::State(s) => s.clone(),
    }
}

/// The Positions live-glance summary band: `TOTAL VALUE` with the Δ vs the prior
/// captured trading day, `NET (POST-TAX)` (market value less the estimated
/// tax) with its `[est]` qualifier, and `REALIZED <year> YTD` — the current tax
/// year's calendar-year-to-date realized P&L — tracked-caps labels, never an
/// unqualified partial figure. Totals come from `reports::build_series_point`
/// over the current snapshot/marks/estimates; the prior-day total comes from the
/// captured History series; the realized line from `reports::realized_ytd`. A
/// degenerate priced book renders the dash (never `0`) and a partial
/// (degraded-symbol) total carries `‡` — but the realized line is
/// reconstructable-exact: a year with no sales renders the flat `· $0`, never
/// a dash, and the line survives an unpriced book (it is mark-independent).
/// Band money is reconciliation money: **exact whole dollars, never
/// abbreviated**. (tui-design.md → "A screen in this language" / "Tracked-caps
/// labels" / "Money columns"; TUI-VIEW-POS-001/003/006/010)
// @spec TUI-VIEW-POS-006, TUI-VIEW-POS-008, TUI-VIEW-POS-010
fn positions_summary_band(
    view: &ViewState,
    caveat: &views::CompositionCaveat,
) -> Vec<ScreenLine> {
    let mut out = Vec::new();
    if caveat.shares_na {
        // Priced total ≤ 0: no priced value at all — dash, never a fabricated $0.
        out.push(ScreenLine::from_spans(vec![
            Seg::new("TOTAL VALUE", Role::FgFaint),
            Seg::new(format!("     {}  ", theme::GLYPH_DASH), Role::Degraded),
            Seg::new(theme::Qualifier::Unpriced.marker(), Role::Degraded),
        ]));
        // Realized P&L is mark-independent: the YTD line survives an unpriced
        // book. (TUI-VIEW-POS-006)
        out.push(realized_ytd_line(view));
        return out;
    }
    let key = view
        .trading_day_key
        .unwrap_or(reports::TradingDayKey(pt_core::Date(0)));
    let point = reports::build_series_point(
        &view.snapshot,
        &view.marks,
        &view.estimates,
        key,
        0,
        key.0,
    );
    let partial = if point.incomplete {
        format!(" {}", theme::GLYPH_DEGRADED)
    } else {
        String::new()
    };
    // The tracked-caps label renders faint; the figure beside it is BOLD fg — the
    // monospace type hierarchy. (tui-design.md → "Tracked-caps labels")
    let mut total_spans = vec![
        Seg::new("TOTAL VALUE  ", Role::FgFaint),
        Seg::bold(
            format!("{:>11}", theme::money_whole(point.total_market_value_cents)),
            Role::Fg,
        ),
    ];
    if !partial.is_empty() {
        total_spans.push(Seg::new(partial.clone(), Role::Degraded));
    }
    if let Some((t, role)) = total_day_delta(view, point.total_market_value_cents.0) {
        total_spans.push(Seg::new(format!("   {t}"), role));
    }
    out.push(ScreenLine::from_spans(total_spans));
    // NET (POST-TAX) = basis + net-of-tax unrealized (i.e. market value less the
    // estimated tax) — an estimate, so the `[est]` qualifier travels with it in
    // the estimate role.
    let net = pt_core::Cents(point.total_basis_cents.0 + point.total_unrealized_net_of_tax_cents.0);
    let est = theme::estimate_qualifier(view.bracket_state).marker();
    // The [est]-marked band figure renders in the estimate role beside its
    // qualifier. (TUI-VIEW-POS-008)
    let mut net_spans = vec![
        Seg::new("NET (POST-TAX)  ", Role::FgFaint),
        Seg::bold(format!("{:>8}", theme::money_whole(net)), Role::Estimate),
    ];
    if !partial.is_empty() {
        net_spans.push(Seg::new(partial, Role::Degraded));
    }
    net_spans.push(Seg::new(format!("   {est}"), Role::Estimate));
    out.push(ScreenLine::from_spans(net_spans));
    out.push(realized_ytd_line(view));
    out
}

/// The summary band's `REALIZED <year> YTD` line: the current tax year's
/// calendar-year-to-date realized P&L from `reports::realized_ytd`, rendered with
/// the delta sign/glyph treatment in its gain/loss role, in the band's exact
/// whole dollars. Realized P&L replays exactly from the event log, so a year
/// with no sales is the TRUE flat `· $0` — never the dash the mark-dependent
/// figures degrade to. (TUI-VIEW-POS-006)
// @spec TUI-VIEW-POS-006
fn realized_ytd_line(view: &ViewState) -> ScreenLine {
    let ytd = reports::realized_ytd(&view.snapshot, view.tax_year);
    let (text, role) = theme::delta_text_whole(ytd.gain_cents);
    ScreenLine::from_spans(vec![
        Seg::new(format!("REALIZED {} YTD  ", view.tax_year), Role::FgFaint),
        Seg::bold(text, role),
    ])
}

/// The whole-book Δ vs the prior captured trading day (the latest **complete**
/// History point before today — a partial *total* cannot source an honest
/// book-level delta, and a zero-key garbage point is never a prior), with its
/// percent — `None` when there is no prior complete capture. Rendered in the
/// band's exact whole dollars. (tui-design.md example: `▲ +$8,420  (+0.66%)`)
fn total_day_delta(view: &ViewState, current_total: i64) -> Option<(String, Role)> {
    let today = view.trading_day_key?;
    let prev = view
        .trading_day_calendar
        .iter()
        .rev()
        .filter(|k| **k < today && k.0 .0 != 0)
        .find_map(|k| view.history_points.get(k).filter(|p| !p.incomplete))?;
    let delta = current_total - prev.total_market_value_cents.0;
    let (text, role) = theme::delta_text_whole(pt_core::Cents(delta));
    let pct = if prev.total_market_value_cents.0 != 0 {
        let ppm = ((delta as i128) * 1_000_000 / (prev.total_market_value_cents.0 as i128)) as i64;
        format!("  ({})", theme::signed_percent_ppm(ppm))
    } else {
        String::new()
    };
    Some((format!("{text}{pct}"), role))
}

/// The rendered lines for the Positions rows, with everything TUI-VIEW-POS-001
/// and TUI-VIEW-POS-004/005 list — shares · price · market value · basis ·
/// pre-tax unrealized · gain % of basis · post-tax net `[est]` (the post-tax
/// VALUE, exact whole dollars) · day change ($ and %) · share % · trend strip —
/// in `│`-ruled columns whose
/// widths are computed FROM THE DATA (max cell per column, min/max clamps),
/// decimal-aligned, the symbol ellipsized only when its clamp forces it, and a
/// degraded figure's `—` sitting inside its own column so neighbours never shift.
/// Qualifiers/glyphs ride the conventions (`‡` degraded / `n/a` share, never `0`);
/// basis (reconstructable) renders even on a degraded row, the gain-% cell is
/// `n/a` on a ≤ 0 basis and the dash when degraded, and the day-change cell is
/// the dash until a prior complete capture exists. The `▎` focus caret marks
/// only the focused row; delta segments carry their gain/loss role and the
/// trend strip its direction tint with a brightened today tick over dim-dot
/// padding.
/// (TUI-VIEW-POS-001/003/004/005/012/013; views-design.md → "Line Rendering";
/// tui-design.md → "Money columns" / "Focus caret" / "Sparklines")
// @spec TUI-VIEW-POS-001, TUI-VIEW-POS-003, TUI-VIEW-POS-004, TUI-VIEW-POS-005, TUI-VIEW-POS-007, TUI-VIEW-POS-008, TUI-VIEW-POS-009, TUI-VIEW-POS-010, TUI-VIEW-POS-012, TUI-VIEW-POS-013, TUI-VIEW-NAV-016
fn position_lines(
    rows: &[views::PositionRow],
    view: &ViewState,
    nav: &NavState,
) -> Vec<ScreenLine> {
    let dash = theme::GLYPH_DASH.to_string();
    let est = theme::estimate_qualifier(view.bracket_state).marker();

    // Render every cell first; the widths come from the data.
    struct Cells {
        focused: bool,
        base: Role,
        label: String,
        name: String,
        shares: String,
        price: String,
        mv: String,
        basis: String,
        unreal: String,
        unreal_role: Role,
        gainpct: String,
        gainpct_role: Role,
        net: String,
        net_role: Role,
        est: String,
        delta: String,
        delta_role: Role,
        share: String,
        spark: String,
        spark_role: Role,
        marker: String,
    }
    let cells: Vec<Cells> = rows
        .iter()
        .map(|r| {
            // A degraded row renders dim with `—` in each figure column and the
            // distinct unpriced/degraded word trailing — never a fabricated 0.
            // (tui-design.md → "Degradation")
            let base = if r.degraded { Role::Degraded } else { Role::Fg };
            // Glance figures render in the compact whole-dollar form; per-share
            // PRICE keeps cents. (TUI-VIEW-POS-010, TUI-VIEW-NAV-016)
            let (unreal, unreal_role) = match r.unrealized_pretax {
                Some(c) => {
                    let (t, role) = theme::delta_text_compact(c);
                    (t, role)
                }
                None => (dash.clone(), base),
            };
            // The day-change cell: the $ delta with its always-paired sign/glyph,
            // the % (relative to the prior value) riding the same cell — the dash
            // until a prior complete capture exists. (TUI-VIEW-POS-005)
            let (delta, delta_role) = match r.day_delta {
                Some(c) => {
                    let (t, role) = theme::delta_text_compact(c);
                    let t = match r.day_delta_ppm {
                        Some(ppm) => format!("{t} ({})", theme::signed_percent_ppm(ppm)),
                        None => t,
                    };
                    (t, role)
                }
                None => (dash.clone(), base),
            };
            // The gain-% of basis: `n/a` (reports' wording) on a ≤ 0 basis, the
            // dash when degraded — distinct cases — and, when present, SIGNED and
            // carrying its gain/loss/flat role (the sign is the redundant cue).
            // (TUI-VIEW-POS-004/008)
            let (gainpct, gainpct_role) = if r.gain_pct_na {
                ("n/a".to_string(), base)
            } else {
                match r.gain_pct_ppm {
                    Some(ppm) => {
                        let role = match ppm.cmp(&0) {
                            std::cmp::Ordering::Greater => Role::Gain,
                            std::cmp::Ordering::Less => Role::Loss,
                            std::cmp::Ordering::Equal => Role::Flat,
                        };
                        (theme::signed_percent_ppm(ppm), role)
                    }
                    None => (dash.clone(), base),
                }
            };
            // The post-tax net — the row's post-tax VALUE (market value less the
            // estimated unrealized tax) — is an [est]-marked figure: it renders
            // in the estimate role beside its qualifier, in EXACT whole dollars
            // (reconciliation money — the keep-if-sold-today figure the owner
            // scans, never compacted); a degraded dash keeps the dim role.
            // (TUI-VIEW-POS-008/010/012)
            let (net, net_role) = match r.net_post_tax {
                Some(c) => (theme::money_whole(c), Role::Estimate),
                None => (dash.clone(), base),
            };
            let marker = if r.degraded {
                let word = if r.tax_degraded {
                    theme::Qualifier::Degraded
                } else {
                    theme::Qualifier::Unpriced
                };
                word.marker()
            } else {
                String::new()
            };
            Cells {
                focused: r.identity == nav.focus,
                base,
                label: r.label.clone(),
                // The company-name cell (ticker fallback; blank on a platform
                // group row). (TUI-VIEW-POS-009)
                name: r.name.clone(),
                shares: r.shares.map(theme::shares_grouped).unwrap_or_else(|| dash.clone()),
                price: r.price.map(theme::money).unwrap_or_else(|| dash.clone()),
                mv: r.market_value.map(theme::money_compact).unwrap_or_else(|| dash.clone()),
                // Basis is reconstructable: the figure renders even degraded.
                // (TUI-VIEW-POS-004)
                basis: theme::money_compact(r.basis),
                unreal,
                unreal_role,
                gainpct,
                gainpct_role,
                net,
                net_role,
                est: if r.degraded { String::new() } else { est.clone() },
                delta,
                delta_role,
                share: if r.share_na {
                    "n/a".to_string()
                } else {
                    r.share_ppm.map(theme::percent_ppm).unwrap_or_else(|| dash.clone())
                },
                spark: r.sparkline.clone(),
                spark_role: r.sparkline_role,
                marker,
            }
        })
        .collect();

    // Data-driven widths under clamps: only the symbol/name clamp hard
    // (ellipsized when forced); numeric columns grow with the data so a
    // 24,150-share or a six-figure row never shears the table. Each column's
    // TITLE participates in its width (the title row rides the same grid).
    // (TUI-VIEW-POS-007)
    let label_w = column_width_titled(cells.iter().map(|c| c.label.as_str()), "SYMBOL", 4, 10);
    let name_w = column_width_titled(cells.iter().map(|c| c.name.as_str()), "NAME", 4, 18);
    let shares_cells =
        decimal_align(&cells.iter().map(|c| c.shares.clone()).collect::<Vec<_>>());
    let shares_w = shares_cells
        .iter()
        .map(|s| char_w(s))
        .chain(std::iter::once(char_w("SHARES")))
        .max()
        .unwrap_or(0)
        .max(3);
    let price_w = column_width_titled(cells.iter().map(|c| c.price.as_str()), "PRICE", 3, 16);
    let mv_w = column_width_titled(cells.iter().map(|c| c.mv.as_str()), "VALUE", 3, 18);
    let basis_w = column_width_titled(cells.iter().map(|c| c.basis.as_str()), "BASIS", 3, 18);
    let unreal_w = column_width_titled(cells.iter().map(|c| c.unreal.as_str()), "UNREAL", 3, 20);
    let gainpct_w =
        column_width_titled(cells.iter().map(|c| c.gainpct.as_str()), "GAIN%", 3, 10);
    let net_w = column_width(cells.iter().map(|c| c.net.as_str()), 3, 18);
    let est_w = cells.iter().map(|c| char_w(&c.est)).max().unwrap_or(0);
    let delta_w = column_width_titled(cells.iter().map(|c| c.delta.as_str()), "DAY Δ", 3, 30);
    let share_w = column_width_titled(cells.iter().map(|c| c.share.as_str()), "SHARE", 3, 7);

    // The column title row: tracked-caps fg-faint titles on the SAME grid as the
    // rows — the `│` rules at identical positions. (TUI-VIEW-POS-007)
    let title_sep = Seg::new(" │ ", Role::FgFaint);
    let mut title = vec![
        Seg::new("  ", Role::Fg), // the caret gutter
        Seg::new(format!("{} ", pad_right("SYMBOL", label_w)), Role::FgFaint),
        Seg::new(format!("{} ", pad_right("NAME", name_w)), Role::FgFaint),
        Seg::new(pad_left("SHARES", shares_w), Role::FgFaint),
        title_sep.clone(),
        Seg::new(pad_left("PRICE", price_w), Role::FgFaint),
        title_sep.clone(),
        Seg::new(pad_left("VALUE", mv_w), Role::FgFaint),
        title_sep.clone(),
        Seg::new(pad_left("BASIS", basis_w), Role::FgFaint),
        title_sep.clone(),
        Seg::new(pad_left("UNREAL", unreal_w), Role::FgFaint),
        title_sep.clone(),
        Seg::new(pad_left("GAIN%", gainpct_w), Role::FgFaint),
        title_sep.clone(),
        // "NET" spans the row's `net ` prefix + figure cell.
        Seg::new(pad_left("NET", 4 + net_w), Role::FgFaint),
    ];
    if est_w > 0 {
        title.push(Seg::new(" ".repeat(1 + est_w), Role::FgFaint));
    }
    title.push(title_sep.clone());
    title.push(Seg::new(pad_left("DAY Δ", delta_w), Role::FgFaint));
    title.push(title_sep);
    title.push(Seg::new(pad_left("SHARE", share_w), Role::FgFaint));
    // The trend strip is fixed-width and titled like every other column — on the
    // symbol pivot only (a platform group has no single per-symbol series).
    // (TUI-VIEW-POS-007/013)
    if matches!(nav.grouping, views::Grouping::BySymbol) {
        title.push(Seg::new(format!(" {}", pad_right("TREND", theme::SPARK_STRIP_W)), Role::FgFaint));
    }

    let mut out = vec![ScreenLine::from_spans(title)];
    out.extend(cells.iter().zip(shares_cells.iter()).map(|(c, shares)| {
        let sep = Seg::new(" │ ", Role::FgFaint);
        let mut spans = vec![
            caret_seg(c.focused),
            Seg::new(
                format!("{} ", pad_right(&ellipsize(&c.label, label_w), label_w)),
                c.base,
            ),
            // The company name, truncated with `…` only when its clamp forces
            // it. (TUI-VIEW-POS-009)
            Seg::new(
                format!("{} ", pad_right(&ellipsize(&c.name, name_w), name_w)),
                c.base,
            ),
            Seg::new(pad_left(shares, shares_w), c.base),
            sep.clone(),
            Seg::new(pad_left(&c.price, price_w), c.base),
            sep.clone(),
            Seg::new(pad_left(&c.mv, mv_w), c.base),
            sep.clone(),
            Seg::new(pad_left(&c.basis, basis_w), c.base),
            sep.clone(),
            Seg::new(pad_left(&c.unreal, unreal_w), c.unreal_role),
            sep.clone(),
            Seg::new(pad_left(&c.gainpct, gainpct_w), c.gainpct_role),
            sep.clone(),
            Seg::new("net ", Role::FgFaint),
            Seg::new(pad_left(&c.net, net_w), c.net_role),
        ];
        if est_w > 0 {
            spans.push(Seg::new(format!(" {}", pad_right(&c.est, est_w)), Role::Estimate));
        }
        spans.push(sep.clone());
        spans.push(Seg::new(pad_left(&c.delta, delta_w), c.delta_role));
        spans.push(sep);
        spans.push(Seg::new(pad_left(&c.share, share_w), c.base));
        // The fixed 12-cell trend strip rides every SYMBOL row — captured ramp,
        // brightened today tick, dim-dot padding for not-yet-captured days. A
        // platform group row renders no strip (no single per-symbol series).
        // (TUI-VIEW-POS-013)
        if matches!(nav.grouping, views::Grouping::BySymbol) {
            spans.push(Seg::new(" ", c.base));
            spans.extend(trend_strip_segs(&c.spark, c.spark_role));
        }
        if !c.marker.is_empty() {
            spans.push(Seg::new(format!("  {}", c.marker), Role::Degraded));
        }
        ScreenLine::from_spans(spans)
    }));
    out
}

/// The rendered lines for the Open Lots rows — id · symbol · acquire date ·
/// source · term · remaining · basis · basis/share · platform · tracking code —
/// in data-driven `│`-ruled columns (max cell per column under clamps,
/// decimal-aligned remaining qty). (TUI-VIEW-LOT-001; views-design.md → "Line
/// Rendering")
// @spec TUI-VIEW-LOT-001, TUI-VIEW-LOT-002, TUI-VIEW-LOT-003, TUI-VIEW-LOT-004
fn lot_lines(rows: &[views::LotRow], nav: &NavState) -> Vec<ScreenLine> {
    let dash = theme::GLYPH_DASH.to_string();
    struct Cells {
        focused: bool,
        id: String,
        sym: String,
        name: String,
        date: String,
        source: String,
        term: &'static str,
        rem: String,
        basis: String,
        bps: String,
        platform: String,
        code: String,
    }
    let cells: Vec<Cells> = rows
        .iter()
        .map(|r| Cells {
            focused: r.identity == nav.focus,
            id: r.lot_id.clone(),
            sym: r.symbol.clone(),
            // The company-name cell (ticker fallback). (TUI-VIEW-LOT-003)
            name: r.name.clone(),
            date: r.acquire_date.0.to_string(),
            source: format!("{:?}", r.source),
            term: term_label(r.term),
            rem: theme::shares_grouped(r.remaining_qty),
            // BASIS compacts (a glance figure); $/SH is per-share money and
            // keeps cents. (TUI-VIEW-LOT-004)
            basis: theme::money_compact(r.basis),
            bps: theme::money(r.basis_per_share),
            platform: r.platform.clone(),
            code: r.tracking_code.clone().unwrap_or_else(|| dash.clone()),
        })
        .collect();

    // Data-driven widths with each column's TITLE participating (the title row
    // rides the same grid). (TUI-VIEW-LOT-002)
    let id_w = column_width_titled(cells.iter().map(|c| c.id.as_str()), "LOT", 2, 14);
    let sym_w = column_width_titled(cells.iter().map(|c| c.sym.as_str()), "SYMBOL", 4, 10);
    let name_w = column_width_titled(cells.iter().map(|c| c.name.as_str()), "NAME", 4, 18);
    let date_w = column_width_titled(cells.iter().map(|c| c.date.as_str()), "DATE", 1, 10);
    let source_w = column_width_titled(cells.iter().map(|c| c.source.as_str()), "SOURCE", 3, 6);
    let term_w = char_w("TERM").max(2);
    let rem_cells = decimal_align(&cells.iter().map(|c| c.rem.clone()).collect::<Vec<_>>());
    let rem_w = rem_cells.iter().map(|s| char_w(s)).max().unwrap_or(0).max(3);
    let basis_w = column_width(cells.iter().map(|c| c.basis.as_str()), 3, 18);
    let bps_w = column_width(cells.iter().map(|c| c.bps.as_str()), 3, 14);
    let platform_w =
        column_width_titled(cells.iter().map(|c| c.platform.as_str()), "PLATFORM", 2, 14);

    // The column title row: tracked-caps fg-faint titles on the SAME grid.
    // "REM" / "BASIS" / "$/SH" span their cells' `rem ` / `basis ` / `/sh`
    // prefixes/suffixes. (TUI-VIEW-LOT-002)
    let tsep = Seg::new(" │ ", Role::FgFaint);
    let title = vec![
        Seg::new("  ", Role::Fg), // the caret gutter
        Seg::new(format!("{} ", pad_right("LOT", id_w)), Role::FgFaint),
        Seg::new(format!("{} ", pad_right("SYMBOL", sym_w)), Role::FgFaint),
        Seg::new(format!("{} ", pad_right("NAME", name_w)), Role::FgFaint),
        Seg::new(format!("{} ", pad_left("DATE", date_w)), Role::FgFaint),
        Seg::new(format!("{} ", pad_right("SOURCE", source_w)), Role::FgFaint),
        Seg::new(format!("{} ", pad_right("TERM", term_w)), Role::FgFaint),
        Seg::new(pad_left("REM", 4 + rem_w), Role::FgFaint),
        tsep.clone(),
        Seg::new(pad_left("BASIS", 6 + basis_w), Role::FgFaint),
        tsep.clone(),
        Seg::new(pad_left("$/SH", bps_w + 3), Role::FgFaint),
        tsep.clone(),
        Seg::new(pad_right("PLATFORM", platform_w), Role::FgFaint),
        tsep,
        Seg::new("CODE", Role::FgFaint),
    ];

    let mut out = vec![ScreenLine::from_spans(title)];
    out.extend(cells.iter().zip(rem_cells.iter()).map(|(c, rem)| {
        let sep = Seg::new(" │ ", Role::FgFaint);
        ScreenLine::from_spans(vec![
            caret_seg(c.focused),
            Seg::new(format!("{} ", pad_right(&ellipsize(&c.id, id_w), id_w)), Role::Fg),
            Seg::new(
                format!("{} ", pad_right(&ellipsize(&c.sym, sym_w), sym_w)),
                Role::Fg,
            ),
            // The company name, truncated with `…` only when its clamp forces
            // it. (TUI-VIEW-LOT-003)
            Seg::new(
                format!("{} ", pad_right(&ellipsize(&c.name, name_w), name_w)),
                Role::Fg,
            ),
            Seg::new(format!("{} ", pad_left(&c.date, date_w)), Role::Fg),
            Seg::new(format!("{} ", pad_right(&c.source, source_w)), Role::Fg),
            Seg::new(format!("{} ", pad_right(c.term, term_w)), Role::Fg),
            Seg::new("rem ", Role::FgFaint),
            Seg::new(pad_left(rem, rem_w), Role::Fg),
            sep.clone(),
            Seg::new("basis ", Role::FgFaint),
            Seg::new(pad_left(&c.basis, basis_w), Role::Fg),
            sep.clone(),
            Seg::new(pad_left(&c.bps, bps_w), Role::Fg),
            Seg::new("/sh", Role::FgFaint),
            sep.clone(),
            Seg::new(pad_right(&c.platform, platform_w), Role::Fg),
            sep,
            Seg::new(c.code.clone(), Role::Fg),
        ])
    }));
    out
}

/// The History chart's fixed height in rows. Each column resolves to
/// `rows × 8` eighth-block rungs, so even the fixed height reads a fine-grained
/// scale. (views-design.md → "History"; TUI-VIEW-HIST-003)
const HISTORY_CHART_ROWS: usize = 6;

/// The History screen's **multi-row block chart** over the trading-day calendar:
/// one column per calendar day, scaled between the series **min and max**, the
/// min/max rendered as right-aligned axis labels on the bottom/top rows behind a
/// `┤` gutter, and a date-span line beneath. The chart is tinted by net
/// direction (over the complete points) with the **latest captured column
/// rendered bold** — the today tick; a gap day is a **blank column** (never
/// interpolated) and an incomplete day's column carries the **degraded** role
/// (the `‡` convention). Suppressed entirely for the no-history and
/// all-incomplete degenerate states (TUI-VIEW-HIST-002); a single point renders
/// a single tick. (TUI-VIEW-HIST-003)
// @spec TUI-VIEW-HIST-003, TUI-VIEW-HIST-004
fn history_chart_lines(hv: &views::HistoryView) -> Vec<ScreenLine> {
    if matches!(
        hv.state,
        views::HistoryState::NoHistory | views::HistoryState::AllIncomplete
    ) {
        return Vec::new();
    }
    // One column per calendar day: a captured (value, incomplete) or a gap.
    let cols: Vec<Option<(i64, bool)>> = hv
        .cells
        .iter()
        .map(|c| match c {
            views::HistoryCell::Value { value, .. } => Some((value.0, false)),
            views::HistoryCell::Incomplete { value, .. } => Some((value.0, true)),
            views::HistoryCell::Gap { .. } => None,
        })
        .collect();
    let captured: Vec<i64> = cols.iter().flatten().map(|(v, _)| *v).collect();
    let (Some(&min), Some(&max)) = (captured.iter().min(), captured.iter().max()) else {
        return Vec::new();
    };
    let span = (max - min).max(1);
    // Direction tint over the COMPLETE points only (an incomplete endpoint must
    // not steer the tint any more than it may steer a delta).
    let complete: Vec<i64> = cols
        .iter()
        .flatten()
        .filter(|(_, inc)| !inc)
        .map(|(v, _)| *v)
        .collect();
    let tint = theme::series_role(&complete);
    // The today tick: the latest captured column, rendered bold.
    let today_col = cols.iter().rposition(|c| c.is_some());

    // Axis labels are glance money: the compact whole-dollar form.
    // (TUI-VIEW-HIST-004)
    let min_label = theme::money_compact(pt_core::Cents(min));
    let max_label = theme::money_compact(pt_core::Cents(max));
    let label_w = char_w(&min_label).max(char_w(&max_label));
    let rows = HISTORY_CHART_ROWS;

    let mut out: Vec<ScreenLine> = Vec::new();
    for row in 0..rows {
        // Axis labels ride the edge rows: max on top, min on the bottom.
        let label = if row == 0 {
            max_label.as_str()
        } else if row == rows - 1 {
            min_label.as_str()
        } else {
            ""
        };
        let mut spans = vec![Seg::new(
            format!("{} {} ", pad_left(label, label_w), '\u{2524}'),
            Role::FgFaint,
        )];
        let row_from_bottom = rows - 1 - row;
        for (i, col) in cols.iter().enumerate() {
            match col {
                // A gap day is a blank column — never interpolated.
                // (TUI-VIEW-HIST-001/003)
                None => spans.push(Seg::new(" ", Role::Fg)),
                Some((v, incomplete)) => {
                    // The column's total height in eighth-blocks (≥ 1 so the min
                    // still shows a tick), sliced into this row's cell.
                    let eighths = ((((v - min) as i128) * ((rows * 8 - 1) as i128)
                        / (span as i128)) as i64)
                        + 1;
                    let cell = (eighths - (row_from_bottom as i64) * 8).clamp(0, 8);
                    if cell == 0 {
                        spans.push(Seg::new(" ", Role::Fg));
                    } else {
                        let ch = theme::SPARK_RAMP[(cell - 1) as usize];
                        let role = if *incomplete { Role::Degraded } else { tint };
                        let seg = if today_col == Some(i) {
                            Seg::bold(ch.to_string(), role)
                        } else {
                            Seg::new(ch.to_string(), role)
                        };
                        spans.push(seg);
                    }
                }
            }
        }
        out.push(ScreenLine::from_spans(spans));
    }
    // The date-span line beneath the chart: the calendar's first → last day.
    let keys: Vec<i32> = hv
        .cells
        .iter()
        .map(|c| match c {
            views::HistoryCell::Value { key, .. }
            | views::HistoryCell::Gap { key }
            | views::HistoryCell::Incomplete { key, .. } => key.0,
        })
        .collect();
    if let (Some(first), Some(last)) = (keys.first(), keys.last()) {
        out.push(ScreenLine::plain(
            format!(
                "{} {} day {first} {} day {last}",
                pad_left("", label_w),
                '\u{2514}',
                theme::GLYPH_FLOW
            ),
            Role::FgFaint,
        ));
    }
    out
}

/// The rendered line for a History cell: a value (glance money — the compact
/// whole-dollar form), an in-band gap (blank), or a `‡`-tinted incomplete day.
/// (TUI-VIEW-HIST-001, TUI-VIEW-HIST-004)
// @spec TUI-VIEW-HIST-004
fn history_cell_line(cell: &views::HistoryCell) -> ScreenLine {
    match cell {
        views::HistoryCell::Value { key, value } => {
            ScreenLine::plain(format!("day {} {}", key.0, theme::money_compact(*value)), Role::Fg)
        }
        // A gap is a blank cell — never interpolated. (TUI-VIEW-HIST-001)
        views::HistoryCell::Gap { key } => {
            ScreenLine::plain(format!("day {} · gap ·", key.0), Role::FgFaint)
        }
        views::HistoryCell::Incomplete { key, value } => ScreenLine::plain(
            format!("day {} {} {}", key.0, theme::money_compact(*value), theme::GLYPH_DEGRADED),
            Role::Degraded,
        ),
    }
}

fn term_label(t: tax::Term) -> &'static str {
    match t {
        tax::Term::LongTerm => "LT",
        tax::Term::ShortTerm => "ST",
    }
}

/// Render the Entry shell — the active composer form as a rounded titled panel (the
/// same gilt panel `views` uses), or a calm idle hint when no flow is open. The
/// active form (top of the entry stack) renders its fields with the gilt focus
/// caret on the focused field, tracked-caps labels, the single inline error/advisory
/// slot directly beneath the offending field, and a footer hint line of the active
/// bracketed accelerators. A Sell's lot picker renders as a Table with a footer
/// band. Every FlowKind renders through this one path. (TUI-ENTRY-FLOW-009;
/// TUI-ENTRY-LOT-006)
// @spec TUI-ENTRY-FLOW-009, TUI-ENTRY-LOT-006
fn render_entry_shell(model: &Model, pal: &Palette, area: Rect, buf: &mut Buffer) {
    let Some(ctx) = model.entry_top() else {
        // No flow open: a calm idle panel naming the mode (never a fabricated form).
        let block = entry_panel(pal, "Entry");
        let inner = block.inner(area);
        block.render(area, buf);
        Paragraph::new("compose → validate → submit → confirmed")
            .style(pal.style(Role::FgFaint))
            .render(inner, buf);
        return;
    };

    let block = entry_panel(pal, &ctx.form.title());
    let inner = block.inner(area);
    block.render(area, buf);

    if ctx.picker_ref().is_some() {
        render_lot_picker(ctx, pal, inner, buf);
    } else {
        render_entry_form(&ctx.form, pal, inner, buf);
    }
}

/// The shared rounded gilt Entry panel (the focused panel's border turns gilt —
/// the same motif as `views`). (TUI-ENTRY-FLOW-009; tui-design.md → "Panels & rules")
fn entry_panel(pal: &Palette, title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(pal.style(Role::Accent))
        .title(title.to_string())
}

/// Render a composer form's fields into the panel inner area: each field on its own
/// line as `‹caret› LABEL  value`, the focused field carrying the gilt focus caret
/// (`▎`) in the `accent` role, tracked-caps labels in `fg-faint`, the single inline
/// error/advisory slot directly beneath the offending field, and a footer hint line
/// of the active accelerators at the bottom. (TUI-ENTRY-FLOW-009)
// @spec TUI-ENTRY-FLOW-009
fn render_entry_form(form: &FormModel, pal: &Palette, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    // The footer hint line pins to the bottom row of the inner area.
    let footer_y = area.bottom().saturating_sub(1);
    let body_height = area.height.saturating_sub(1);

    let mut y = area.y;
    for (i, field) in form.fields.iter().enumerate() {
        if y >= area.y + body_height {
            break;
        }
        let focused = i == form.focus;
        // The gilt focus caret on the focused field; a blank gutter otherwise so the
        // columns align. (TUI-ENTRY-FLOW-009)
        let caret = if focused { theme::GLYPH_FOCUS } else { ' ' };
        // Tracked-caps label (the monospace type-hierarchy substitute).
        let label = field.label.to_uppercase();
        let line = format!("{caret} {label}  {}", field.value);
        let role = if focused { Role::Accent } else { Role::Fg };
        let row = Rect::new(area.x, y, area.width, 1);
        Paragraph::new(line).style(pal.style(role)).render(row, buf);
        y += 1;

        // The single inline error/advisory slot, directly BENEATH the offending
        // field — shared by inline validation + a submit-time disagreement. A
        // `warn`-role advisory vs an `error`-role rejection. (TUI-ENTRY-FLOW-009)
        if let Some(err) = &form.error {
            if err.field == i && y < area.y + body_height {
                let role = if err.advisory { Role::Warn } else { Role::Error };
                let glyph = if err.advisory { theme::GLYPH_WARN } else { theme::GLYPH_ERROR };
                let row = Rect::new(area.x, y, area.width, 1);
                Paragraph::new(format!("    {glyph} {}", err.message))
                    .style(pal.style(role))
                    .render(row, buf);
                y += 1;
            }
        }
    }

    // The footer hint line of the active bracketed accelerators, in chrome
    // `fg-faint`. (TUI-ENTRY-FLOW-009)
    let footer = Rect::new(area.x, footer_y, area.width, 1);
    Paragraph::new(form.footer_hint())
        .style(pal.style(Role::FgFaint))
        .render(footer, buf);
}

/// Render the lot picker as a `Table` — one row per open lot (id · source+date ·
/// term · remaining · basis/share · a `take [N]` input cell) with the focus caret
/// on the active row — and the running total / `[est]` preview / empty-state /
/// inline error in a **footer band beneath the table body, never over a data row**.
/// (TUI-ENTRY-LOT-006)
// @spec TUI-ENTRY-LOT-006
fn render_lot_picker(ctx: &EntryContext, pal: &Palette, area: Rect, buf: &mut Buffer) {
    let Some(picker) = ctx.picker_ref() else { return };
    if area.height == 0 {
        return;
    }

    // The footer band: the running total / preview / empty-state / inline error,
    // computed first so we can reserve its rows beneath the table. (TUI-ENTRY-LOT-006)
    let error = match &ctx.form.error {
        Some(e) => Some(entry::InlineError::Field(e.message.clone())),
        None => None,
    };
    let footer = form::picker_footer(picker, ctx.preview.as_ref(), error.as_ref());
    let footer_rows = footer.lines.len() as u16;
    // The footer band + the footer hint line sit at the bottom; the table body gets
    // the rest. A one-row hairline separates them visually.
    let hint_y = area.bottom().saturating_sub(1);
    let band_top = hint_y.saturating_sub(footer_rows);
    let table_height = band_top.saturating_sub(area.y);

    // The table of open lots. The focus caret marks the active row. (TUI-ENTRY-LOT-006)
    let header = Row::new(vec![
        Cell::from(""),
        Cell::from("LOT"),
        Cell::from("SOURCE"),
        Cell::from("TERM"),
        Cell::from("REMAINING"),
        Cell::from("BASIS/SH"),
        Cell::from("TAKE"),
    ])
    .style(pal.style(Role::FgFaint));

    let rows: Vec<Row> = picker
        .lots
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let focused = i == ctx.picker_focus;
            let caret = if focused { theme::GLYPH_FOCUS.to_string() } else { String::new() };
            // A `rem 0` lot is greyed (listed but unallocatable). (TUI-ENTRY-LOT-004)
            let role = if l.greyed() {
                Role::FgFaint
            } else if focused {
                Role::Accent
            } else {
                Role::Fg
            };
            let take = if l.greyed() {
                String::new()
            } else {
                format!("take [{}]", theme::shares(l.take))
            };
            Row::new(vec![
                Cell::from(caret),
                Cell::from(l.lot_id.clone()),
                Cell::from(format!("{:?} {}", l.source, l.acquire_date.0)),
                Cell::from(term_label(l.term)),
                Cell::from(format!("rem {}", theme::shares(l.remaining_qty))),
                Cell::from(theme::money(l.basis_per_share)),
                Cell::from(take),
            ])
            .style(pal.style(role))
        })
        .collect();

    use ratatui::layout::Constraint;
    let widths = [
        Constraint::Length(1),
        Constraint::Min(8),
        Constraint::Min(12),
        Constraint::Length(4),
        Constraint::Min(10),
        Constraint::Min(9),
        Constraint::Min(10),
    ];
    let table_area = Rect::new(area.x, area.y, area.width, table_height.max(1));
    Table::new(rows, widths)
        .header(header)
        .render(table_area, buf);

    // The footer band — beneath the table body, each line in its role. The empty/
    // insufficient state, the running total, the [est] preview, and the inline
    // error all live here, never over a data row. (TUI-ENTRY-LOT-006)
    for (i, (text, role)) in footer.lines.iter().enumerate() {
        let y = band_top + i as u16;
        if y >= hint_y {
            break;
        }
        let row = Rect::new(area.x, y, area.width, 1);
        Paragraph::new(text.clone()).style(pal.style(*role)).render(row, buf);
    }

    // The footer hint line of the active accelerators (`[F] FIFO`, `[enter] submit`,
    // `[esc] cancel`). (TUI-ENTRY-FLOW-009)
    let hint = Rect::new(area.x, hint_y, area.width, 1);
    Paragraph::new(ctx.form.footer_hint())
        .style(pal.style(Role::FgFaint))
        .render(hint, buf);
}

// ===========================================================================
// The lot-picker gain/tax preview (TUI-ENTRY-LOT-005). The kernel's computation on
// the proposed allocation, stacked at the sale's sale_date YTD position. `[est]`-
// flagged, degraded if the mark is unavailable; never blocks the sell.
// ===========================================================================

/// Compute the live estimated gain/tax preview for a lot-picker allocation. The
/// preview **computes nothing itself**: it delegates the tax figure to the verified
/// `tax` kernel (`tax::unrealized_estimate`), which stacks the proposed gain at the
/// sale's `sale_date` position in the year's realized YTD, routes long-term lots
/// through the preferential set, and adds NIIT + the resolved state — so the preview
/// matches the accrual the Sell will record. Degraded (`‡`, never a zero) when the
/// symbol's mark is unavailable; the tax is `n/a` under cold-start brackets. The
/// estimate is `[est]`-flagged and **never blocks the sell**. (TUI-ENTRY-LOT-005)
///
/// `mark_available` is whether the symbol has a current mark (a degraded mark makes
/// the whole preview degraded). The proposed allocation is modelled as synthetic
/// `OpenLot`s — one per taken lot, carrying that lot's `acquire_date` (so LT/ST
/// classifies correctly as of `sale_date`) and its share of the net proceeds less
/// basis as the per-lot unrealized — which the kernel stacks exactly as it would the
/// realized accrual. Per-lot figures carry the kernel's ±½¢ allocation rounding;
/// only the totals are exact.
pub fn gain_tax_preview(
    picker: &entry::LotPicker,
    unit_price: pt_core::Cents,
    fees: pt_core::Cents,
    snapshot: &ledger_core::Snapshot,
    ctx: &tax::TaxContext,
    as_of: pt_core::Date,
    mark_available: bool,
) -> entry::GainTaxPreview {
    if !mark_available {
        return entry::GainTaxPreview { est_gain: None, est_tax: None, degraded: true };
    }
    // Estimated gain = net proceeds − Σ consumed basis over the proposed allocation.
    // proceeds = scale(allocated_qty × unit_price) − fees (the kernel's net). The
    // fees are spread across the taken lots in proportion to qty so each synthetic
    // lot's unrealized nets its share of the fee.
    let allocated = picker.allocated();
    let gross = pt_core::scale((allocated.0 as i128) * (unit_price.0 as i128)) as i64;
    let net_proceeds = gross - fees.0;

    // Build a synthetic OpenLot per taken lot: its proceeds (gross at the unit
    // price) less its basis is its unrealized, which the kernel splits LT/ST by the
    // lot's acquire_date as of `as_of` (the sale_date). The fee is allocated last to
    // a single lot's unrealized so the synthetic Σ unrealized equals the net gain
    // exactly (only the total is exact; per-lot figures carry the ±½¢ rounding).
    let mut consumed_basis: i64 = 0;
    let mut synthetic: Vec<ledger_core::OpenLot> = Vec::new();
    for l in &picker.lots {
        if l.take.0 == 0 {
            continue;
        }
        let lot_basis = pt_core::scale((l.basis_per_share.0 as i128) * (l.take.0 as i128)) as i64;
        consumed_basis += lot_basis;
        let lot_gross = pt_core::scale((l.take.0 as i128) * (unit_price.0 as i128)) as i64;
        synthetic.push(ledger_core::OpenLot {
            lot: ledger_core::Lot {
                id: l.lot_id.clone(),
                symbol: picker.symbol.clone(),
                acquire_date: l.acquire_date,
                open_seq: pt_core::Seq(0),
                source: l.source,
                remaining_qty: l.take,
                remaining_basis_cents: pt_core::Cents(lot_basis),
                platform: picker.platform.clone(),
                tracking_code: None,
            },
            unrealized_cents: Some(pt_core::Cents(lot_gross - lot_basis)),
        });
    }
    let est_gain = net_proceeds - consumed_basis;
    // Fold the (signed) fee into the last synthetic lot's unrealized so the kernel
    // sees Σ unrealized == est_gain exactly.
    if let Some(last) = synthetic.last_mut() {
        if let Some(u) = last.unrealized_cents {
            last.unrealized_cents = Some(pt_core::Cents(u.0 - fees.0));
        }
    }

    // The realized YTD the proposed gain stacks on — the gains realized in the
    // SALE's tax year (the sale_date's calendar year), split LT/ST. This is the
    // "stacked at the sale's sale_date position in the year's YTD" the spec calls
    // for, so the preview matches the accrual the Sell records. (TUI-ENTRY-LOT-005)
    let sale_year = tax::tax_year_of(as_of);
    let mut ytd_st: i64 = 0;
    let mut ytd_lt: i64 = 0;
    for g in &snapshot.realized_gains {
        if tax::tax_year_of(g.sale_date) != sale_year {
            continue;
        }
        match tax::classify_term(g.acquire_date, g.sale_date) {
            tax::Term::LongTerm => ytd_lt += g.gain_cents.0,
            tax::Term::ShortTerm => ytd_st += g.gain_cents.0,
        }
    }

    // Delegate the tax computation to the verified kernel: it stacks the proposed
    // gain at the YTD position, distinguishes LT vs ST, and adds NIIT + the resolved
    // state. `estimated_tax_cents` is `None` under a degraded mark (n/a here, since
    // we only reach this with a mark) or `NoBracketsAvailable` (cold-start). The
    // accrues_to_state is the residency default the Sell will stamp. (TUI-ENTRY-LOT-005)
    let accrues_to_state = ctx.residency_default.clone();
    let estimate = tax::unrealized_estimate(
        &picker.symbol,
        &synthetic,
        as_of,
        &accrues_to_state,
        pt_core::Cents(ytd_st),
        pt_core::Cents(ytd_lt),
        ctx,
    );

    entry::GainTaxPreview {
        est_gain: Some(pt_core::Cents(est_gain)),
        est_tax: estimate.estimated_tax_cents,
        degraded: false,
    }
}
