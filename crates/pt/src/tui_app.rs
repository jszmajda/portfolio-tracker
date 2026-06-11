//! The interactive TUI's terminal event loop and the headless-summary driver — the
//! live I/O that runs THROUGH `runtime`. Kept out of the `pt` library because it
//! owns the real `crossterm` terminal and the live `GoogleSheetsApi`; both are
//! confirmed by the manual `pt` run and the env-gated e2e, never by `cargo test`.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Stdout};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::{execute, ExecutableCommand};
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;

use ledger_core::LedgerEvent;
use pt_core::Date;
use runtime::{per_symbol_freshness, Boot, CycleOutcome, StoreLockAdapter};
use summary::{dispatch, OutputMode, WriteLockProbe};
use tax::TaxContext;
use tui::form::FormAction;
use tui::port::{Connection, IntegrityError, RuntimePort, SubmitOutcome, ViewState};
use tui::Model;

use pt::shell::{self, KeyRoute, LaunchFlow, ShellKey};
use pt::wiring::{
    context_for_year, history_bundle, run_live_cycle,
    submit_ledger_through_store as run_submit_ledger, submit_tax_through_store as run_submit_tax,
    summary_inputs_from_cycle, view_state_from_cycle,
};

/// Epoch seconds now (the wall clock; the live paths only).
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The reporting-TZ "today" as a days-since-epoch `Date` (a calendar-day metadata
/// value, never the trading-day KEY). A simple UTC-day derivation; the exact
/// reporting-TZ offset is a `config` concern wired later.
fn today_date() -> Date {
    Date((now_secs() / 86_400) as i32)
}

// ===========================================================================
// The headless `summary` driver (the `pt summary` body). Connect, run ONE cycle
// read-only, project the inputs, run `summary::run_summary`, dispatch the output +
// exit code. Read-only: it does not hold the advisory write-lock and never writes
// the event log; the capture append is the only write and is non-fatal on failure.
// ===========================================================================

/// Run the headless summary against the live workbook, returning the rendered
/// output and the process exit code. Read-only cycle: load -> replay -> read marks
/// back -> project -> capture (last-wins) -> delta -> report.
pub fn run_headless_summary(
    settings: config::Settings,
    mode: OutputMode,
) -> Result<(String, i32), Box<dyn Error>> {
    let tax_year = calendar_year(today_date());

    // The composition root (RUNTIME-BOOT-001/003): the single place that captures the
    // creds (for the ONE Sheets client every workbook touch rides) and constructs the
    // runtime-owned advisory lock. The summary runs THROUGH the collaborators this
    // root builds rather than re-wiring its own.
    let boot = Boot::from_settings(&settings, "summary");

    // Load the workbook domain config and resolve the year's tax context from it
    // (brackets, NIIT, income, de-minimis, residency default). An unreadable
    // config degrades to the regime-agnostic context — never a fabricated tax.
    let cfg = load_workbook_config(&boot, &settings);
    let ctx = match &cfg {
        Some(data) => pt::wiring::context_from_config(data, tax_year, today_date()),
        None => context_for_year(tax_year),
    };
    let bracket_state = cfg
        .as_ref()
        .map(|d| pt::wiring::bracket_state_for(d, tax_year))
        .unwrap_or(config::BracketState::NoBracketsAvailable);
    let aliases =
        cfg.as_ref().map(|d| d.aliases.clone()).unwrap_or_default();

    let outcome = run_live_cycle(&boot, &ctx, today_date(), &aliases)?;

    let inputs = summary_inputs_from_cycle(
        &outcome,
        bracket_state,
        tax_year,
        Vec::new(), // the trading-day calendar (History axis) — empty until History is wired
        today_date(),
        now_secs(),
    );

    // The History client built by the composition root over the SAME ONE Sheets
    // primitive (no segment opens its own client). The capture append is the only
    // write; a failure is non-fatal (the uncaptured-delta path, exit 0).
    let mut history = boot.history_client(sheets_view::HISTORY_TAB)?;

    // The capture append acquires the runtime-owned advisory write-lock INSIDE its
    // write primitive (RUNTIME-LOCK-002): the SAME machine-local lock the root owns,
    // adapted via `StoreLockAdapter`, so single-writer protection is independent of
    // the caller. The held probe stays never-held: a `summary` run already degrades to
    // read-only safely whether or not an interactive TUI holds the lock (the only
    // write, the capture, is itself lock-acquired). (RUNTIME-LOCK-002, SUMMARY-CAP-002)
    let probe = NeverHeldProbe;
    let lock = StoreLockAdapter::new(boot.lock());

    let run = summary::run_summary(
        &mut history,
        &lock,
        &probe,
        summary::TrustState::Ok,
        &inputs,
    );
    let (out, exit) = dispatch(&run, mode);
    Ok((out, exit.code()))
}

/// The current calendar year for `date` (days since epoch). A thin civil-date
/// derivation for the tax-year default.
fn calendar_year(date: Date) -> i32 {
    reports::calendar_year_of(date)
}

/// Load the workbook's domain config through the composition root (the `Tax
/// Rules` config tab; the adapter persists the whole domain config there). A
/// connection/read failure returns `None` — callers degrade to the bracket-less
/// context rather than fabricating one. (CONFIG-SETTINGS-003/004)
fn load_workbook_config(boot: &Boot, settings: &config::Settings) -> Option<config::ConfigData> {
    use config::ConfigStore as _;
    let client = boot
        .config_client(sheets_view::TAX_RULES_TAB, Some(settings.clone()))
        .ok()?;
    client.load().ok()
}

/// A `WriteLockProbe` that always reports the lock free (the binary's read-only
/// summary degrades safely whether or not the lock is held; the real cross-process
/// probe is `runtime::AdvisoryLock`).
struct NeverHeldProbe;
impl WriteLockProbe for NeverHeldProbe {
    fn is_held(&self) -> bool {
        false
    }
}

// ===========================================================================
// The live replay cycle runs THROUGH the `runtime` composition root: the store and
// the view client are built by `Boot` (see `pt::wiring::run_live_cycle`), so the
// binary re-wires no collaborators of its own. (RUNTIME-BOOT-001/003)
// ===========================================================================

// ===========================================================================
// The interactive TUI. Build a live read-only `RuntimePort`, then run the
// hand-rolled Model-Update-View loop over `crossterm`. A non-live config (or a
// connection failure) renders the loud integrity block rather than crashing.
// ===========================================================================

/// Launch the interactive TUI against the live workbook. Sets up the alternate
/// screen + raw mode, runs the event loop, and always restores the terminal on the
/// way out (even on a panic-free error).
pub fn run_tui(settings: config::Settings) -> Result<(), Box<dyn Error>> {
    let port = LivePort::connect(settings);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, port);

    // Restore the terminal regardless of how the loop ended.
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// How long the loop waits for an input event before waking to check the ambient
/// auto-refresh clock — the "tick" the shell's clock rule rides (tui-design.md →
/// "The shell owns every clock"). Short enough that a due refresh (or the deferred
/// one after a composer closes) lands promptly; the hourly threshold itself is
/// [`shell::AUTOREFRESH_INTERVAL`].
const POLL_TICK: Duration = Duration::from_secs(1);

/// The hand-rolled Model-Update-View loop: draw the model, poll for a key (a short
/// tick, so the loop wakes without input), dispatch to the active context's keymap,
/// check the ambient auto-refresh clock, repeat until quit. (tui-design.md → the
/// Model-Update-View loop; TUI-ENTRY-FLOW-008) The **active context owns the keymap**:
/// an open Entry composer takes the key (field nav / text input); a key unbound there,
/// and every key in Views, falls through to the shell's global bindings (`q` quit,
/// `tab` toggle mode, `r` refresh, `esc` ascend).
///
/// **Ambient auto-refresh** (TUI-VIEW-NAV-017/018/019): once an hour has elapsed
/// since the last successful refresh — the connect cycle seeds the clock; manual
/// `[r]`, automatic, and post-submit refreshes all advance it via [`run_refresh`] —
/// the loop triggers the same refresh as `[r]`, unless an entry context is open
/// (the deferred refresh runs on the first tick after the composer closes). The
/// decision itself is the pure [`shell::should_autorefresh`], unit-tested in
/// `shell_autorefresh.rs`; the loop only measures the elapsed time and executes.
// @spec TUI-VIEW-NAV-017, TUI-VIEW-NAV-018
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mut port: LivePort,
) -> Result<(), Box<dyn Error>> {
    let mut model = Model::new();
    // The ambient auto-refresh clock: the instant of the last successful refresh.
    // The connect cycle that produced the opening view seeds it (TUI-VIEW-NAV-017).
    let mut last_refresh = Instant::now();
    loop {
        terminal.draw(|f| {
            let area = f.area();
            let buf = tui::render_to_buffer(&model, &port, area.width, area.height);
            // Blit the pre-rendered buffer into the frame's buffer.
            let target = f.buffer_mut();
            for y in 0..area.height.min(buf.area.height) {
                for x in 0..area.width.min(buf.area.width) {
                    if let (Some(src), Some(dst)) =
                        (buf.cell((x, y)), target.cell_mut((x, y)))
                    {
                        *dst = src.clone();
                    }
                }
            }
        })?;

        if event::poll(POLL_TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if dispatch_key(&mut model, &mut port, key, &mut last_refresh)
                    == KeyResult::Quit
                {
                    break;
                }
            }
        }

        // The ambient hourly refresh — the same path as `[r]`. The pure decision is
        // the shell's; only an open entry context suppresses it (the help overlay is
        // chrome and does not). A failed attempt leaves `last_refresh` unadvanced
        // inside `run_refresh`, so the next tick retries. (TUI-VIEW-NAV-017/018/019)
        if shell::should_autorefresh(last_refresh.elapsed(), model.entry_top().is_some()) {
            run_refresh(&mut model, &mut port, &mut last_refresh);
        }
    }
    Ok(())
}

