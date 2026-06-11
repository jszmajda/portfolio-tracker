//! The single low-level Sheets-access layer: RUNTIME-SHEETS-001/002. The real
//! client's pure pieces (backoff schedule, retryability, the JWT/auth parse) are
//! exercised here without network; the I/O methods are driven through the
//! adapter against the FAKE low-level API. The real GoogleSheetsApi against a real
//! workbook is the finalize e2e.
#![allow(clippy::inconsistent_digit_grouping)]

use std::time::Duration;

use std::cell::RefCell;

use runtime::auth::{AccessToken, AuthError, ServiceAccount, SHEETS_SCOPE};
use runtime::sheets::{is_retryable_status, SheetsError, StoreSheetsAdapter};
use runtime::testkit::FakeSheetsApi;
use runtime::{run_with_retry, BackoffPolicy};

use ledger_core::{LedgerEvent, LedgerEventKind};
use pt_core::{Cents, Date, MicroShares, Seq};
use store::serde_rows;
use store::{SheetsClient, Tab};

// @spec RUNTIME-SHEETS-001
#[test]
fn backoff_schedule_is_capped_exponential() {
    // The rate-limit/backoff policy (Deferred-2, landed here): attempt 1 waits
    // nothing; attempt n >= 2 waits base * 2^(n-2), capped at max_delay.
    // (RUNTIME-SHEETS-001)
    let policy = BackoffPolicy {
        max_attempts: 5,
        base_delay: Duration::from_millis(500),
        max_delay: Duration::from_secs(4),
    };
    assert_eq!(
        policy.delay_for(1),
        Duration::ZERO,
        "the first attempt waits nothing"
    );
    assert_eq!(policy.delay_for(2), Duration::from_millis(500));
    assert_eq!(policy.delay_for(3), Duration::from_secs(1));
    assert_eq!(policy.delay_for(4), Duration::from_secs(2));
    // Capped at max_delay (would be 4s then 8s; capped to 4s).
    assert_eq!(policy.delay_for(5), Duration::from_secs(4));
    assert_eq!(
        policy.delay_for(6),
        Duration::from_secs(4),
        "delay is capped"
    );
}

// @spec RUNTIME-SHEETS-001
#[test]
fn retry_budget_bounds_attempts() {
    let policy = BackoffPolicy {
        max_attempts: 3,
        ..BackoffPolicy::default()
    };
    assert!(policy.may_retry(1), "may retry after 1 attempt");
    assert!(policy.may_retry(2));
    assert!(
        !policy.may_retry(3),
        "no retry once the attempt cap is reached"
    );
}

// @spec RUNTIME-SHEETS-001
#[test]
fn only_429_and_5xx_are_retryable() {
    // The throttling policy retries rate-limit (429) and transient server (5xx)
    // errors, and fails fast on a 4xx (bad range / missing tab / permission).
    // (RUNTIME-SHEETS-001)
    assert!(is_retryable_status(429), "rate-limit is retryable");
    assert!(is_retryable_status(500));
    assert!(is_retryable_status(503));
    assert!(!is_retryable_status(400), "a bad request fails fast");
    assert!(!is_retryable_status(403), "a permission error fails fast");
    assert!(!is_retryable_status(404), "a missing tab fails fast");
    assert!(!is_retryable_status(200));
}

