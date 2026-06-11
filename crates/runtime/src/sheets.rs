//! The single low-level Sheets-access layer (RUNTIME-SHEETS).
//!
//! The ONE place Google Sheets API mechanics live: service-account JWT
//! authentication (RUNTIME-SHEETS-001), reads, appends, `batchUpdate`, clears, and
//! **rate-limit / backoff / retry** (Deferred-2 in `runtime-design.md`). Every
//! workbook touch goes through it: `store`'s event-log append/read, `config`'s
//! domain tabs, `sheets-view`'s view-tab republish + marks read-back, and
//! `reports`' History. Each segment keeps its OWN write discipline (append +
//! read-back-verify; full-tab `batchUpdate` with tail-truncate; History last-wins)
//! but builds it on this one primitive (RUNTIME-SHEETS-002), so auth, throttling,
//! and the single-writer serialization point are not re-implemented per segment.
//!
//! The primitive is the raw **cell-grid** surface — [`SheetsApi`] — over A1
//! ranges. The segment-shaped traits (`store::SheetsClient`,
//! `sheets_view::SheetsViewClient`, `reports::HistoryClient`, `config::ConfigStore`)
//! are *adapters* over it; [`StoreSheetsAdapter`] is the worked example (RUNTIME-
//! SHEETS-002): no segment opens its own Sheets client.
//!
//! Tests exercise the lock, the cycle, and every segment adapter against the
//! in-memory [`crate::testkit::FakeSheetsApi`] (no network); the retry/backoff loop
//! is unit-tested via [`run_with_retry`] over a programmed status sequence. Only the
//! live HTTP round-trip of [`GoogleSheetsApi`] against a real workbook (the thin
//! `reqwest` send/parse wiring `with_retry` governs) is confirmed manually.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::auth::{ServiceAccount, TokenCache};