/// Run the shared refresh path — `Model::refresh` (re-read marks via the port's cycle
/// → re-resolve stacked anchors → re-anchor focus) — and advance the auto-refresh
/// clock **only when the refreshed view is live**: a failed refresh has already
/// degraded calmly inside the port (offline-stale marking, the last successful run's
/// `updated HH:MM` retained), and leaving the clock unadvanced is what makes the next
/// tick retry. Every refresh — manual `[r]`, post-submit, automatic — rides this one
/// helper so each kind advances the same clock. (TUI-VIEW-NAV-017, TUI-VIEW-NAV-019)
// @spec TUI-VIEW-NAV-019
fn run_refresh(model: &mut Model, port: &mut LivePort, last_refresh: &mut Instant) {
    let _notices = model.refresh(port);
    if port.view().connection == Connection::Live {
        *last_refresh = Instant::now();
    }
}

/// Whether a dispatched key asked the loop to quit.
#[derive(PartialEq, Eq)]
enum KeyResult {
    /// Keep looping (re-render from the resulting Model).
    Continue,
    /// Quit the app (the global quit binding fired at the landing frame).
    Quit,
}

/// Translate a `crossterm` [`KeyEvent`] into the abstract [`ShellKey`] the pure router
/// routes — `shift-tab` arrives as `BackTab`. (TUI-ENTRY-FLOW-008)
fn shell_key(key: KeyEvent) -> ShellKey {
    match key.code {
        KeyCode::Tab => ShellKey::Tab,
        KeyCode::BackTab => ShellKey::BackTab,
        KeyCode::Up => ShellKey::Up,
        KeyCode::Down => ShellKey::Down,
        KeyCode::Enter => ShellKey::Enter,
        KeyCode::Esc => ShellKey::Esc,
        KeyCode::Backspace => ShellKey::Backspace,
        KeyCode::Char(c) => ShellKey::Char(c),
        _ => ShellKey::Other,
    }
}

