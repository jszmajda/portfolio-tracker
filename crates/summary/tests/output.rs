//! Output Modes: SUMMARY-OUT-001, SUMMARY-OUT-002.
#![allow(clippy::inconsistent_digit_grouping)]

mod common;

use common::*;
use config::BracketState;
use summary::testkit::{FakeLockProbe, InMemoryHistory, NoopLock};
use summary::{
    build_report, compute_delta, dispatch, render_json, render_text, run_summary, select_mode,
    CaptureOutcome, ExitCode, OutputMode, Report, SummaryRun, TrustState, JSON_SCHEMA_VERSION,
};

/// A fresh-run report with a real point-to-point delta.
fn fresh_report() -> Report {
    let inputs = inputs_at(19_490, 200_00);
    let current = summary::build_current_point(&inputs).unwrap();
    let stored = vec![point_at(19_485, 190_00)];
    let capture = CaptureOutcome::Appended(current);
    let delta = compute_delta(&capture, &stored, &[]);
    build_report(&inputs, &capture, delta, stored.first())
}

/// A stale (lock-held) report: uncaptured, marked stale in-band.
fn stale_report() -> Report {
    let inputs = inputs_at(19_490, 200_00);
    let live = summary::build_current_point(&inputs).unwrap();
    let stored = vec![point_at(19_485, 190_00)];
    let capture = CaptureOutcome::SkippedLockHeld(live);
    let delta = compute_delta(&capture, &stored, &[]);
    build_report(&inputs, &capture, delta, stored.first())
}

// @spec SUMMARY-OUT-001
#[test]
fn text_is_the_default_report_to_stdout() {
    let report = fresh_report();
    let text = render_text(&report);
    // A non-empty ASCII report naming the portfolio's symbols and totals.
    assert!(text.is_ascii(), "the terminal report is ASCII");
    assert!(text.contains("AMZN"), "names the positions");
    assert!(text.contains("GOOG"));
    assert!(!text.trim().is_empty());
}

// @spec SUMMARY-OUT-001
#[test]
fn json_is_a_schema_version_tagged_object() {
    let report = fresh_report();
    let json = render_json(&report);
    // A versioned object — schema_version present so consumers gate on shape
    // changes (no recreating the legacy scrape fragility). (SUMMARY-OUT-001)
    assert!(json.contains("schema_version"));
    assert!(json.contains(&JSON_SCHEMA_VERSION.to_string()));
    // The versioned shape carries the documented kernels.
    assert!(json.contains("trading_day"));
    assert!(json.contains("total_value_cents"));
    assert!(json.contains("net_post_tax_cents"));
    assert!(json.contains("total_delta_cents"));
    assert!(json.contains("positions"));
    assert!(json.contains("tax"));
    assert!(json.contains("brackets_state"));
    assert!(json.contains("stale"));
    assert!(json.contains("degraded_symbols"));
    // It parses as a JSON object.
    assert!(json.trim_start().starts_with('{') && json.trim_end().ends_with('}'));
}

// @spec SUMMARY-OUT-002
#[test]
fn stale_is_marked_in_band_in_text_and_json() {
    let report = stale_report();
    assert!(report.stale, "a lock-held / offline run is stale");

    // Text marks staleness in-band (a visible marker).
    let text = render_text(&report);
    let lower = text.to_lowercase();
    assert!(lower.contains("stale"), "text marks staleness in-band");

    // JSON marks staleness via `stale: true`.
    let json = render_json(&report);
    assert!(json.contains("\"stale\""));
    assert!(json.contains("\"stale\":true") || json.contains("\"stale\": true"));
}

// @spec SUMMARY-OUT-002
#[test]
fn a_fresh_run_is_not_marked_stale() {
    let report = fresh_report();
    assert!(!report.stale, "a fresh captured run is not stale");
    let json = render_json(&report);
    assert!(json.contains("\"stale\":false") || json.contains("\"stale\": false"));
}

// @spec SUMMARY-OUT-001
#[test]
fn json_reflects_the_bracket_state_on_cold_start() {
    // The versioned JSON carries the bracket state (so a consumer sees cold-start).
    let inputs = with_bracket_state(inputs_at(19_490, 200_00), BracketState::NoBracketsAvailable);
    let current = summary::build_current_point(&inputs).unwrap();
    let capture = CaptureOutcome::Appended(current);
    let delta = compute_delta(&capture, &[], &[]);
    let report = build_report(&inputs, &capture, delta, None);
    let json = render_json(&report);
    assert!(json.contains("brackets_state"));
    // net_post_tax_cents is null on cold-start (never fabricated).
    assert!(
        json.contains("\"net_post_tax_cents\":null")
            || json.contains("\"net_post_tax_cents\": null")
    );
}