/// Epoch-seconds now (the wall clock; the real client path only). Used for the
/// per-request / per-retry token-freshness re-evaluation. (RUNTIME-SHEETS-004/005)
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A failure crossing the low-level Sheets-access seam: auth, transport (after
/// the backoff budget is exhausted), or an API error. The segment-shaped adapters
/// map this to their own error enum (`StoreError::Unreachable`, etc.).
/// (RUNTIME-SHEETS-001)
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SheetsError {
    /// The service-account credentials are missing / unparseable / the JWT could
    /// not be minted or exchanged for an access token. (RUNTIME-SHEETS-001)
    Auth(String),
    /// The workbook was unreachable after the retry/backoff budget was exhausted
    /// (a 5xx / network error that did not clear). (RUNTIME-SHEETS-001, Deferred-2)
    Unreachable(String),
    /// The Sheets API returned a non-retryable error (4xx other than 429): a bad
    /// range, a missing tab, a permission error. Carries the status + message.
    Api { status: u16, message: String },
}

impl std::fmt::Display for SheetsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for SheetsError {}

/// One cell value in a grid read/write — the string form a Sheets cell carries
/// (the segment serde already produces strings: `store::Row` cells,
/// `sheets_view::Cell` text). The low-level layer is content-agnostic; typing
/// (value vs formula) is the *caller's* write discipline.
pub type CellValue = String;

/// A rectangular block of cells (rows of cell strings) as the Sheets `values` API
/// shapes them. The low-level primitive's read/write currency.
pub type Grid = Vec<Vec<CellValue>>;

/// The retry / backoff policy for the rate-limited Sheets API (the policy `store`
/// deferred, now landed here — Deferred-2). Exponential backoff with a capped
/// number of attempts; a `429`/`5xx` is retried, a `4xx` is not. (RUNTIME-SHEETS-001)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BackoffPolicy {
    /// Maximum number of attempts (the initial try plus retries).
    pub max_attempts: u32,
    /// The base backoff delay. Attempt 1 waits nothing; attempt `n ≥ 2` waits
    /// `base * 2^(n-2)`, capped at `max_delay` (see [`BackoffPolicy::delay_for`]).
    pub base_delay: Duration,
    /// The cap on any single backoff delay.
    pub max_delay: Duration,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        // Patient enough to ride out a full Sheets per-minute quota window: the
        // worst-case wait sums to ~95s (1+2+4+8+16+30+30 + jitterless), which a
        // bulk operation (the import's hundreds of throttled appends) needs —
        // the prior 5×500ms budget (~7.5s) died inside one 60s quota window.
        // Delays only occur on retryable failures; the success path is unwaited.
        BackoffPolicy {
            max_attempts: 8,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl BackoffPolicy {
    /// The backoff delay before attempt `attempt` (1-based): the first attempt
    /// waits nothing; attempt `n ≥ 2` waits `base * 2^(n-2)` (so attempt 2 = base,
    /// attempt 3 = 2·base, …), capped at `max_delay`. Pure so the schedule is
    /// unit-testable without sleeping. (RUNTIME-SHEETS-001, Deferred-2)
    pub fn delay_for(&self, attempt: u32) -> Duration {
        if attempt <= 1 {
            return Duration::ZERO;
        }
        let shift = (attempt - 2).min(20); // guard against overflow on huge attempts
        let scaled = self.base_delay.saturating_mul(1u32 << shift);
        scaled.min(self.max_delay)
    }

    /// Whether another attempt is allowed after `attempt` attempts have been made.
    pub fn may_retry(&self, attempts_made: u32) -> bool {
        attempts_made < self.max_attempts
    }
}

/// Whether an HTTP status from the Sheets API is retryable under the backoff
/// policy: `429` (rate-limit) and `5xx` (transient server error) are; everything
/// else is a terminal API error. (RUNTIME-SHEETS-001, Deferred-2)
pub fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

/// The low-level Sheets-access primitive (RUNTIME-SHEETS-001): the ONE client to
/// the workbook. Reads, appends, `batchUpdate`, clears — over A1 ranges, with
/// auth + rate-limit/backoff/retry handled by the implementation. The segment
/// traits are adapters over this; no segment opens its own client
/// (RUNTIME-SHEETS-002).
pub trait SheetsApi {
    /// Read a range's values (the Sheets `spreadsheets.values.get` API), as a grid
    /// of cell strings in row-major order.
    fn read_range(&self, range: &str) -> Result<Grid, SheetsError>;

    /// Append rows to the end of a range's table (the Sheets
    /// `spreadsheets.values.append` API, `INSERT_ROWS`). The single low-level
    /// append every segment's append discipline builds on.
    fn append_rows(&self, range: &str, rows: &Grid) -> Result<(), SheetsError>;

    /// Overwrite a range's values (the Sheets `spreadsheets.values.update` /
    /// `batchUpdate` API). The full-tab `batchUpdate` with tail-truncate that
    /// `sheets-view`'s republish and `reports`' last-wins upsert build on.
    fn update_range(&self, range: &str, rows: &Grid) -> Result<(), SheetsError>;

    /// Clear a range's values (the Sheets `spreadsheets.values.clear` API). Used by
    /// `store`'s cache rebuild / view-tab tail-truncate.
    fn clear_range(&self, range: &str) -> Result<(), SheetsError>;

    /// Idempotently ensure a sheet (tab) named `title` exists — the `addSheet`
    /// batch request, a no-op when present. The bootstrap primitive a fresh
    /// workbook needs: `store`'s adapter creates a missing event-log tab through
    /// this before the first append (STORE-WRITE-009).
    fn ensure_sheet(&self, title: &str) -> Result<(), SheetsError>;

    /// Append rows with VERBATIM cell semantics (`valueInputOption=RAW`) — no
    /// USER_ENTERED interpretation. The event-log trust seam requires it: a
    /// `LotRefs` cell like `3:1000000` is a duration to USER_ENTERED (stored as
    /// `16669:40:00`), silently breaking the row↔event identity the drift guard
    /// proves (STORE-GUARD-002); read-back-verify catches the coercion but the
    /// write must simply not coerce. View tabs keep USER_ENTERED (their formulas
    /// need it); event rows ride RAW.
    fn append_rows_raw(&self, range: &str, rows: &Grid) -> Result<(), SheetsError>;
}

// ===========================================================================
// The real Google Sheets API client (RUNTIME-SHEETS-001). Blocking HTTP over the
// Sheets REST API, a service-account JWT exchanged for an access token, JSON, and
// the retry/backoff loop. Its control logic (the retry loop, the auth JWT mint) is
// unit-tested via `run_with_retry` and `ServiceAccount::mint_assertion`; only the
// live HTTP round-trip is confirmed manually against a real workbook.
// ===========================================================================

/// The real low-level Sheets client over the Google Sheets REST API: a blocking
/// `reqwest` client, the service-account JWT auth (`config` credentials), and the
/// rate-limit / backoff / retry loop. The ONE place Sheets mechanics live.
/// (RUNTIME-SHEETS-001)
///
/// Its retry loop is unit-tested via [`run_with_retry`] and its auth JWT mint via
/// [`crate::auth::ServiceAccount::mint_assertion`]; only the live HTTP round-trip is
/// confirmed manually against a real workbook (the unit suite uses the fake).
pub struct GoogleSheetsApi {
    /// The target spreadsheet id (the workbook).
    spreadsheet_id: String,
    /// The service-account, which mints + caches the OAuth access token.
    account: ServiceAccount,
    /// The blocking HTTP client.
    http: reqwest::blocking::Client,
    /// The retry/backoff policy (Deferred-2). (RUNTIME-SHEETS-001)
    backoff: BackoffPolicy,
    /// The coalesced access-token cache: lazy mint, reuse-until-margin, and a
    /// single in-flight re-mint under a retry storm (no thundering-herd re-auth).
    /// Interior-mutable (its own `Mutex`) so the read/append/update/clear methods
    /// take `&self` (the trait shape). (RUNTIME-SHEETS-003/004/005)
    token: TokenCache,
}

impl GoogleSheetsApi {
    /// Construct the real client for `spreadsheet_id`, authenticating with the
    /// service-account loaded from `credentials_path` (the standard Google
    /// service-account JSON). (RUNTIME-SHEETS-001)
    pub fn from_credentials_file(
        spreadsheet_id: impl Into<String>,
        credentials_path: &str,
    ) -> Result<Self, SheetsError> {
        let account = ServiceAccount::from_file(credentials_path)
            .map_err(|e| SheetsError::Auth(e.to_string()))?;
        Ok(GoogleSheetsApi {
            spreadsheet_id: spreadsheet_id.into(),
            account,
            http: reqwest::blocking::Client::builder()
                .build()
                .map_err(|e| SheetsError::Unreachable(e.to_string()))?,
            backoff: BackoffPolicy::default(),
            token: TokenCache::new(),
        })
    }

    /// Override the retry/backoff policy (Deferred-2). (RUNTIME-SHEETS-001)
    pub fn with_backoff(mut self, backoff: BackoffPolicy) -> Self {
        self.backoff = backoff;
        self
    }

    /// The target workbook id.
    pub fn spreadsheet_id(&self) -> &str {
        &self.spreadsheet_id
    }

    /// Obtain a valid bearer access token, minting + caching a fresh one when the
    /// cached token is absent or expired. The single auth site. (RUNTIME-SHEETS-001)
    ///
    /// The token is minted **lazily** on the first request that needs it, cached,
    /// and reused until expiry — a cached, unexpired token short-circuits the mint
    /// (RUNTIME-SHEETS-003). Freshness is re-evaluated on **every** call, so a token
    /// that lapses within its 60s margin mid-retry-storm is re-minted before the next
    /// attempt issues (RUNTIME-SHEETS-004/005); and the re-mint is **coalesced** by
    /// the [`TokenCache`], so a storm triggers at most one in-flight re-auth rather
    /// than a thundering herd (RUNTIME-SHEETS-005).
    // @spec RUNTIME-SHEETS-003, RUNTIME-SHEETS-005
    fn access_token(&self) -> Result<String, SheetsError> {
        self.token
            .fresh_bearer(now_secs(), || self.account.fetch_access_token(&self.http))
            .map_err(|e| SheetsError::Auth(e.to_string()))
    }

    /// The base `spreadsheets.values` URL for a range on this workbook.
    fn values_url(&self, range: &str) -> String {
        format!(
            "https://sheets.googleapis.com/v4/spreadsheets/{}/values/{}",
            self.spreadsheet_id,
            urlencode(range),
        )
    }

    /// Run a request closure with the retry/backoff loop: retry `429`/`5xx` with
    /// exponential backoff up to the policy's attempt cap, fail fast on a `4xx`.
    /// Delegates to the free [`run_with_retry`] (testable without network/sleeping)
    /// with a real `sleep`. (RUNTIME-SHEETS-001, Deferred-2)
    fn with_retry<T>(
        &self,
        attempt_fn: impl FnMut() -> Result<T, SheetsError>,
    ) -> Result<T, SheetsError> {
        run_with_retry(&self.backoff, attempt_fn, std::thread::sleep)
    }

    /// The `spreadsheets.batchUpdate` URL (workbook-metadata operations:
    /// add/delete sheet, tab-order/property updates — NOT a values-grid touch).
    fn batch_update_url(&self) -> String {
        format!(
            "https://sheets.googleapis.com/v4/spreadsheets/{}:batchUpdate",
            self.spreadsheet_id
        )
    }

    /// Fetch the workbook's sheet titles -> `sheetId` map (the `sheets.properties`
    /// metadata field). The single place tab existence / id is resolved for the
    /// add/delete sheet management ops. (RUNTIME-SHEETS-001)
    pub fn sheet_ids(&self) -> Result<std::collections::BTreeMap<String, i64>, SheetsError> {
        let url = format!(
            "https://sheets.googleapis.com/v4/spreadsheets/{}?fields=sheets.properties.title,sheets.properties.sheetId",
            self.spreadsheet_id
        );
        self.with_retry(|| {
            // Re-evaluate token freshness on EACH attempt: a long retry storm can
            // outlast the token's 60s margin, so re-mint (coalesced) before issuing.
            // (RUNTIME-SHEETS-005)
            let token = self.access_token()?;
            let resp = self
                .http
                .get(&url)
                .bearer_auth(&token)
                .send()
                .map_err(|e| SheetsError::Unreachable(e.to_string()))?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(SheetsError::Api { status, message: resp.text().unwrap_or_default() });
            }
            let body: SpreadsheetMeta = resp
                .json()
                .map_err(|e| SheetsError::Api { status, message: e.to_string() })?;
            let mut out = std::collections::BTreeMap::new();
            for s in body.sheets.unwrap_or_default() {
                if let Some(p) = s.properties {
                    out.insert(p.title, p.sheet_id);
                }
            }
            Ok(out)
        })
    }

    /// Idempotently ensure a tab named `title` exists, returning its `sheetId`. A
    /// no-op (returning the existing id) when the tab is already present — so a
    /// re-run is safe. The dedicated-test-tab creation the e2e round-trip needs;
    /// the real workbook republish uses the existing view tabs. (RUNTIME-SHEETS-001)
    pub fn ensure_sheet(&self, title: &str) -> Result<i64, SheetsError> {
        if let Some(id) = self.sheet_ids()?.get(title) {
            return Ok(*id);
        }
        let url = self.batch_update_url();
        // 40 columns so the fingerprint block's AA1:AC1 metadata range sits
        // inside the grid (the Sheets default of 26 puts AA out of bounds and a
        // values call against it 400s). (STORE-CACHE-002)
        let body = serde_json::json!({
            "requests": [ { "addSheet": { "properties": {
                "title": title,
                "gridProperties": { "rowCount": 1000, "columnCount": 40 }
            } } } ]
        });
        self.with_retry(|| {
            let token = self.access_token()?; // per-attempt freshness (RUNTIME-SHEETS-005)
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .map_err(|e| SheetsError::Unreachable(e.to_string()))?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(SheetsError::Api { status, message: resp.text().unwrap_or_default() });
            }
            // Re-read the id rather than parse the reply shape (the title is now
            // present); a concurrent creator only makes this idempotent.
            Ok(())
        })?;
        self.sheet_ids()?
            .get(title)
            .copied()
            .ok_or_else(|| SheetsError::Api {
                status: 500,
                message: format!("sheet {title:?} not present after addSheet"),
            })
    }

    /// Idempotently delete the tab named `title` (the e2e cleanup, so a re-run is
    /// safe). A no-op when the tab is already absent. NEVER used against the real
    /// view/event tabs — only the dedicated test tabs the e2e owns.
    /// (RUNTIME-SHEETS-001)
    pub fn delete_sheet(&self, title: &str) -> Result<(), SheetsError> {
        let Some(&sheet_id) = self.sheet_ids()?.get(title) else {
            return Ok(()); // already absent — idempotent cleanup
        };
        let url = self.batch_update_url();
        let body = serde_json::json!({
            "requests": [ { "deleteSheet": { "sheetId": sheet_id } } ]
        });
        self.with_retry(|| {
            let token = self.access_token()?; // per-attempt freshness (RUNTIME-SHEETS-005)
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .map_err(|e| SheetsError::Unreachable(e.to_string()))?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(SheetsError::Api { status, message: resp.text().unwrap_or_default() });
            }
            Ok(())
        })
    }
}