// @spec RUNTIME-SHEETS-001
#[test]
fn retry_loop_retries_a_429_then_5xx_then_succeeds() {
    // The actual retry/backoff CONTROL LOOP (not just the predicate): a 429 then a
    // 503 are retried with backoff, and the third attempt's 200 succeeds — so the
    // loop made exactly 3 attempts and returned Ok. Driven over a FAKE transport (a
    // programmed status sequence) with a no-op sleep that records the schedule, so
    // there is no network and no real waiting. (RUNTIME-SHEETS-001)
    let policy = BackoffPolicy {
        max_attempts: 5,
        base_delay: Duration::from_millis(10),
        max_delay: Duration::from_secs(1),
    };
    // The programmed transport responses, consumed in order.
    let responses: RefCell<Vec<Result<&str, SheetsError>>> = RefCell::new(vec![
        Err(SheetsError::Api {
            status: 429,
            message: "rate limited".into(),
        }),
        Err(SheetsError::Api {
            status: 503,
            message: "transient".into(),
        }),
        Ok("body"),
    ]);
    let attempts = RefCell::new(0u32);
    let slept: RefCell<Vec<Duration>> = RefCell::new(Vec::new());

    let result = run_with_retry(
        &policy,
        || {
            *attempts.borrow_mut() += 1;
            responses.borrow_mut().remove(0)
        },
        |d| slept.borrow_mut().push(d),
    );

    assert_eq!(
        result,
        Ok("body"),
        "the loop returns the final success body"
    );
    assert_eq!(
        *attempts.borrow(),
        3,
        "the loop made exactly 3 attempts (429, 503, 200)"
    );
    // Two retries happened, so two backoff sleeps were scheduled, exponentially.
    let slept = slept.borrow();
    assert_eq!(slept.len(), 2, "two retries → two backoff sleeps");
    assert_eq!(
        slept[0],
        Duration::from_millis(10),
        "first retry waits base"
    );
    assert_eq!(
        slept[1],
        Duration::from_millis(20),
        "second retry waits 2·base"
    );
}

// @spec RUNTIME-SHEETS-001
#[test]
fn retry_loop_fails_fast_on_a_4xx_without_retrying() {
    // A non-retryable 4xx (a bad range / missing tab / permission) fails fast: the
    // loop makes exactly ONE attempt, never sleeps, and surfaces the terminal Api
    // error unchanged — it does NOT get mapped to Unreachable or retried.
    // (RUNTIME-SHEETS-001)
    let policy = BackoffPolicy::default();
    let attempts = RefCell::new(0u32);
    let slept: RefCell<Vec<Duration>> = RefCell::new(Vec::new());

    let result: Result<(), SheetsError> = run_with_retry(
        &policy,
        || {
            *attempts.borrow_mut() += 1;
            Err(SheetsError::Api {
                status: 403,
                message: "forbidden".into(),
            })
        },
        |d| slept.borrow_mut().push(d),
    );

    assert!(
        matches!(result, Err(SheetsError::Api { status: 403, .. })),
        "a 4xx surfaces the terminal Api error unchanged, got {result:?}"
    );
    assert_eq!(*attempts.borrow(), 1, "a 4xx is not retried");
    assert!(slept.borrow().is_empty(), "a fail-fast error never sleeps");
}

// @spec RUNTIME-SHEETS-001
#[test]
fn retry_loop_exhausts_the_budget_then_surfaces_unreachable() {
    // A retryable error that never clears exhausts the attempt budget and surfaces
    // Unreachable (return control to the owner) — the loop tries exactly
    // max_attempts times, then gives up. (RUNTIME-SHEETS-001)
    let policy = BackoffPolicy {
        max_attempts: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(4),
    };
    let attempts = RefCell::new(0u32);

    let result: Result<(), SheetsError> = run_with_retry(
        &policy,
        || {
            *attempts.borrow_mut() += 1;
            Err(SheetsError::Api {
                status: 429,
                message: "still limited".into(),
            })
        },
        |_| {},
    );

    assert!(
        matches!(result, Err(SheetsError::Unreachable(_))),
        "an exhausted budget surfaces Unreachable, got {result:?}"
    );
    assert_eq!(
        *attempts.borrow(),
        3,
        "the loop tried exactly max_attempts times"
    );
}

// @spec RUNTIME-SHEETS-001
#[test]
fn retry_loop_retries_a_transient_transport_error_then_gives_up() {
    // A transient transport failure (Unreachable, e.g. a dropped connection) is
    // retryable too: the loop retries it up to the budget, then surfaces the last
    // Unreachable. (RUNTIME-SHEETS-001)
    let policy = BackoffPolicy {
        max_attempts: 2,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
    };
    let attempts = RefCell::new(0u32);
    let result: Result<(), SheetsError> = run_with_retry(
        &policy,
        || {
            *attempts.borrow_mut() += 1;
            Err(SheetsError::Unreachable("connection reset".into()))
        },
        |_| {},
    );
    assert!(matches!(result, Err(SheetsError::Unreachable(_))));
    assert_eq!(
        *attempts.borrow(),
        2,
        "a transient transport error is retried to the budget"
    );
}

