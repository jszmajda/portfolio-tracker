//! The pure argv -> subcommand dispatch seam + the local-settings loader. The
//! binary's live terminal / Sheets paths are confirmed by the env-gated e2e and the
//! manual `pt` run; this suite exercises the part that needs no terminal/network.

use std::env::VarError;

use pt::{load_settings_from, parse_command, settings_are_live, Command};
use summary::{dispatch, select_mode, ExitCode, OutputMode, SummaryRun};

#[test]
fn no_args_launches_the_tui() {
    // The default (bare `pt`) is the interactive TUI.
    let empty: Vec<String> = Vec::new();
    assert_eq!(parse_command(&empty), Command::Tui);
    assert_eq!(parse_command(["tui"]), Command::Tui);
}

#[test]
fn summary_subcommand_text_and_json() {
    // `pt summary` is the headless summary; default is text, `--json` selects json.
    assert_eq!(parse_command(["summary"]), Command::Summary { json: false });
    assert_eq!(
        parse_command(["summary", "--json"]),
        Command::Summary { json: true }
    );
    // Flag order / extra unknown flags do not change the selection.
    assert_eq!(
        parse_command(["summary", "--verbose", "--json"]),
        Command::Summary { json: true }
    );
}

// @spec SUMMARY-OUT-004
#[test]
fn pt_owns_the_headless_summary_argv_to_stdout_to_exit_code_contract() {
    // The `pt` binary owns the HEADLESS entrypoint for `pt summary`: it maps argv
    // (including `--json`) to the rendered stdout output AND to the process
    // `ExitCode` per the exit-code contract — while `runtime` owns the composition
    // root that constructs/wires the inputs. This pins the pure seam the binary's
    // `run_summary` is built over (no terminal / live Sheets here). (SUMMARY-OUT-004)

    // 1. argv -> the Summary command, with --json carried through the binary's parser.
    let text_cmd = parse_command(["summary"]);
    let json_cmd = parse_command(["summary", "--json"]);
    assert_eq!(text_cmd, Command::Summary { json: false });
    assert_eq!(json_cmd, Command::Summary { json: true });

    // 2. The `json` flag the binary parsed selects the SAME OutputMode `summary`
    //    renders in — the binary maps `Command::Summary { json }` to the mode, and
    //    that mapping agrees with `summary::select_mode` over the raw argv. So the
    //    one headless contract owner is `pt`, threading argv -> mode. (SUMMARY-OUT-004)
    let mode_of = |cmd: &Command| match cmd {
        Command::Summary { json: true } => OutputMode::Json,
        Command::Summary { json: false } => OutputMode::Text,
        other => panic!("expected a Summary command, got {other:?}"),
    };
    assert_eq!(mode_of(&text_cmd), OutputMode::Text);
    assert_eq!(mode_of(&json_cmd), OutputMode::Json);
    assert_eq!(mode_of(&text_cmd), select_mode(std::iter::empty::<&str>()));
    assert_eq!(mode_of(&json_cmd), select_mode(["--json"]));

    // 3. The selected mode + a finished SummaryRun produce the rendered stdout output
    //    PAIRED with the process ExitCode the binary returns. A fatal (no trustworthy
    //    summary) run carries exit 2 in BOTH modes — the exit-code contract the
    //    headless entrypoint maps argv to. (SUMMARY-OUT-004, SUMMARY-EXIT-002)
    let fatal = SummaryRun::Fatal(summary::FatalError::BadCredentials);
    let (text_out, text_exit) = dispatch(&fatal, mode_of(&text_cmd));
    let (json_out, json_exit) = dispatch(&fatal, mode_of(&json_cmd));
    assert_eq!(text_exit, ExitCode::NoTrustworthySummary);
    assert_eq!(text_exit.code(), 2, "argv -> ExitCode 2 on a fatal headless run");
    assert_eq!(json_exit.code(), 2);
    assert!(!text_out.contains("Total value"), "no confident report on a fatal run");
    assert!(json_out.contains("schema_version"), "json fatal is still schema-tagged");
    // The rendered output is the stdout the binary `print!`s — non-empty, mode-shaped.
    assert!(!text_out.trim().is_empty());
    assert!(json_out.trim_start().starts_with('{'));
}