/// The `spreadsheets.get` metadata response (only `sheets.properties` is requested).
#[derive(serde::Deserialize)]
struct SpreadsheetMeta {
    #[serde(default)]
    sheets: Option<Vec<SheetMeta>>,
}

#[derive(serde::Deserialize)]
struct SheetMeta {
    properties: Option<SheetProps>,
}

#[derive(serde::Deserialize)]
struct SheetProps {
    title: String,
    #[serde(rename = "sheetId", default)]
    sheet_id: i64,
}

/// The retry/backoff control loop, factored out of [`GoogleSheetsApi`] so it can be
/// unit-tested over a FAKE transport (a programmed status sequence) with NO network
/// and NO real sleeps — the loop's attempt count and terminal mapping are what
/// matters, not the HTTP plumbing (RUNTIME-SHEETS-001, Deferred-2):
///
/// - a retryable error (`429`/`5xx` API, or a transient transport `Unreachable`) is
///   retried with exponential backoff up to the policy's attempt cap;
/// - a non-retryable `Api{4xx}` or an `Auth` error **fails fast** (no retry);
/// - exhausting the attempt budget on a retryable error surfaces `Unreachable`.
///
/// `sleep_fn` is the backoff sleep (the real path passes `thread::sleep`; tests pass
/// a no-op that records the delays), so the schedule is exercised without waiting.
pub fn run_with_retry<T>(
    policy: &BackoffPolicy,
    mut attempt_fn: impl FnMut() -> Result<T, SheetsError>,
    mut sleep_fn: impl FnMut(Duration),
) -> Result<T, SheetsError> {
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        match attempt_fn() {
            Ok(v) => return Ok(v),
            Err(SheetsError::Api { status, message }) if is_retryable_status(status) => {
                if !policy.may_retry(attempts) {
                    return Err(SheetsError::Unreachable(format!(
                        "exhausted {attempts} attempts (last status {status}): {message}"
                    )));
                }
                sleep_fn(policy.delay_for(attempts + 1));
            }
            Err(SheetsError::Unreachable(msg)) => {
                if !policy.may_retry(attempts) {
                    return Err(SheetsError::Unreachable(msg));
                }
                sleep_fn(policy.delay_for(attempts + 1));
            }
            // A non-retryable API error (4xx) or auth error fails fast.
            Err(other) => return Err(other),
        }
    }
}

