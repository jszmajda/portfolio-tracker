//! `pt` — the portfolio-tracker binary's library half: the pure argv -> subcommand
//! dispatch seam, the local-settings loader, and the production `runtime` wiring.
//!
//! The binary ([`crate::main`]) is the thin argv -> stdout -> exit glue over this
//! seam; everything testable without a real terminal or a live workbook lives here
//! so `cargo test` is the gate. The three subcommands run THROUGH `runtime` — the
//! shared host that owns the Sheets-access layer (the ONE Google Sheets client),
//! the cross-process advisory write-lock, and the replay -> project -> read-marks ->
//! cache cycle:
//!
//!   * default (no args) -> launch the interactive ratatui TUI;
//!   * `pt summary` -> the headless daily-summary (text, or `--json`);
//!   * `pt import`  -> the one-time legacy importer (dry-run by default).
//!
//! It is NOT a `verus!{}` crate (argv parsing + process wiring; CLAUDE.md "Verify
//! the money math; trust the I/O"). No float crosses here — money stays integer
//! `pt_core::Cents` / `MicroShares`, computed by the kernels it drives.

use std::path::Path;

pub mod import_app;
pub mod shell;
pub mod wiring;

/// The settings file the binary reads to point `runtime` at the workbook + creds:
/// gitignored (it carries the credentials path), beside the invocation. Env vars
/// override it so CI / the e2e can run with no committed file.
pub const LOCAL_CONFIG_FILE: &str = "config.local.toml";

// ===========================================================================
// The argv -> subcommand dispatch (the pure seam; unit-tested). Default (no args)
// launches the TUI; `summary` runs the headless summary; `import` runs the
// importer. An unknown first token is an error the binary reports on stderr.
// ===========================================================================

/// The selected subcommand the binary dispatches over `runtime`. Parsed from the
/// process arguments (after the program name) so the seam is pure and testable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    /// No args -> launch the interactive TUI. (the default)
    Tui,
    /// `pt summary [--json]` -> the headless daily-summary. `json` is `true` when
    /// `--json` was passed (the versioned object instead of the default text).
    Summary { json: bool },
    /// `pt import [--commit]` -> the one-time legacy importer. `commit` is `true`
    /// only when `--commit` was passed; the default is a dry-run (writes nothing).
    Import { commit: bool },
    /// An unrecognized first token. Carries it back so the binary can print a usage
    /// line on stderr and exit non-zero.
    Unknown(String),
}

/// Parse the process arguments (those AFTER the program name, e.g.
/// `std::env::args().skip(1)`) into the [`Command`] the binary dispatches:
///
/// - empty -> [`Command::Tui`] (the default: launch the TUI);
/// - `summary` (+ optional `--json`) -> [`Command::Summary`];
/// - `import` (+ optional `--commit`) -> [`Command::Import`] (dry-run by default);
/// - anything else -> [`Command::Unknown`].
///
/// Flag order after the subcommand does not matter; an unknown flag is ignored
/// (forward-compatible), matching `summary::select_mode`.
pub fn parse_command<I, S>(args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter();
    let Some(first) = iter.next() else {
        return Command::Tui;
    };
    let rest: Vec<String> = iter.map(|s| s.as_ref().to_string()).collect();
    match first.as_ref() {
        "summary" => Command::Summary {
            json: rest.iter().any(|a| a == "--json"),
        },
        "import" => Command::Import {
            commit: rest.iter().any(|a| a == "--commit"),
        },
        // The default the bare `pt` launches is the TUI; an explicit `tui` token is
        // accepted as its alias so a wrapper can name it.
        "tui" => Command::Tui,
        other => Command::Unknown(other.to_string()),
    }
}

// ===========================================================================
// The local-settings loader. The binary reads `config.local.toml` (gitignored) to
// point `runtime` at the workbook + service-account creds, with env-var overrides
// so CI / the e2e need no committed file. A minimal hand-rolled TOML reader (no
// serde/toml dep on the binary): only the four flat string keys `Settings` needs.
// ===========================================================================