/// Fold one key event into the Model along the active context's keymap, executing the
/// route the pure [`pt::shell::route`] returns (TUI-ENTRY-FLOW-008). The routing
/// decision (which key reaches the active context, the text-input suppression of the
/// global accelerators, the fall-through to the shell's global bindings) is unit-tested
/// in `pt::shell`; this drives the Model update methods + the port from that decision.
// @spec TUI-ENTRY-FLOW-008, TUI-VIEW-NAV-011
fn dispatch_key(
    model: &mut Model,
    port: &mut LivePort,
    key: KeyEvent,
    last_refresh: &mut Instant,
) -> KeyResult {
    match shell::route(model, shell_key(key)) {
        // --- Global shell bindings ----------------------------------------------------
        KeyRoute::Quit => return KeyResult::Quit,
        KeyRoute::Ascend => {
            model.ascend();
        }
        KeyRoute::ToggleMode => model.toggle_mode(),
        // `[r]`: the shared refresh path — a successful manual refresh advances the
        // ambient auto-refresh clock too (TUI-VIEW-NAV-017).
        KeyRoute::Refresh => run_refresh(model, port, last_refresh),
        // `?` toggles the modal help overlay — chrome only, no state beyond the
        // flag. (TUI-VIEW-NAV-011)
        KeyRoute::ToggleHelp => model.toggle_help(),
        // `1`–`5` switch directly between the Views screens (the stack is
        // replaced; each screen's filter/sort/grouping are retained by the
        // Model). (TUI-VIEW-NAV-014)
        KeyRoute::SwitchScreen(screen) => model.switch_screen(screen),

        // --- Flow launch (the idle Entry panel) ---------------------------------------
        KeyRoute::OpenFlow(flow) => open_flow(model, port, flow),

        // --- Entry composer keymap ----------------------------------------------------
        KeyRoute::EntryFocusNext => {
            model.entry_focus_next();
        }
        KeyRoute::EntryFocusPrev => {
            model.entry_focus_prev();
        }
        KeyRoute::EntryStartTyping(c) => {
            // A printable key starts editing the focused field — enter text-input mode
            // (suppressing the global accelerators) and type the character. The edit
            // drops any frozen retry candidate (a new entry, new id). (FLOW-010)
            model.entry_enter_field();
            model.entry_type_char(c);
        }
        KeyRoute::EntryType(c) => {
            model.entry_type_char(c);
        }
        KeyRoute::EntryBackspace => {
            model.entry_backspace();
        }
        KeyRoute::EntryEnter => {
            // `enter` commits on the last field (submit) or advances; the Model decides.
            let action = model.entry_enter();
            run_form_action(model, port, action, last_refresh);
        }
        KeyRoute::EntryEscape => {
            // `esc` leaves text-input (handled inside `entry_escape`), or cancels +
            // pops the flow; a dirty field asks for a confirm-discard the shell honors.
            if model.entry_escape() == FormAction::ConfirmDiscard {
                model.entry_discard();
            }
        }
        KeyRoute::EntryConfirmNewSymbol => {
            // `[c]` on a held new-symbol guard: confirm the new position + re-submit.
            // (TUI-ENTRY-ACT-006)
            let aliases = port.aliases.clone();
            model.entry_confirm_new_symbol(port, &aliases);
        }

        // --- Lot-picker keymap --------------------------------------------------------
        KeyRoute::PickerFocusNext => model.picker_focus_next(),
        KeyRoute::PickerFocusPrev => model.picker_focus_prev(),
        KeyRoute::PickerFillFifo => model.picker_fill_fifo(),
        KeyRoute::PickerStartTake(c) => {
            // A digit starts a `take [N]` edit on the active row: enter text-input on
            // the picker's take buffer and seed the first digit. (TUI-ENTRY-LOT-002)
            // The take edit reads the digit run, then `enter` applies it; here the
            // single-digit fast path sets the take directly so the running total moves
            // immediately (the owner can keep typing digits to grow it).
            if let Some(digit) = c.to_digit(10) {
                model.picker_set_take_on_focus(picker_take_from_digit(model, digit as i64));
            }
        }

        KeyRoute::Noop => {}
    }
    KeyResult::Continue
}