impl SheetsApi for GoogleSheetsApi {
    fn read_range(&self, range: &str) -> Result<Grid, SheetsError> {
        let url = self.values_url(range);
        self.with_retry(|| {
            let token = self.access_token()?; // per-attempt freshness (RUNTIME-SHEETS-005)
            let resp = self
                .http
                .get(&url)
                .bearer_auth(&token)
                .send()
                .map_err(|e| SheetsError::Unreachable(e.to_string()))?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                let message = resp.text().unwrap_or_default();
                return Err(SheetsError::Api { status, message });
            }
            let body: ValuesResponse = resp
                .json()
                .map_err(|e| SheetsError::Api { status, message: e.to_string() })?;
            Ok(body.values.unwrap_or_default())
        })
    }

    fn append_rows(&self, range: &str, rows: &Grid) -> Result<(), SheetsError> {
        let url = format!(
            "{}:append?valueInputOption=USER_ENTERED&insertDataOption=INSERT_ROWS",
            self.values_url(range)
        );
        let body = ValuesBody { range: range.to_string(), values: rows.clone() };
        self.with_retry(|| {
            let token = self.access_token()?; // per-attempt freshness (RUNTIME-SHEETS-005)
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .map_err(|e| SheetsError::Unreachable(e.to_string()))?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(SheetsError::Api { status, message: resp.text().unwrap_or_default() });
            }
            Ok(())
        })
    }

    fn update_range(&self, range: &str, rows: &Grid) -> Result<(), SheetsError> {
        let url = format!("{}?valueInputOption=USER_ENTERED", self.values_url(range));
        let body = ValuesBody { range: range.to_string(), values: rows.clone() };
        self.with_retry(|| {
            let token = self.access_token()?; // per-attempt freshness (RUNTIME-SHEETS-005)
            let resp = self
                .http
                .put(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .map_err(|e| SheetsError::Unreachable(e.to_string()))?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(SheetsError::Api { status, message: resp.text().unwrap_or_default() });
            }
            Ok(())
        })
    }

    fn clear_range(&self, range: &str) -> Result<(), SheetsError> {
        let url = format!("{}:clear", self.values_url(range));
        self.with_retry(|| {
            let token = self.access_token()?; // per-attempt freshness (RUNTIME-SHEETS-005)
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&token)
                .send()
                .map_err(|e| SheetsError::Unreachable(e.to_string()))?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(SheetsError::Api { status, message: resp.text().unwrap_or_default() });
            }
            Ok(())
        })
    }

    fn ensure_sheet(&self, title: &str) -> Result<(), SheetsError> {
        // The inherent method does the addSheet-if-absent work (and returns the
        // sheetId, which the trait seam does not need).
        GoogleSheetsApi::ensure_sheet(self, title).map(|_| ())
    }

    fn append_rows_raw(&self, range: &str, rows: &Grid) -> Result<(), SheetsError> {
        let url = format!(
            "{}:append?valueInputOption=RAW&insertDataOption=INSERT_ROWS",
            self.values_url(range)
        );
        let body = ValuesBody { range: range.to_string(), values: rows.clone() };
        self.with_retry(|| {
            let token = self.access_token()?; // per-attempt freshness (RUNTIME-SHEETS-005)
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .map_err(|e| SheetsError::Unreachable(e.to_string()))?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(SheetsError::Api { status, message: resp.text().unwrap_or_default() });
            }
            Ok(())
        })
    }
}