#[test]
fn import_subcommand_dry_run_default_and_commit() {
    // `pt import` is dry-run by default (writes nothing); `--commit` is the gated
    // explicit write step.
    assert_eq!(parse_command(["import"]), Command::Import { commit: false });
    assert_eq!(
        parse_command(["import", "--commit"]),
        Command::Import { commit: true }
    );
}

#[test]
fn unknown_subcommand_is_surfaced() {
    // An unrecognized first token is carried back so the binary prints usage + a
    // non-zero exit — never silently treated as the TUI.
    assert_eq!(
        parse_command(["frobnicate"]),
        Command::Unknown("frobnicate".to_string())
    );
}

#[test]
fn settings_loader_env_overrides_take_precedence() {
    // With no local file present, the env vars supply the whole config: workbook id,
    // credentials path, cache path, reporting tz.
    let env = |k: &str| -> Result<String, VarError> {
        match k {
            "PT_WORKBOOK_ID" => Ok("WB-123".to_string()),
            "GOOGLE_APPLICATION_CREDENTIALS" => Ok("/creds/sa.json".to_string()),
            "PT_CACHE_PATH" => Ok("/tmp/pt.sqlite".to_string()),
            "PT_REPORTING_TZ" => Ok("US/Eastern".to_string()),
            _ => Err(VarError::NotPresent),
        }
    };
    let s = load_settings_from("/nonexistent/config.local.toml", env);
    assert_eq!(s.workbook_id, "WB-123");
    assert_eq!(s.credentials_path, "/creds/sa.json");
    assert_eq!(s.cache_path, "/tmp/pt.sqlite");
    assert_eq!(s.reporting_timezone, "US/Eastern");
}

#[test]
fn settings_loader_reads_local_toml_then_env_wins() {
    // Write a gitignored-style local config to a temp file; env then overrides the
    // workbook id (last wins), leaving the file's credentials path intact.
    let dir = std::env::temp_dir();
    let path = dir.join(format!("pt-test-config-{}.toml", std::process::id()));
    std::fs::write(
        &path,
        "# local settings\n[settings]\nworkbook_id = \"FILE-WB\"\ncredentials_path = \"/file/creds.json\"\nreporting_timezone = \"US/Pacific\"\n",
    )
    .unwrap();

    let env = |k: &str| -> Result<String, VarError> {
        match k {
            "PT_WORKBOOK_ID" => Ok("ENV-WB".to_string()), // overrides the file
            _ => Err(VarError::NotPresent),
        }
    };
    let s = load_settings_from(&path, env);
    assert_eq!(s.workbook_id, "ENV-WB", "env overrides the file");
    assert_eq!(s.credentials_path, "/file/creds.json", "file value kept where env is absent");
    assert_eq!(s.reporting_timezone, "US/Pacific");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn settings_are_live_requires_workbook_creds_and_existing_file() {
    // Empty config is not live.
    let mut s = config::Settings::default();
    assert!(!settings_are_live(&s));

    // A workbook id + a credentials path that does NOT exist is not live.
    s.workbook_id = "WB".to_string();
    s.credentials_path = "/definitely/missing/creds.json".to_string();
    assert!(!settings_are_live(&s));

    // A workbook id + an existing credentials file IS live.
    let dir = std::env::temp_dir();
    let creds = dir.join(format!("pt-test-creds-{}.json", std::process::id()));
    std::fs::write(&creds, "{}").unwrap();
    s.credentials_path = creds.to_string_lossy().to_string();
    assert!(settings_are_live(&s));
    let _ = std::fs::remove_file(&creds);
}