// ===========================================================================
// SUMMARY-OUT-003: the schema_version = 1 --json shape is PINNED — the exact
// top-level, positions-element, and tax key sets (not just contains()-substrings),
// integer cents only, with null (never a fabricated zero) for a degraded /
// cold-start figure. A small brace-aware key extractor reads the object's keys.
// ===========================================================================

/// Extract the ordered keys of the FIRST JSON object found starting at/after `from`
/// in `s`, descending exactly one level (nested objects/arrays are skipped, so only
/// this object's own keys are returned). The summary JSON is hand-built and ASCII;
/// keys are the `"..."` tokens at brace-depth 1 immediately followed by `:`.
fn object_keys(s: &str, from: usize) -> Vec<String> {
    let bytes = s.as_bytes();
    let start = from + s[from..].find('{').expect("an object");
    let mut depth = 0usize;
    let mut keys = Vec::new();
    let mut i = start;
    let mut in_str = false;
    let mut cur = String::new();
    let mut str_start = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if c == '"' {
                in_str = false;
                // A key is a depth-1 string immediately followed by `:`.
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                    j += 1;
                }
                if depth == 1 && j < bytes.len() && bytes[j] as char == ':' {
                    keys.push(cur.clone());
                }
                cur.clear();
                let _ = str_start;
            } else {
                cur.push(c);
            }
        } else {
            match c {
                '"' => {
                    in_str = true;
                    str_start = i;
                }
                '{' | '[' => depth += 1,
                '}' | ']' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    keys
}

/// The exact set of keys (order-independent) of the object at/after `from`.
fn key_set(s: &str, from: usize) -> std::collections::BTreeSet<String> {
    object_keys(s, from).into_iter().collect()
}

fn expected(keys: &[&str]) -> std::collections::BTreeSet<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

// @spec SUMMARY-OUT-003
#[test]
fn json_schema_v1_pins_the_exact_key_sets() {
    let report = fresh_report();
    let json = render_json(&report);

    // schema_version is exactly 1.
    assert!(
        json.contains("\"schema_version\":1"),
        "schema_version is the pinned version 1"
    );

    // The TOP-LEVEL object carries EXACTLY these keys — no more, no fewer.
    // (SUMMARY-OUT-003)
    let top = key_set(&json, 0);
    assert_eq!(
        top,
        expected(&[
            "schema_version",
            "trading_day",
            "baseline_day",
            "run_at",
            "total_value_cents",
            "net_post_tax_cents",
            "total_delta_cents",
            "positions",
            "tax",
            "stale",
            "degraded_symbols",
        ]),
        "top-level key set is pinned exactly"
    );

    // Each POSITIONS element carries EXACTLY these keys. (SUMMARY-OUT-003)
    let pos_at = json.find("\"positions\":").expect("positions present");
    let first_elem = pos_at + json[pos_at..].find('{').expect("a positions element");
    let pos_keys = key_set(&json, first_elem);
    assert_eq!(
        pos_keys,
        expected(&[
            "symbol",
            "shares",
            "price_cents",
            "value_cents",
            "delta_cents",
            "degraded"
        ]),
        "positions-element key set is pinned exactly"
    );
    assert!(
        !report.positions.is_empty(),
        "the report actually has positions to pin"
    );

    // The TAX object carries EXACTLY these keys — including next_period (EMIT-006).
    // (SUMMARY-OUT-003)
    let tax_at = json.find("\"tax\":").expect("tax present");
    let tax_keys = key_set(&json, tax_at);
    assert_eq!(
        tax_keys,
        expected(&[
            "year",
            "accrued",
            "moved",
            "outstanding",
            "next_period",
            "brackets_state"
        ]),
        "tax key set is pinned exactly (carries next_period per EMIT-006)"
    );
}

// @spec SUMMARY-OUT-003
#[test]
fn json_schema_v1_uses_integer_cents_and_null_never_a_fabricated_zero() {
    // Cold-start: bracket-dependent figures are null (never a fabricated 0), integer
    // cents elsewhere. (SUMMARY-OUT-003)
    let inputs = with_bracket_state(inputs_at(19_490, 200_00), BracketState::NoBracketsAvailable);
    let current = summary::build_current_point(&inputs).unwrap();
    let capture = CaptureOutcome::Appended(current);
    let delta = compute_delta(&capture, &[], &[]);
    let report = build_report(&inputs, &capture, delta, None);
    let json = render_json(&report);

    // The cold-start / first-ever figures are JSON null, never a fabricated zero.
    assert!(
        json.contains("\"net_post_tax_cents\":null"),
        "net is null on cold-start"
    );
    assert!(
        json.contains("\"total_delta_cents\":null"),
        "first-ever delta is null"
    );
    assert!(
        json.contains("\"accrued\":null"),
        "accrued is null on cold-start"
    );
    assert!(json.contains("\"moved\":null"));
    assert!(json.contains("\"outstanding\":null"));
    assert!(
        json.contains("\"next_period\":null"),
        "no fabricated period on cold-start"
    );
    // Never a fabricated zero for any of those degraded/cold-start figures.
    assert!(!json.contains("\"net_post_tax_cents\":0"));
    assert!(!json.contains("\"accrued\":0"));

    // total_value_cents is the integer-cents market value (bracket-independent) — a
    // plain integer, not a string, not a float.
    assert!(
        json.contains(&format!(
            "\"total_value_cents\":{}",
            report.header.total_value_cents.0
        )),
        "integer cents only"
    );
    assert!(!json.contains('.'), "no float anywhere in the cents JSON");
}

/// A produced (fresh) run through the full orchestration, for dispatch tests.
fn produced_run() -> SummaryRun {
    let mut client = InMemoryHistory::new();
    client.set_rows(vec![row(&point_at(19_485, 190_00))]);
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    let inputs = inputs_at(19_490, 200_00);
    run_summary(&mut client, &lock, &probe, TrustState::Ok, &inputs)
}

// @spec SUMMARY-OUT-001
#[test]
fn the_default_mode_is_text_and_json_is_selected_by_the_flag() {
    // No flag → text is the default; --json selects the JSON object. Unknown flags
    // leave the default unchanged (forward-compatible). (SUMMARY-OUT-001)
    assert_eq!(
        select_mode(std::iter::empty::<&str>()),
        OutputMode::Text,
        "default is text"
    );
    assert_eq!(
        select_mode(["--json"]),
        OutputMode::Json,
        "--json selects json"
    );
    assert_eq!(
        select_mode(["--quiet"]),
        OutputMode::Text,
        "unknown flag → default text"
    );
    assert_eq!(select_mode(["--verbose", "--json"]), OutputMode::Json);
}

// @spec SUMMARY-OUT-001
#[test]
fn dispatch_selects_the_renderer_and_pairs_the_exit_code() {
    let run = produced_run();

    // No flag → the text report (matches render_text), exit 0. (SUMMARY-OUT-001)
    let (text_out, text_exit) = dispatch(&run, select_mode(std::iter::empty::<&str>()));
    let SummaryRun::Produced(ref report) = run else {
        panic!("expected produced")
    };
    assert_eq!(
        text_out,
        render_text(report),
        "default dispatch == render_text"
    );
    assert!(text_out.contains("AMZN"));
    assert_eq!(text_exit, ExitCode::Produced);
    assert_eq!(text_exit.code(), 0, "exit equals exit_code().code()");

    // --json → the versioned JSON object (matches render_json), exit 0.
    let (json_out, json_exit) = dispatch(&run, select_mode(["--json"]));
    assert_eq!(
        json_out,
        render_json(report),
        "--json dispatch == render_json"
    );
    assert!(json_out.contains("schema_version"));
    assert_eq!(json_exit.code(), run.exit_code().code());
}

// @spec SUMMARY-EXIT-002
#[test]
fn dispatch_of_a_fatal_run_carries_exit_2_and_never_a_confident_report() {
    // A fatal run (bad creds) dispatches a short diagnostic, never a fabricated
    // report, and pairs the exit-2 code in both modes. (SUMMARY-EXIT-002)
    let mut client = InMemoryHistory::new();
    let lock = NoopLock::new();
    let probe = FakeLockProbe::free();
    let inputs = inputs_at(19_490, 200_00);
    let run = run_summary(
        &mut client,
        &lock,
        &probe,
        TrustState::BadCredentials,
        &inputs,
    );

    let (text_out, exit) = dispatch(&run, OutputMode::Text);
    assert_eq!(exit, ExitCode::NoTrustworthySummary);
    assert_eq!(exit.code(), 2);
    assert!(
        !text_out.contains("Total value"),
        "no confident report on a fatal run"
    );

    let (json_out, json_exit) = dispatch(&run, OutputMode::Json);
    assert_eq!(json_exit.code(), 2);
    assert!(json_out.contains("error"), "json fatal carries an error");
    assert!(json_out.contains("schema_version"));
}