/// The Sheets `values.get` response shape.
#[derive(serde::Deserialize)]
struct ValuesResponse {
    #[serde(default)]
    values: Option<Grid>,
}

/// The Sheets `values` request body for append/update.
#[derive(serde::Serialize)]
struct ValuesBody {
    range: String,
    values: Grid,
}

/// Minimal percent-encoding of an A1 range for the URL path segment (the tab name
/// may contain spaces, e.g. `Ledger Events!A2:Z`). Encodes the characters that
/// matter for a Sheets `values` path; the alphanumerics and `!:$._-~` stay.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'!' | b':' | b'$' | b'.' | b'_' | b'-'
            | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ===========================================================================
// RUNTIME-SHEETS-002: a segment's write discipline is built on this ONE
// primitive; no segment opens its own client. `StoreSheetsAdapter` is the worked
// example — it implements `store::SheetsClient` over a `SheetsApi`, so store's
// append + read-back-verify rides the runtime layer's auth/throttling rather than
// re-opening Sheets. (sheets-view / reports / config adapters follow the same
// shape; this one demonstrates the seam the design names.)
// ===========================================================================

/// Adapts a low-level [`SheetsApi`] to `store`'s [`store::SheetsClient`], so
/// `store`'s append + read-back-verify write discipline is built on the ONE
/// runtime Sheets primitive — not a second, separately-authenticated client
/// (RUNTIME-SHEETS-002). The A1 range for each tab is derived from the tab name +
/// its typed header; the cell-grid <-> `store::Row` mapping uses the tab's header
/// order (the same projection `store::Row::to_cells` / `from_cells` define).
pub struct StoreSheetsAdapter<A: SheetsApi> {
    api: A,
}