/// Load the runtime [`config::Settings`] from (in precedence order, last wins):
/// the defaults, then `config.local.toml` if present beside the invocation, then
/// the environment (`PT_WORKBOOK_ID`, `GOOGLE_APPLICATION_CREDENTIALS`,
/// `PT_CACHE_PATH`, `PT_REPORTING_TZ`). The env overrides exist so CI and the
/// env-gated e2e run with no committed credentials file.
///
/// Returns the resolved settings; missing workbook id / credentials are NOT an
/// error here (the caller decides — the TUI surfaces it as an integrity block, the
/// e2e supplies them via env), so this never panics on a partial config.
pub fn load_settings() -> config::Settings {
    load_settings_from(LOCAL_CONFIG_FILE, |k| std::env::var(k))
}

/// The testable core of [`load_settings`]: read the TOML at `path` (if any), then
/// apply the env overrides via the injected `env` lookup (so a test drives it with
/// a fake environment, no real `std::env`). Pure but for the single file read.
pub fn load_settings_from<F>(path: impl AsRef<Path>, env: F) -> config::Settings
where
    F: Fn(&str) -> Result<String, std::env::VarError>,
{
    let mut settings = config::Settings::default();

    // 1. The gitignored local file, if present (best-effort: a missing/unreadable
    //    file leaves the defaults — env can still supply everything).
    if let Ok(contents) = std::fs::read_to_string(path.as_ref()) {
        for (key, val) in parse_flat_toml(&contents) {
            apply_setting(&mut settings, &key, &val);
        }
    }

    // 2. Env overrides (last wins) — the path CI and the e2e use.
    if let Ok(v) = env("PT_WORKBOOK_ID") {
        if !v.is_empty() {
            settings.workbook_id = v;
        }
    }
    if let Ok(v) = env("GOOGLE_APPLICATION_CREDENTIALS") {
        if !v.is_empty() {
            settings.credentials_path = v;
        }
    }
    if let Ok(v) = env("PT_CACHE_PATH") {
        if !v.is_empty() {
            settings.cache_path = v;
        }
    }
    if let Ok(v) = env("PT_REPORTING_TZ") {
        if !v.is_empty() {
            settings.reporting_timezone = v;
        }
    }

    settings
}

/// Apply one `key = value` pair from the local TOML to the settings. The recognized
/// keys mirror `config::Settings`'s fields (the four flat strings the binary needs).
fn apply_setting(settings: &mut config::Settings, key: &str, val: &str) {
    match key {
        "workbook_id" => settings.workbook_id = val.to_string(),
        "credentials_path" => settings.credentials_path = val.to_string(),
        "cache_path" => settings.cache_path = val.to_string(),
        "reporting_timezone" => settings.reporting_timezone = val.to_string(),
        _ => {} // unknown keys are ignored (forward-compatible)
    }
}

/// Parse a FLAT `key = "value"` TOML body into `(key, value)` pairs — only the
/// shape `Settings` needs (no tables, no arrays). Lines that are blank, comments
/// (`#`), or table headers (`[...]`) are skipped; a value's surrounding quotes are
/// stripped. Deliberately tiny so the binary takes no `toml`/serde dependency.
fn parse_flat_toml(contents: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        let mut val = val.trim();
        // Strip a trailing inline comment that is OUTSIDE quotes (a quoted `#` in a
        // path stays). Only strip when the value is unquoted.
        if !val.starts_with('"') && !val.starts_with('\'') {
            if let Some((before, _)) = val.split_once('#') {
                val = before.trim();
            }
        }
        let val = val
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string();
        out.push((key, val));
    }
    out
}

/// Whether the settings carry enough to open a live workbook: a non-empty workbook
/// id and a readable credentials file. The TUI / summary use this to decide between
/// a live cycle and an integrity / fatal degrade. (does not authenticate — only
/// checks presence + readability of the creds file.)
pub fn settings_are_live(settings: &config::Settings) -> bool {
    !settings.workbook_id.is_empty()
        && !settings.credentials_path.is_empty()
        && Path::new(&settings.credentials_path).exists()
}
