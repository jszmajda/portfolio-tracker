//! Re-auth freshness during a retry storm: RUNTIME-SHEETS-005. Token freshness is
//! re-evaluated before each attempt (a long retry storm can outlast the token's 60s
//! margin), and concurrent refreshers are COALESCED so a storm triggers at most ONE
//! in-flight re-mint rather than a thundering herd of re-auth requests. Driven over
//! a counting mint closure (no network) and a fake clock.
#![allow(clippy::inconsistent_digit_grouping)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use runtime::auth::{AccessToken, AuthError, TokenCache};

/// A token whose margined expiry is `expires_at` epoch-seconds.
fn token(bearer: &str, expires_at: u64) -> AccessToken {
    AccessToken {
        bearer: bearer.to_string(),
        expires_at_secs: expires_at,
    }
}

// @spec RUNTIME-SHEETS-005, RUNTIME-SHEETS-003
#[test]
fn freshness_is_re_evaluated_per_call_and_a_fresh_token_short_circuits() {
    // A cached, unexpired token short-circuits the mint on every call (the per-call
    // freshness re-evaluation the retry loop rides). (RUNTIME-SHEETS-003/005)
    let cache = TokenCache::new();
    let mints = AtomicU64::new(0);

    // First call at t=1000 mints (the lazy first mint), token good until t=2000.
    let b1 = cache
        .fresh_bearer(1_000, || {
            mints.fetch_add(1, Ordering::SeqCst);
            Ok(token("fresh", 2_000))
        })
        .expect("first mint");
    assert_eq!(b1, "fresh");
    assert_eq!(mints.load(Ordering::SeqCst), 1, "the lazy first mint ran");

    // A second call still WITHIN the margined expiry reuses the cached token — no
    // re-mint (freshness re-evaluated, found fresh). The mint closure would panic if
    // it ran, proving the short-circuit. (RUNTIME-SHEETS-003)
    let b2 = cache
        .fresh_bearer(1_500, || {
            mints.fetch_add(1, Ordering::SeqCst);
            Ok(token("should-not-mint", 9_999))
        })
        .expect("reuse");
    assert_eq!(
        b2, "fresh",
        "the cached fresh token is reused, not re-minted"
    );
    assert_eq!(
        mints.load(Ordering::SeqCst),
        1,
        "a fresh token short-circuits the mint"
    );
}

// @spec RUNTIME-SHEETS-005, RUNTIME-SHEETS-004
#[test]
fn a_token_lapsing_mid_storm_is_re_minted_before_the_next_attempt() {
    // A token within its 60s margin (expired as of `now`) is re-minted on the next
    // call — the per-retry freshness re-check covers a token outliving a long retry
    // storm. (RUNTIME-SHEETS-004/005)
    let cache = TokenCache::new();
    let mints = AtomicU64::new(0);

    // Mint at t=1000, good until t=2000.
    cache
        .fresh_bearer(1_000, || {
            mints.fetch_add(1, Ordering::SeqCst);
            Ok(token("first", 2_000))
        })
        .expect("first mint");
    assert_eq!(mints.load(Ordering::SeqCst), 1);

    // Now t has advanced past the margined expiry (a long retry storm). The next
    // attempt re-evaluates freshness, finds it lapsed, and re-mints. (RUNTIME-SHEETS-005)
    let bearer = cache
        .fresh_bearer(2_500, || {
            mints.fetch_add(1, Ordering::SeqCst);
            Ok(token("second", 3_500))
        })
        .expect("re-mint after lapse");
    assert_eq!(
        bearer, "second",
        "the lapsed token was re-minted before the attempt"
    );
    assert_eq!(
        mints.load(Ordering::SeqCst),
        2,
        "exactly one re-mint on lapse"
    );
}

// @spec RUNTIME-SHEETS-005
#[test]
fn concurrent_refreshers_are_coalesced_to_a_single_in_flight_re_mint() {
    // The thundering-herd guard: N threads all observe a needed re-mint at once
    // (cold cache), but exactly ONE in-flight re-mint runs and the rest reuse its
    // result — never N re-auth requests. The mint closure blocks on a barrier so all
    // refreshers genuinely race, then counts how many actually minted.
    // (RUNTIME-SHEETS-005)
    const N: usize = 16;
    let cache = Arc::new(TokenCache::new());
    let mints = Arc::new(AtomicU64::new(0));
    // A barrier the FIRST minter waits on, released once all N threads are lined up,
    // so the single refresher holds the in-flight gate while the others arrive and
    // coalesce onto it rather than each starting their own mint.
    let lineup = Arc::new(Barrier::new(N));

    let mut handles = Vec::new();
    for _ in 0..N {
        let cache = Arc::clone(&cache);
        let mints = Arc::clone(&mints);
        let lineup = Arc::clone(&lineup);
        handles.push(std::thread::spawn(move || {
            // Line every thread up so they all hit fresh_bearer on a cold cache at
            // once — maximizing the chance to expose a thundering-herd re-auth.
            lineup.wait();
            cache
                .fresh_bearer(1_000, || {
                    // The single in-flight re-mint. A tiny sleep widens the window in
                    // which the other refreshers would (wrongly) each start their own.
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    mints.fetch_add(1, Ordering::SeqCst);
                    Ok(token("coalesced", 9_999))
                })
                .expect("fresh bearer")
        }));
    }

    let bearers: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    // Every caller got the SAME coalesced token.
    assert!(
        bearers.iter().all(|b| b == "coalesced"),
        "all callers reuse the one minted token"
    );
    // And exactly ONE re-mint ran — no thundering herd. (RUNTIME-SHEETS-005)
    assert_eq!(
        mints.load(Ordering::SeqCst),
        1,
        "a retry storm triggers at most one in-flight re-mint"
    );
}

// @spec RUNTIME-SHEETS-005
#[test]
fn a_mint_failure_is_surfaced_and_clears_the_in_flight_gate_for_a_retry() {
    // A failed re-mint surfaces to the caller and clears the in-flight gate, so a
    // subsequent attempt (the next retry) can try again rather than waiting forever
    // on a refresher that already gave up. (RUNTIME-SHEETS-005)
    let cache = TokenCache::new();

    // First attempt: the mint fails.
    let err = cache.fresh_bearer(1_000, || Err(AuthError::TokenExchangeFailed("boom".into())));
    assert!(
        matches!(err, Err(AuthError::TokenExchangeFailed(_))),
        "a mint failure surfaces"
    );

    // The in-flight gate was cleared: a subsequent attempt can mint successfully.
    let ok = cache
        .fresh_bearer(1_000, || Ok(token("recovered", 9_999)))
        .expect("retry mints");
    assert_eq!(
        ok, "recovered",
        "the next retry can re-mint (the gate was cleared)"
    );
    assert_eq!(cache.cached_bearer(1_000), Some("recovered".to_string()));
}