impl<A: SheetsApi> StoreSheetsAdapter<A> {
    /// Build the adapter over a low-level Sheets API client. (RUNTIME-SHEETS-002)
    pub fn new(api: A) -> Self {
        StoreSheetsAdapter { api }
    }

    /// Borrow the underlying low-level API (diagnostics / tests).
    pub fn api(&self) -> &A {
        &self.api
    }

    /// The A1 data range for a tab (data rows beneath the header row): e.g.
    /// `'Ledger Events'!A2:Z`. The column span covers the tab's typed header width.
    fn data_range(tab: store::Tab) -> String {
        let cols = tab.header().len();
        let last = col_letter(cols);
        format!("'{}'!A2:{}", tab.name(), last)
    }

    /// The A1 range covering the header + data (the whole table), for a read.
    fn full_range(tab: store::Tab) -> String {
        let cols = tab.header().len();
        let last = col_letter(cols);
        format!("'{}'!A1:{}", tab.name(), last)
    }
}

impl<A: SheetsApi> store::SheetsClient for StoreSheetsAdapter<A> {
    fn read_rows(&self, tab: store::Tab) -> Result<Vec<store::Row>, store::StoreError> {
        let grid = self
            .api
            .read_range(&Self::full_range(tab))
            .map_err(map_store_err)?;
        // Row 0 is the header; data rows follow. Project each cell vector back to a
        // `store::Row` using the tab's header order (store::Row::from_cells).
        let rows = grid
            .into_iter()
            .skip(1)
            .map(|cells| store::Row::from_cells(tab, &cells))
            .collect();
        Ok(rows)
    }