/// Open an entry flow from the idle Entry panel, seeding the composer through the
/// port + the live `config` (TUI-ENTRY-FLOW-007). The typed symbol is empty at
/// launch (the owner types it into the symbol field); the date defaults to today,
/// the platform to the first `config` suggestion, the alias table resolves the
/// symbol. A Sell builds its lot picker live from the snapshot. (TUI-ENTRY-FLOW-008)
// @spec TUI-ENTRY-FLOW-008
fn open_flow(model: &mut Model, port: &LivePort, flow: LaunchFlow) {
    let id = next_local_id(flow);
    match flow {
        LaunchFlow::Buy => model.open_buy(port, id, "", &port.platforms, &port.aliases),
        LaunchFlow::Vest => model.open_vest(port, id, "", &port.platforms, &port.aliases),
        LaunchFlow::Sell => {
            model.open_sell(port, id, "", &port.residency, &port.platforms, &port.aliases)
        }
        LaunchFlow::Split => model.open_split("", port.today()),
    }
}

/// A local candidate id for a launched flow (the store assigns the durable EventId
/// at append; this is only the composer's placeholder lot/sale id). Derived from the
/// wall clock so two launches in one session do not collide.
fn next_local_id(flow: LaunchFlow) -> String {
    let prefix = match flow {
        LaunchFlow::Buy => "L",
        LaunchFlow::Vest => "V",
        LaunchFlow::Sell => "S",
        LaunchFlow::Split => "X",
    };
    format!("{prefix}{}", now_secs())
}