// @spec RUNTIME-SHEETS-001
#[test]
fn retry_loop_fails_fast_on_an_auth_error() {
    // An auth error is permanent (a misconfigured share / bad credentials), so the
    // loop does NOT retry it — it surfaces immediately. (RUNTIME-SHEETS-001)
    let policy = BackoffPolicy::default();
    let attempts = RefCell::new(0u32);
    let result: Result<(), SheetsError> = run_with_retry(
        &policy,
        || {
            *attempts.borrow_mut() += 1;
            Err(SheetsError::Auth("bad credentials".into()))
        },
        |_| {},
    );
    assert!(matches!(result, Err(SheetsError::Auth(_))));
    assert_eq!(*attempts.borrow(), 1, "an auth error is not retried");
}

// @spec RUNTIME-SHEETS-001
#[test]
fn service_account_mints_a_signed_jwt_from_credentials_json() {
    // The auth flow loads the standard Google service-account JSON and mints a
    // signed RS256 JWT scoped to the Sheets API — the single auth site. Uses a
    // throwaway test RSA key (NOT the real credentials). (RUNTIME-SHEETS-001)
    let pem = include_str!("test_rsa_key.pem");
    // Build the credentials JSON with proper escaping (serde_json) so the multi-
    // line PEM survives the round trip.
    let json = serde_json::json!({
        "client_email": "svc@proj.iam.gserviceaccount.com",
        "private_key": pem,
        "token_uri": "https://oauth2.googleapis.com/token",
    })
    .to_string();
    let account = ServiceAccount::from_json(&json).expect("parse service account");
    assert_eq!(account.client_email(), "svc@proj.iam.gserviceaccount.com");

    let jwt = account.mint_assertion().expect("mint a signed JWT");
    // A JWS is three base64url segments separated by dots.
    assert_eq!(
        jwt.split('.').count(),
        3,
        "a signed JWT has header.payload.signature"
    );
    // The scope is the Sheets scope (the assertion authorizes the Sheets API).
    assert_eq!(SHEETS_SCOPE, "https://www.googleapis.com/auth/spreadsheets");
}

// @spec RUNTIME-SHEETS-001
#[test]
fn malformed_credentials_surface_a_loud_auth_error() {
    // Missing/garbage credentials surface a hard Auth error, never a silent
    // unauthenticated client. (RUNTIME-SHEETS-001)
    let err = ServiceAccount::from_json("not json").unwrap_err();
    assert!(matches!(err, AuthError::CredentialsMalformed(_)));

    // A well-formed JSON but an unusable private key fails at signing time.
    let bad_key = r#"{"client_email":"x@y.iam","private_key":"-----BEGIN PRIVATE KEY-----\nnope\n-----END PRIVATE KEY-----\n","token_uri":"https://oauth2.googleapis.com/token"}"#;
    let account = ServiceAccount::from_json(bad_key).expect("parse");
    assert!(
        account.mint_assertion().is_err(),
        "an unusable RSA key fails to sign"
    );
}

// @spec RUNTIME-SHEETS-003, RUNTIME-SHEETS-004
#[test]
fn cached_token_is_reused_until_expiry_then_treated_expired_at_the_margin_boundary() {
    // The token is minted lazily, cached, and reused UNTIL expiry (RUNTIME-SHEETS-003);
    // the 60s safety margin is baked into `expires_at_secs` (stored expiry = real
    // expiry − 60s), so `is_expired` flips at that pre-margined boundary — the client
    // refreshes before issuing rather than letting a token expire mid-flight
    // (RUNTIME-SHEETS-004). We exercise the reuse/refresh DECISION (`is_expired`) the
    // cache consumer rides on, without touching the network (the live mint + the
    // numeric margin application are the env-gated e2e).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // A token whose (already-margined) stored expiry is comfortably in the future is
    // NOT expired: the cached token is reused, no re-mint. (RUNTIME-SHEETS-003)
    let fresh = AccessToken {
        bearer: "fresh".to_string(),
        expires_at_secs: now + 300,
    };
    assert!(
        !fresh.is_expired(),
        "a token before its margined expiry is reused, not re-minted"
    );

    // A token whose margined stored expiry has already passed reads as expired: the
    // client refreshes before issuing. (RUNTIME-SHEETS-004)
    let stale = AccessToken {
        bearer: "stale".to_string(),
        expires_at_secs: now.saturating_sub(1),
    };
    assert!(
        stale.is_expired(),
        "a token at/after its margined expiry is treated as expired"
    );
}

