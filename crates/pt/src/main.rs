//! `pt` — the portfolio-tracker binary. The thin argv -> subcommand -> stdout/exit
//! glue over the pure [`pt`] dispatch seam and the [`pt::wiring`] live host:
//!
//!   pt              (no args)  -> launch the interactive ratatui TUI
//!   pt summary [--json]        -> the headless daily-summary (text or json)
//!   pt import  [--commit]      -> the one-time legacy importer (dry-run default)
//!
//! Everything testable without a real terminal / live Sheets lives in the `pt`
//! library; this file is the I/O shell (CLAUDE.md "Verify the money math; trust the
//! I/O") and is confirmed by the manual `pt` run + the env-gated e2e.

use std::io::Write;
use std::process::ExitCode;

use pt::{load_settings, parse_command, settings_are_live, Command};

use pt::import_app;

mod tui_app;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_command(&args) {
        Command::Tui => run_tui(),
        Command::Summary { json } => run_summary(json),
        Command::Import { commit } => run_import(commit),
        Command::Unknown(tok) => {
            eprintln!("pt: unknown subcommand {tok:?}");
            eprintln!("usage: pt [summary [--json] | import [--commit]]   (no args: launch the TUI)");
            ExitCode::from(2)
        }
    }
}

// ===========================================================================
// `pt summary` — the headless daily-summary, run THROUGH `runtime`'s replay cycle.
// Loads the settings, connects the live host, drives ONE cycle, projects the
// SummaryInputs, runs `summary::run_summary`, and dispatches the rendered output +
// the process exit code. A non-live config exits 2 (no trustworthy summary) rather
// than printing a confident report.
// ===========================================================================

fn run_summary(json: bool) -> ExitCode {
    use summary::{dispatch, OutputMode};

    let mode = if json { OutputMode::Json } else { OutputMode::Text };
    let settings = load_settings();

    // No live workbook configured -> no trustworthy summary (exit 2), never a
    // fabricated report. (SUMMARY-EXIT-002)
    if !settings_are_live(&settings) {
        let run = summary::SummaryRun::Fatal(summary::FatalError::BadCredentials);
        let (out, exit) = dispatch(&run, mode);
        print!("{out}");
        let _ = std::io::stdout().flush();
        return ExitCode::from(exit.code() as u8);
    }

    match tui_app::run_headless_summary(settings, mode) {
        Ok((out, code)) => {
            print!("{out}");
            let _ = std::io::stdout().flush();
            ExitCode::from(code as u8)
        }
        Err(e) => {
            // A connection failure mid-run is no trustworthy summary (exit 2).
            let run = summary::SummaryRun::Fatal(summary::FatalError::BadCredentials);
            let (out, _exit) = dispatch(&run, mode);
            print!("{out}");
            eprintln!("pt summary: {e}");
            let _ = std::io::stdout().flush();
            ExitCode::from(2)
        }
    }
}

// ===========================================================================
// `pt import` — the one-time legacy importer. Dry-run by default (writes nothing):
// it reconstructs the event stream, validates it through the kernel, reconciles it
// against the legacy Positions, and prints the report. `--commit` is the explicit
// gated write step. The legacy workbook is parsed into `import::LegacyWorkbook` for
// a manual run; the binary refuses to fabricate one, so without a parsed source it
// reports that the legacy source must be supplied.
// ===========================================================================

fn run_import(commit: bool) -> ExitCode {
    // Fetch the legacy tabs READ-ONLY, parse the owner's layout, dry-run, render
    // the reconciliation report; `--commit` (after a reviewed dry run) accepts and
    // writes into the NEW workbook. The legacy workbook is never written.
    let settings = load_settings();
    if !settings_are_live(&settings) {
        eprintln!("pt import: no live settings (config.local.toml / PT_WORKBOOK_ID + credentials)");
        return ExitCode::from(2);
    }
    match import_app::run_import_flow(&settings, commit) {
        Ok((out, code)) => {
            print!("{out}");
            let _ = std::io::stdout().flush();
            ExitCode::from(code as u8)
        }
        Err(e) => {
            eprintln!("pt import: {e}");
            ExitCode::from(2)
        }
    }
}

// ===========================================================================
// `pt` (no args) — launch the interactive TUI. Delegates to the crossterm event
// loop in `tui_app`. A non-live config still launches: the shell renders an
// integrity block ("credentials unavailable") rather than crashing.
// ===========================================================================

fn run_tui() -> ExitCode {
    let settings = load_settings();
    match tui_app::run_tui(settings) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pt: {e}");
            ExitCode::FAILURE
        }
    }
}