/// The take to set on the focused picker row from a typed digit: append the digit to
/// the row's current whole-share take (so `1` then `0` reads as `10` shares). Reads
/// the active row's current take to grow it digit-by-digit.
fn picker_take_from_digit(model: &Model, digit: i64) -> pt_core::MicroShares {
    let current_whole = model
        .entry_top()
        .and_then(|c| c.picker_ref())
        .and_then(|p| p.lots.get(model.entry_top().map(|c| c.picker_focus).unwrap_or(0)))
        .map(|l| l.take.0 / pt_core::SHARE_SCALE)
        .unwrap_or(0);
    pt_core::MicroShares((current_whole * 10 + digit) * pt_core::SHARE_SCALE)
}

/// Perform the side effect a form transition asked the shell to run (the transition
/// itself stays in the Model — tui-design.md → "Errors and submits ride the Model").
/// A `Submit` runs the composer's submit through the port; a clean `Cancel` was already
/// handled by [`Model::entry_escape`]. (TUI-ENTRY-FLOW-008)
fn run_form_action(
    model: &mut Model,
    port: &mut LivePort,
    action: FormAction,
    last_refresh: &mut Instant,
) {
    match action {
        // `enter` on the last field submits: re-parse the edited fields into the typed
        // composer, compose (or reuse the FROZEN candidate on a retry), submit through
        // the port (gated by the new-symbol/residency guards), and fold the returned
        // Phase onto the form — clearing the context on confirmed durability. The
        // authoritative write path is `LivePort::submit_*` → `submit_ledger_through_store`,
        // unit-tested in `submit_write_path.rs`. (TUI-ENTRY-FLOW-002/003/004/005/010)
        FormAction::Submit => {
            let aliases = port.aliases.clone();
            model.entry_submit(port, &aliases);
            // A confirmed durable append closes the entry context; refresh re-runs
            // the PUBLISHING cycle so the workbook view reflects the append —
            // "republish after each successful event append" — and, succeeding,
            // advances the ambient auto-refresh clock (TUI-VIEW-NAV-017). (SHEET-PUB-001)
            if model.entry_top().is_none() {
                run_refresh(model, port, last_refresh);
            }
        }
        FormAction::Consumed | FormAction::Cancel | FormAction::ConfirmDiscard => {}
    }
}