// @spec RUNTIME-SHEETS-002
#[test]
fn store_write_discipline_is_built_on_the_one_low_level_primitive() {
    // store's append + read-back-verify rides the ONE runtime Sheets primitive via
    // the adapter — no second client. Driving store's SheetsClient through the
    // adapter against the fake low-level API: an append goes through the primitive's
    // append, and a read goes through its read. (RUNTIME-SHEETS-002)
    let fake = FakeSheetsApi::new();
    let mut adapter = StoreSheetsAdapter::new(fake);

    // Seed the header row so the adapter's read skips it (row 0 = header).
    let header: Vec<String> = Tab::Ledger.header().iter().map(|s| s.to_string()).collect();
    adapter.api().seed(
        &format!("'{}'!A1:{}", Tab::Ledger.name(), last_col(Tab::Ledger)),
        vec![header.clone()],
    );

    // Append a ledger event row through store's SheetsClient (built on the primitive).
    let ev = LedgerEvent {
        id: "e1".to_string(),
        seq: Seq(1),
        date: Date(19_000),
        kind: LedgerEventKind::Buy {
            lot_id: "lot-1".to_string(),
            symbol: "AMZN".to_string(),
            qty: MicroShares(1_000_000),
            unit_price_cents: Cents(150_00),
            fees_cents: Cents(0),
            platform: "schwab".to_string(),
            tracking_code: None,
        },
    };
    let row = serde_rows::ledger_to_row(&ev);
    adapter
        .append_row(Tab::Ledger, &row)
        .expect("append through the primitive");

    // The append went through the ONE low-level append (count incremented).
    assert_eq!(
        adapter.api().append_count(),
        1,
        "store's append rode the low-level primitive"
    );

    // A read through the adapter sees the appended row (header skipped), proving the
    // segment's write discipline reads back through the SAME primitive.
    let back = adapter
        .read_rows(Tab::Ledger)
        .expect("read through the primitive");
    assert_eq!(
        back.len(),
        1,
        "the appended row is read back through the primitive"
    );
    assert_eq!(back[0].get("EventId"), "e1");
    assert!(
        adapter.api().read_count() >= 1,
        "the read rode the low-level primitive"
    );
}

// @spec RUNTIME-SHEETS-002
#[test]
fn read_probe_reads_and_parses_the_fixed_fingerprint_metadata_range() {
    // store's cheap currency probe (STORE-CACHE-002) reads the dedicated fingerprint
    // metadata range — fixed at 'Tab'!AA1:AC1, cells (count, max_seq, checksum) — via
    // the ONE low-level primitive. This pins the range + parse order so a store
    // schema change that moves the fingerprint cells fails loudly here, not silently.
    // (RUNTIME-SHEETS-002, STORE-CACHE-002)
    let fake = FakeSheetsApi::new();
    // Seed the probe row at the exact metadata range the adapter reads.
    fake.seed(
        &format!("'{}'!AA1:AC1", Tab::Ledger.name()),
        vec![vec![
            "7".to_string(),
            "42".to_string(),
            "123456".to_string(),
        ]],
    );
    let adapter = StoreSheetsAdapter::new(fake);

    let probe = adapter
        .read_probe(Tab::Ledger)
        .expect("read the probe through the primitive");
    assert_eq!(probe.count, Some(7), "count parses from AA1");
    assert_eq!(probe.max_seq, Some(42), "max_seq parses from AB1");
    assert_eq!(probe.checksum, Some(123456), "checksum parses from AC1");
    assert!(probe.is_present(), "a well-formed probe row is present");
    assert!(
        adapter.api().read_count() >= 1,
        "the probe rode the low-level read primitive"
    );
}

// @spec RUNTIME-SHEETS-002
#[test]
fn read_probe_treats_missing_fingerprint_cells_as_a_change() {
    // A missing/empty fingerprint range yields a probe with None cells (not present),
    // which the store probe treats as a change. (RUNTIME-SHEETS-002, STORE-CACHE-002)
    let fake = FakeSheetsApi::new();
    let adapter = StoreSheetsAdapter::new(fake);
    let probe = adapter
        .read_probe(Tab::Ledger)
        .expect("read an absent probe");
    assert_eq!(probe.count, None);
    assert_eq!(probe.max_seq, None);
    assert_eq!(probe.checksum, None);
    assert!(
        !probe.is_present(),
        "an absent probe is not present (treated as a change)"
    );
}

