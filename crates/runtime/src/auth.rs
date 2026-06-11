//! Service-account JWT authentication for the real Sheets-access layer
//! (RUNTIME-SHEETS-001).
//!
//! Loads the standard Google service-account JSON (`client_email`, `private_key`,
//! `token_uri`, …), mints a signed JWT bearer assertion, and exchanges it at the
//! OAuth token endpoint for a short-lived access token scoped to the Sheets API.
//! This is the single auth site the low-level [`crate::sheets::GoogleSheetsApi`]
//! calls. The JWT assertion mint ([`ServiceAccount::mint_assertion`]) is unit-tested
//! with a throwaway RSA key; only the live token-endpoint exchange
//! ([`ServiceAccount::fetch_access_token`]) is confirmed manually (the lock + cycle
//! unit tests use the fake client and never authenticate).

use std::sync::{Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

/// The Sheets API OAuth scope (read/write the spreadsheets the service account is
/// shared on). (RUNTIME-SHEETS-001)
pub const SHEETS_SCOPE: &str = "https://www.googleapis.com/auth/spreadsheets";

/// An auth failure: a missing / unparseable credentials file, an unsigned JWT, or
/// a rejected token exchange. (RUNTIME-SHEETS-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AuthError {
    /// The credentials file was missing or could not be read.
    CredentialsUnreadable(String),
    /// The credentials JSON was malformed (missing a required field, bad key PEM).
    CredentialsMalformed(String),
    /// The JWT could not be signed (a bad RSA key).
    SigningFailed(String),
    /// The OAuth token endpoint rejected the assertion or was unreachable.
    TokenExchangeFailed(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for AuthError {}

/// The fields of the standard Google service-account JSON the auth flow uses.
/// (RUNTIME-SHEETS-001)
#[derive(Clone, Debug, Deserialize)]
pub struct ServiceAccountKey {
    pub client_email: String,
    pub private_key: String,
    #[serde(default = "default_token_uri")]
    pub token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

/// A short-lived OAuth access token with its expiry, cached by the Sheets client
/// and refreshed when expired. (RUNTIME-SHEETS-001)
#[derive(Clone, Debug)]
pub struct AccessToken {
    /// The bearer token value.
    pub bearer: String,
    /// Epoch-seconds at which the token expires (with a safety margin already
    /// applied).
    pub expires_at_secs: u64,
}

impl AccessToken {
    /// Whether the token has expired as of the wall clock (refresh needed).
    pub fn is_expired(&self) -> bool {
        self.is_expired_at(now_secs())
    }

    /// Whether the token has expired as of `now` epoch-seconds (the 60s safety
    /// margin is already baked into `expires_at_secs`, so this flips at the
    /// pre-margined boundary). Pure, so the per-retry freshness re-evaluation
    /// (`RUNTIME-SHEETS-005`) is unit-testable without the wall clock.
    // @spec RUNTIME-SHEETS-004
    pub fn is_expired_at(&self, now: u64) -> bool {
        now >= self.expires_at_secs
    }
}

/// A coalesced OAuth-token cache (RUNTIME-SHEETS-005): the single in-process holder
/// of the cached [`AccessToken`], guarding the **re-mint** so that during a
/// backoff/retry storm — when many retry attempts (or concurrent callers) all
/// observe the token is within its 60s expiry margin at once — exactly **one**
/// in-flight re-mint runs and the rest wait for and reuse its result, rather than a
/// thundering herd of re-auth requests.
///
/// The freshness re-evaluation runs on **every** [`TokenCache::fresh_bearer`] call
/// (each retry attempt re-checks), so a token that lapses mid-storm is refreshed
/// before the next attempt issues (`RUNTIME-SHEETS-005`/`004`). Coalescing is
/// implemented with a `Mutex` + `Condvar`: a refresher marks an in-flight re-mint,
/// drops the lock to mint (so it does not hold the lock across the network round
/// trip), and on completion stores the token and notifies the waiters; concurrent
/// callers that find a re-mint already in flight wait on the condvar and then reuse
/// the freshly-stored token instead of minting their own.
pub struct TokenCache {
    state: Mutex<TokenState>,
    /// Signaled when an in-flight re-mint completes (success or failure), so waiters
    /// wake and reuse the result rather than starting their own.
    refreshed: Condvar,
}

#[derive(Default)]
struct TokenState {
    /// The cached token (absent before the first mint).
    token: Option<AccessToken>,
    /// `true` while exactly one caller is minting a fresh token — the coalescing
    /// gate. Concurrent callers wait on `refreshed` rather than minting too.
    refreshing: bool,
}

impl Default for TokenCache {
    fn default() -> Self {
        TokenCache::new()
    }
}

impl TokenCache {
    /// An empty cache (no token minted yet — the lazy first mint happens on the
    /// first `fresh_bearer`). (RUNTIME-SHEETS-003/005)
    pub fn new() -> Self {
        TokenCache {
            state: Mutex::new(TokenState::default()),
            refreshed: Condvar::new(),
        }
    }

    /// The cached bearer if present and not expired as of `now`, without minting —
    /// for diagnostics / tests.
    pub fn cached_bearer(&self, now: u64) -> Option<String> {
        let state = self.state.lock().unwrap();
        state
            .token
            .as_ref()
            .filter(|t| !t.is_expired_at(now))
            .map(|t| t.bearer.clone())
    }

    /// Obtain a fresh bearer token as of `now` (RUNTIME-SHEETS-003/004/005):
    ///
    /// 1. **Re-evaluate freshness** (every call, so each retry attempt re-checks): a
    ///    cached token still within its margined expiry short-circuits the mint.
    /// 2. **Coalesce**: if no fresh token but another caller is already minting,
    ///    WAIT for it and reuse its result — no second re-auth (the storm triggers at
    ///    most one in-flight re-mint).
    /// 3. Otherwise mark the in-flight re-mint, drop the lock, call `mint_fn` (the
    ///    network round trip — kept OUTSIDE the lock so it does not serialize other
    ///    work), store the result, and notify waiters.
    ///
    /// `mint_fn` is the actual token-endpoint exchange (the real path passes a
    /// closure over [`ServiceAccount::fetch_access_token`]); tests pass a closure
    /// that counts calls, so the coalescing is exercised without the network.
    pub fn fresh_bearer(
        &self,
        now: u64,
        mint_fn: impl FnOnce() -> Result<AccessToken, AuthError>,
    ) -> Result<String, AuthError> {
        let mut state = self.state.lock().unwrap();
        loop {
            // 1. Re-evaluate freshness on every call (per-retry re-check). A cached,
            //    unexpired token short-circuits — no mint. (RUNTIME-SHEETS-003/004)
            if let Some(tok) = state.token.as_ref() {
                if !tok.is_expired_at(now) {
                    return Ok(tok.bearer.clone());
                }
            }
            // 2. Coalesce: another caller is already minting — wait for it and reuse.
            //    The storm triggers at most ONE in-flight re-mint. (RUNTIME-SHEETS-005)
            if state.refreshing {
                state = self.refreshed.wait(state).unwrap();
                continue; // re-check freshness against whatever the refresher stored.
            }
            // 3. We are the single refresher. Mark in-flight and mint OUTSIDE the lock.
            state.refreshing = true;
            drop(state);

            let minted = mint_fn();

            let mut state = self.state.lock().unwrap();
            state.refreshing = false;
            // Wake every waiter so they reuse the freshly-stored token (or re-decide
            // on a mint failure) rather than each starting a new re-mint.
            self.refreshed.notify_all();
            match minted {
                Ok(token) => {
                    let bearer = token.bearer.clone();
                    state.token = Some(token);
                    return Ok(bearer);
                }
                // A mint failure is surfaced to this caller; the in-flight flag is
                // cleared so a subsequent attempt (next retry) can try again.
                Err(e) => return Err(e),
            }
        }
    }
}

/// The service account: the loaded key, used to mint a signed JWT and exchange it
/// for an access token. The single auth identity for the workbook.
/// (RUNTIME-SHEETS-001)
pub struct ServiceAccount {
    key: ServiceAccountKey,
}

impl std::fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redact the private key — never print credentials.
        f.debug_struct("ServiceAccount")
            .field("client_email", &self.key.client_email)
            .field("token_uri", &self.key.token_uri)
            .field("private_key", &"<redacted>")
            .finish()
    }
}

impl ServiceAccount {
    /// Load a service account from the standard Google service-account JSON at
    /// `path`. (RUNTIME-SHEETS-001)
    pub fn from_file(path: &str) -> Result<Self, AuthError> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| AuthError::CredentialsUnreadable(e.to_string()))?;
        Self::from_json(&contents)
    }

    /// Parse a service account from the credentials JSON string. (RUNTIME-SHEETS-001)
    pub fn from_json(json: &str) -> Result<Self, AuthError> {
        let key: ServiceAccountKey =
            serde_json::from_str(json).map_err(|e| AuthError::CredentialsMalformed(e.to_string()))?;
        Ok(ServiceAccount { key })
    }

    /// The service-account email (the identity the workbook must be shared with).
    pub fn client_email(&self) -> &str {
        &self.key.client_email
    }

    /// Mint a signed JWT assertion for the OAuth `jwt-bearer` grant: a 1-hour
    /// token scoped to the Sheets API, signed RS256 with the service-account RSA
    /// private key. (RUNTIME-SHEETS-001)
    pub fn mint_assertion(&self) -> Result<String, AuthError> {
        let iat = now_secs();
        let exp = iat + 3600;
        let claims = JwtClaims {
            iss: self.key.client_email.clone(),
            scope: SHEETS_SCOPE.to_string(),
            aud: self.key.token_uri.clone(),
            iat,
            exp,
        };
        let encoding_key = EncodingKey::from_rsa_pem(self.key.private_key.as_bytes())
            .map_err(|e| AuthError::CredentialsMalformed(e.to_string()))?;
        jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &encoding_key)
            .map_err(|e| AuthError::SigningFailed(e.to_string()))
    }

    /// Exchange a freshly-minted JWT assertion at the OAuth token endpoint for an
    /// access token (with a 60s safety margin on its expiry). (RUNTIME-SHEETS-001)
    ///
    /// The 60-second safety margin is baked into `expires_at_secs` here (stored
    /// expiry = real expiry − 60s), so [`AccessToken::is_expired`] treats the token
    /// as expired once `now + 60s ≥ real_expiry` and the client refreshes before
    /// issuing — a token never expires mid-flight in transit. (RUNTIME-SHEETS-004)
    // @spec RUNTIME-SHEETS-004
    pub fn fetch_access_token(
        &self,
        http: &reqwest::blocking::Client,
    ) -> Result<AccessToken, AuthError> {
        let assertion = self.mint_assertion()?;
        let form = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", assertion.as_str()),
        ];
        let resp = http
            .post(&self.key.token_uri)
            .form(&form)
            .send()
            .map_err(|e| AuthError::TokenExchangeFailed(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().unwrap_or_default();
            return Err(AuthError::TokenExchangeFailed(format!("status {status}: {body}")));
        }
        let body: TokenResponse = resp
            .json()
            .map_err(|e| AuthError::TokenExchangeFailed(e.to_string()))?;
        // Apply a 60s safety margin so a token never expires mid-request.
        let margin = 60;
        let ttl = body.expires_in.saturating_sub(margin);
        Ok(AccessToken {
            bearer: body.access_token,
            expires_at_secs: now_secs() + ttl,
        })
    }
}

/// The JWT claim set for the Google OAuth `jwt-bearer` grant. (RUNTIME-SHEETS-001)
#[derive(Serialize)]
struct JwtClaims {
    iss: String,
    scope: String,
    aud: String,
    iat: u64,
    exp: u64,
}

/// The OAuth token endpoint response.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
}

fn default_expires_in() -> u64 {
    3600
}

/// Epoch-seconds now (the wall clock; auth runs on the real path only).
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