/// A live, read-only [`RuntimePort`] for the interactive TUI: it holds the
/// projected [`ViewState`] from a cycle and re-runs the cycle on `refresh`, and drives
/// the authoritative write path on `submit_*` — re-validate live → assign EventId →
/// revalidate against the refreshed view → acquire the advisory write-lock → append +
/// read-back-verify → `SubmitOutcome`. (RUNTIME-EVENTID-001, RUNTIME-REVALIDATE-001)
struct LivePort {
    boot: Boot,
    view: ViewState,
    ctx: TaxContext,
    today: Date,
    /// `config` seeds the entry flows default through (TUI-ENTRY-FLOW-007): the
    /// platform suggestion list, the symbol→ticker alias map, and the residency
    /// timeline. Until the config tabs are read into the cycle, these default to the
    /// empty suggestion sets plus a residency timeline derived from the context's
    /// residency default — overridable on every field.
    platforms: config::PlatformList,
    aliases: config::AliasMap,
    residency: config::ResidencyTimeline,
    /// `config`'s symbol→company-display-name map, threaded into the view bundle
    /// for the Positions / Open Lots name columns. (TUI-VIEW-POS-009)
    display_names: config::DisplayNameMap,
    /// The reporting timezone the `updated HH:MM` wall-clock formats in.
    /// (TUI-VIEW-NAV-013)
    reporting_timezone: String,
    /// `config`'s resolved bracket state for the current tax year, threaded into
    /// every projected view so the `[est]` qualifier reflects the brackets the
    /// figures were actually computed under. (TUI-VIEW-POS-011)
    bracket_state: config::BracketState,
}

impl LivePort {
    /// Connect and run the first cycle; on any failure render the loud integrity
    /// block (the TUI never crashes on a bad config / offline workbook).
    ///
    /// Construction goes through the `runtime` composition root (RUNTIME-BOOT-003):
    /// the TUI delegates the wiring of the Sheets client, the store, and the
    /// advisory lock to `Boot` rather than building its own collaborators.
    fn connect(settings: config::Settings) -> Self {
        let today = today_date();
        let tax_year = calendar_year(today);
        let boot = Boot::from_settings(&settings, "tui");

        // The workbook domain config drives the whole port: the year's resolved
        // tax context (brackets/NIIT/income/de-minimis), the entry composers'
        // platform suggestions + symbol aliases, and the residency timeline a
        // Sell's `accrues_to_state` defaults through. An unreadable config
        // degrades to the bracket-less context + empty suggestion sets.
        let cfg = load_workbook_config(&boot, &settings);
        let ctx = match &cfg {
            Some(data) => pt::wiring::context_from_config(data, tax_year, today),
            None => context_for_year(tax_year),
        };
        let (platforms, aliases, residency) = match &cfg {
            Some(data) => (data.platforms.clone(), data.aliases.clone(), data.residency.clone()),
            None => (
                config::PlatformList::default(),
                config::AliasMap::default(),
                // Fall back to the context's residency default (effective at the
                // epoch) so a Sell still defaults rather than blocking.
                match &ctx.residency_default {
                    Some(state) => {
                        config::ResidencyTimeline::from_entries(vec![config::ResidencyEntry {
                            effective_date: Date(0),
                            state_code: state.clone(),
                        }])
                        .unwrap_or_default()
                    }
                    None => config::ResidencyTimeline::new(),
                },
            ),
        };
        let display_names = cfg
            .as_ref()
            .map(|d| d.display_names.clone())
            .unwrap_or_default();
        // The resolved bracket state for the current year rides every projected
        // view (the `[est]` qualifier's source) — never a hardcoded cold-start.
        // (TUI-VIEW-POS-011)
        let bracket_state = pt::wiring::view_bracket_state(cfg.as_ref(), tax_year);
        let reporting_timezone = settings.reporting_timezone.clone();
        let view = match run_live_cycle(&boot, &ctx, today, &aliases) {
            Ok(outcome) => project_view(
                &boot,
                &outcome,
                tax_year,
                bracket_state,
                Connection::Live,
                Some(pt::wiring::hhmm_in_reporting_tz(now_secs(), &reporting_timezone)),
                display_names.clone(),
            ),
            Err(_) => blocked_view(tax_year, IntegrityError::BadCredentials),
        };
        LivePort {
            boot,
            view,
            ctx,
            today,
            platforms,
            aliases,
            residency,
            display_names,
            reporting_timezone,
            bracket_state,
        }
    }
}