// @spec RUNTIME-SHEETS-002
#[test]
fn batch_update_and_clear_ride_the_one_primitive_with_tail_truncate() {
    // store's full-tab batchUpdate (with tail-truncate) and clear ride the ONE
    // primitive (update_range / clear_range), not a second client — the rest of
    // store's write discipline named in the spec. (RUNTIME-SHEETS-002)
    let fake = FakeSheetsApi::new();
    let mut adapter = StoreSheetsAdapter::new(fake);

    // Seed a header + three stale data rows on the Ledger tab.
    let header: Vec<String> = Tab::Ledger.header().iter().map(|s| s.to_string()).collect();
    let full = format!("'{}'!A1:{}", Tab::Ledger.name(), last_col(Tab::Ledger));
    adapter.api().seed(&full, vec![header.clone()]);
    let stale_row = ledger_row("stale-1", 1);
    let r2 = serde_rows::ledger_to_row(&stale_row);
    adapter
        .append_row(Tab::Ledger, &r2)
        .expect("seed a stale data row");
    adapter
        .append_row(Tab::Ledger, &r2)
        .expect("seed a stale data row");
    adapter
        .append_row(Tab::Ledger, &r2)
        .expect("seed a stale data row");

    // batch_update with a SINGLE row must tail-truncate the residual 2 stale rows.
    let fresh = serde_rows::ledger_to_row(&ledger_row("fresh-1", 2));
    adapter
        .batch_update(Tab::Ledger, std::slice::from_ref(&fresh))
        .expect("batch_update");
    assert_eq!(
        adapter.api().update_count(),
        1,
        "batch_update rode the low-level update"
    );

    let back = adapter.read_rows(Tab::Ledger).expect("read back");
    assert_eq!(
        back.len(),
        1,
        "tail-truncate: the residual stale rows are gone"
    );
    assert_eq!(
        back[0].get("EventId"),
        "fresh-1",
        "the full-tab content is exactly the new row"
    );

    // clear removes all data rows (the data range from row 2 down).
    adapter.clear(Tab::Ledger).expect("clear");
    assert_eq!(
        adapter.api().clear_count(),
        1,
        "clear rode the low-level clear"
    );
    let back = adapter
        .read_rows(Tab::Ledger)
        .expect("read back after clear");
    assert!(back.is_empty(), "clear leaves no data rows");
}

// @spec RUNTIME-SHEETS-002
#[test]
fn a_transport_failure_on_the_primitive_returns_control_to_the_segment() {
    // When the ONE primitive is offline, the segment adapter maps it to its own
    // error contract (store: Unreachable → return control to the owner), rather
    // than each segment inventing its own transport handling. (RUNTIME-SHEETS-002)
    let fake = FakeSheetsApi::new();
    fake.set_unreachable(true);
    let adapter = StoreSheetsAdapter::new(fake);

    match adapter.read_rows(Tab::Ledger) {
        Err(store::StoreError::Unreachable) => {}
        other => panic!("offline primitive must surface as Unreachable, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// A Ledger `Buy` event with the given id/seq, for seeding adapter rows.
fn ledger_row(id: &str, seq: u64) -> LedgerEvent {
    LedgerEvent {
        id: id.to_string(),
        seq: Seq(seq),
        date: Date(19_000),
        kind: LedgerEventKind::Buy {
            lot_id: format!("lot-{id}"),
            symbol: "AMZN".to_string(),
            qty: MicroShares(1_000_000),
            unit_price_cents: Cents(150_00),
            fees_cents: Cents(0),
            platform: "schwab".to_string(),
            tracking_code: None,
        },
    }
}

fn last_col(tab: Tab) -> String {
    // Mirror the adapter's column-letter math for the seeded header range.
    let mut n = tab.header().len().max(1);
    let mut s = String::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        s.insert(0, (b'A' + rem as u8) as char);
        n = (n - 1) / 26;
    }
    s
}