    fn append_row(&mut self, tab: store::Tab, row: &store::Row) -> Result<(), store::StoreError> {
        // RAW append: event cells are VERBATIM (the trust seam's row↔event
        // identity, STORE-GUARD-002). USER_ENTERED coerces `lot:qty` LotRefs
        // into durations.
        let cells = row.to_cells(tab);
        self.api
            .append_rows_raw(&Self::data_range(tab), &vec![cells])
            .map_err(map_store_err)
    }

    fn read_probe(&self, tab: store::Tab) -> Result<store::sheets::ProbeCells, store::StoreError> {
        // The cheap fingerprint cells are a dedicated metadata range the real
        // workbook computes as formulas (COUNTA / MAX(Seq) / SUMPRODUCT checksum)
        // that auto-extend on append. They live in a fixed metadata range next to
        // the tab; reading it is one low-level call. (STORE-CACHE-002)
        let range = format!("'{}'!AA1:AC1", tab.name());
        let grid = self.api.read_range(&range).map_err(map_store_err)?;
        let cells = grid.into_iter().next().unwrap_or_default();
        let parse = |i: usize| cells.get(i).and_then(|s| s.trim().parse::<i64>().ok());
        Ok(store::sheets::ProbeCells {
            count: parse(0),
            max_seq: parse(1),
            checksum: parse(2),
        })
    }

    fn batch_update(&mut self, tab: store::Tab, rows: &[store::Row]) -> Result<(), store::StoreError> {
        let grid: Grid = rows.iter().map(|r| r.to_cells(tab)).collect();
        self.api
            .update_range(&Self::data_range(tab), &grid)
            .map_err(map_store_err)
    }

    fn clear(&mut self, tab: store::Tab) -> Result<(), store::StoreError> {
        self.api
            .clear_range(&Self::data_range(tab))
            .map_err(map_store_err)
    }