impl RuntimePort for LivePort {
    fn view(&self) -> &ViewState {
        &self.view
    }

    fn refresh(&mut self) {
        let tax_year = calendar_year(self.today);
        self.view = match run_live_cycle(&self.boot, &self.ctx, self.today, &self.aliases) {
            Ok(outcome) => project_view(
                &self.boot,
                &outcome,
                tax_year,
                self.bracket_state,
                Connection::Live,
                // The refresh wall-clock the status line's `updated HH:MM`
                // renders. (TUI-VIEW-NAV-013)
                Some(pt::wiring::hhmm_in_reporting_tz(now_secs(), &self.reporting_timezone)),
                self.display_names.clone(),
            ),
            // Offline: keep rendering, marked stale (the prior view survives, but
            // the connection flips so figures are stale-marked; the `updated`
            // stamp stays the LAST successful run's — never fabricated).
            Err(_) => {
                let mut v = self.view.clone();
                v.connection = Connection::Offline;
                v
            }
        };
    }

    fn tax_context(&self) -> &TaxContext {
        &self.ctx
    }

    fn validate_ledger(&self, _candidate: &LedgerEvent) -> Result<(), ledger_core::LedgerError> {
        Ok(())
    }

    fn validate_tax(&self, _candidate: &tax::TaxEvent) -> Result<(), tax::TaxError> {
        Ok(())
    }

    // @spec TUI-ENTRY-FLOW-002, TUI-ENTRY-FLOW-003, TUI-ENTRY-FLOW-004, TUI-ENTRY-FLOW-005, RUNTIME-EVENTID-001, RUNTIME-REVALIDATE-001
    fn submit_ledger(&mut self, candidate: &LedgerEvent) -> SubmitOutcome {
        // The authoritative write path (TUI-ENTRY-FLOW-002/003/004/005): build the
        // `store` over the ONE Sheets client the root captured creds for, threading the
        // runtime-owned advisory lock into its write primitives, then run the submit —
        // re-validate live → assign EventId → revalidate against the refreshed view →
        // acquire the rich advisory lock → append + read-back-verify → map to outcome.
        // A construction failure (offline / bad creds) returns control non-destructively
        // so the entry is preserved and `[r]etry` is offered. (RUNTIME-BOOT-003,
        // RUNTIME-EVENTID-001, RUNTIME-REVALIDATE-001)
        let mut store = match self.boot.store(StoreLockAdapter::new(self.boot.lock())) {
            Ok(s) => s,
            Err(_) => return SubmitOutcome::WriteFailed(tui::port::WriteFailure::Unreachable),
        };
        run_submit_ledger(&mut store, self.boot.lock(), candidate)
    }

    // @spec TUI-ENTRY-FLOW-002, TUI-ENTRY-FLOW-003, TUI-ENTRY-FLOW-004, TUI-ENTRY-FLOW-005, RUNTIME-EVENTID-001, RUNTIME-REVALIDATE-001
    fn submit_tax(&mut self, candidate: &tax::TaxEvent) -> SubmitOutcome {
        let mut store = match self.boot.store(StoreLockAdapter::new(self.boot.lock())) {
            Ok(s) => s,
            Err(_) => return SubmitOutcome::WriteFailed(tui::port::WriteFailure::Unreachable),
        };
        run_submit_tax(&mut store, self.boot.lock(), &self.ctx, candidate)
    }

    fn today(&self) -> Date {
        self.today
    }
}

/// Project a cycle outcome into the TUI's read state (live connection), reading
/// the durable History series for the Positions day-change column and the
/// History chart — the same workbook tab the headless summary reads for its
/// delta, through the same composition-root client. An **unreachable** History
/// degrades calmly to an empty series (the day-change/chart render their
/// no-capture states); a History **integrity** failure (non-monotonic keys, an
/// unparseable row, a checksum mismatch) blocks the screens with the loud `✗`
/// treatment — `reports` flagged a corrupt, non-reconstructable series and the
/// TUI refuses to render derived numbers from it.
/// (TUI-VIEW-POS-005, TUI-VIEW-POS-011, TUI-VIEW-HIST-001/003; tui-design.md →
/// "Integrity errors")
// @spec TUI-VIEW-POS-005, TUI-VIEW-POS-011, TUI-VIEW-HIST-003
fn project_view(
    boot: &Boot,
    outcome: &CycleOutcome,
    tax_year: i32,
    bracket_state: config::BracketState,
    connection: Connection,
    updated_hhmm: Option<String>,
    display_names: config::DisplayNameMap,
) -> ViewState {
    let freshness = per_symbol_freshness(&outcome.marks);
    let (history_points, trading_day_calendar) = match load_history(boot) {
        Ok(points) => history_bundle(points),
        Err(reports::HistoryError::Unreachable) => (BTreeMap::new(), Vec::new()),
        Err(e) => {
            return blocked_view(
                tax_year,
                IntegrityError::CorruptLog(format!("History tab: {e}")),
            )
        }
    };
    view_state_from_cycle(
        outcome,
        freshness,
        history_points,
        trading_day_calendar,
        Vec::new(), // orphan warnings
        None,       // next estimated-payment period
        bracket_state,
        Vec::new(), // staleness signals
        tax_year,
        connection,
        updated_hhmm,
        display_names,
    )
}

/// Read the durable History series through the composition root's client (the
/// SAME one Sheets primitive every workbook touch rides). A client-construction
/// failure reads as an unreachable workbook.
fn load_history(boot: &Boot) -> Result<Vec<reports::SeriesPoint>, reports::HistoryError> {
    let client = boot
        .history_client(sheets_view::HISTORY_TAB)
        .map_err(|_| reports::HistoryError::Unreachable)?;
    reports::read_history(&client)
}

/// A view state blocked by an integrity failure (the loud `✗` treatment): derived
/// numbers are not rendered. Built with an empty snapshot.
fn blocked_view(tax_year: i32, integrity: IntegrityError) -> ViewState {
    ViewState {
        snapshot: ledger_core::Snapshot::default(),
        accruals: Vec::new(),
        annual_rows: Vec::new(),
        estimates: BTreeMap::new(),
        marks: BTreeMap::new(),
        freshness: BTreeMap::new(),
        trading_day_key: None,
        as_of_calendar: None,
        updated_hhmm: None,
        display_names: config::DisplayNameMap::default(),
        trading_day_calendar: Vec::new(),
        history_points: BTreeMap::new(),
        orphan_warnings: Vec::new(),
        connection: Connection::Offline,
        integrity: Some(integrity),
        tax_year,
        bracket_state: config::BracketState::NoBracketsAvailable,
        staleness: Vec::new(),
        next_estimated_payment_period: None,
    }
}