    fn ensure_tab(&mut self, tab: store::Tab) -> Result<(), store::StoreError> {
        // Create the sheet (a no-op when present; 40 columns so the fingerprint
        // metadata range exists), then seed the schema. (STORE-WRITE-009)
        self.api.ensure_sheet(tab.name()).map_err(map_store_err)?;
        let name = tab.name();

        // Fingerprint block first, header second: the cells are independent on
        // the real grid, but the fake's truncate-style update keeps the LAST
        // write — which must be the header row the data reads skip.
        // The three formula cells auto-extend over the data rows: COUNTA,
        // MAX(Seq) (Seq is column A), and a coarse change-sensitive checksum
        // (Seq-sum + weighted COUNTA over the full data range) — edits outside
        // its reach fall to the content hash. (STORE-CACHE-002/003)
        let last = col_letter(tab.header().len());
        let fp = vec![vec![
            format!("=COUNTA('{name}'!A2:A)"),
            format!("=IFERROR(MAX('{name}'!A2:A),0)"),
            format!("=IFERROR(SUM('{name}'!A2:A)+31*COUNTA('{name}'!A2:{last}),0)"),
        ]];
        self.api
            .update_range(&format!("'{name}'!AA1:AC1"), &fp)
            .map_err(map_store_err)?;

        // The frozen header row.
        let header: Vec<String> = tab.header().iter().map(|h| h.to_string()).collect();
        self.api
            .update_range(&format!("'{name}'!A1"), &vec![header])
            .map_err(map_store_err)
    }
}

/// Map the low-level [`SheetsError`] to `store`'s error contract. A values call
/// against a tab that does not exist — the Sheets API's non-retryable 400
/// `Unable to parse range` — is `TabMissing` (the fresh-workbook cold start,
/// STORE-LOAD-007); any other transport / API failure is `Unreachable` (return
/// control to the owner, leave no partial state — STORE-WRITE-005).
/// (RUNTIME-SHEETS-002)
///
/// NOTE: beyond the missing-tab classification, store's error contract is a
/// coarse surface, so this **collapses the terminal-vs-transient distinction**
/// the [`SheetsError`] layer preserves: an `Auth(_)` or another non-retryable
/// `Api{4xx}` (e.g. a 403 permission error on a misconfigured share) maps to the
/// same `Unreachable` as a transient transport failure. A caller that blindly
/// retries on `Unreachable` would loop forever on such a permanent error. This
/// is faithful to store's seam (the distinction has nowhere to go); the
/// permanent error is preserved one layer down in `SheetsError` for any caller
/// that drives `GoogleSheetsApi` directly. If store later adds a
/// terminal/forbidden variant, map `Auth`/`Api{4xx}` to it here.
/// (RUNTIME-SHEETS-002)
fn map_store_err(e: SheetsError) -> store::StoreError {
    if is_missing_tab(&e) {
        store::StoreError::TabMissing
    } else {
        store::StoreError::Unreachable
    }
}

/// Whether a [`SheetsError`] is the Sheets API's "this tab does not exist"
/// failure — the non-retryable 400 whose message carries `Unable to parse
/// range`. The classification every adapter's fresh-workbook cold start /
/// bootstrap-on-first-write rides (STORE-LOAD-007 / STORE-WRITE-009 and the
/// view/History publish-implies-create readings of SHEET-TAB-001 /
/// REPORT-HIST-001).
pub(crate) fn is_missing_tab(e: &SheetsError) -> bool {
    matches!(e, SheetsError::Api { status: 400, message } if message.contains("Unable to parse range"))
}

/// The 1-based column count -> the A1 column letter of the LAST column (1 -> `A`,
/// 26 -> `Z`, 27 -> `AA`). Used to bound a tab's A1 range to its header width.
fn col_letter(count: usize) -> String {
    let mut n = count.max(1);
    let mut s = String::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        s.insert(0, (b'A' + rem as u8) as char);
        n = (n - 1) / 26;
    }
    s
}

#[cfg(test)]
mod col_tests {
    use super::col_letter;

    #[test]
    fn column_letters() {
        assert_eq!(col_letter(1), "A");
        assert_eq!(col_letter(26), "Z");
        assert_eq!(col_letter(27), "AA");
        assert_eq!(col_letter(52), "AZ");
    }
}
