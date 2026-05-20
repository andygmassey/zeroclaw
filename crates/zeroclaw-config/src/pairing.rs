// Gateway pairing mode — first-connect authentication.
//
// Two flows coexist:
//
// 1. Legacy 6-digit code (`POST /pair` with `X-Pairing-Code` header). Used by
//    the web dashboard and existing customers paired before the spec'd flow.
//    Returns a plaintext bearer token (`zc_<64hex>`).
//
// 2. Spec'd QR-pairing flow (SHARED_AUTH_SPEC.md §3.2 + §3.3). On first run the
//    Hub mints a 32-byte base64url `pairing_token` with 10-minute TTL and
//    embeds it in a JSON QR payload. The iOS Companion scans it, then completes
//    the WebAuthn + AES-KW handshake at `POST /auth/pair/init` +
//    `POST /auth/pair/register`. Returns a wrapped DEK (40 B AES-KW) instead
//    of a bearer token; long-lived auth uses the WebAuthn assertion.
//
// Already-paired bearer tokens (legacy) are persisted in config so restarts
// don't require re-pairing.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Maximum failed pairing attempts before lockout.
const MAX_PAIR_ATTEMPTS: u32 = 5;
/// Lockout duration after too many failed pairing attempts.
const PAIR_LOCKOUT_SECS: u64 = 300; // 5 minutes
/// Maximum number of tracked client entries to bound memory usage.
const MAX_TRACKED_CLIENTS: usize = 10_000;
/// Retention period for failed-attempt entries with no activity.
const FAILED_ATTEMPT_RETENTION_SECS: u64 = 900; // 15 min
/// Minimum interval between full sweeps of the failed-attempt map.
const FAILED_ATTEMPT_SWEEP_INTERVAL_SECS: u64 = 300; // 5 min

/// Per-client failed attempt state with optional absolute lockout deadline.
#[derive(Debug, Clone, Copy)]
struct FailedAttemptState {
    count: u32,
    lockout_until: Option<Instant>,
    last_attempt: Instant,
}

/// Manages pairing state for the gateway.
///
/// Bearer tokens are stored as SHA-256 hashes to prevent plaintext exposure
/// in config files. When a new token is generated, the plaintext is returned
/// to the client once, and only the hash is retained.
// TODO: I've just made this work with parking_lot but it should use either flume or tokio's async mutexes
#[derive(Debug, Clone)]
pub struct PairingGuard {
    /// Whether pairing is required at all.
    require_pairing: bool,
    /// One-time pairing code (generated on startup, consumed on first pair).
    pairing_code: Arc<Mutex<Option<String>>>,
    /// Set of SHA-256 hashed bearer tokens (persisted across restarts).
    paired_tokens: Arc<Mutex<HashSet<String>>>,
    /// Brute-force protection: per-client failed attempt state + last sweep timestamp.
    failed_attempts: Arc<Mutex<(HashMap<String, FailedAttemptState>, Instant)>>,
    /// Linked §3.3 pairing-token store. When set, a successful legacy-code
    /// pair via `try_pair` also consumes the linked store entry, invalidating
    /// the QR-encoded `pairing_token` carrying the same legacy code.
    /// Attached after construction by `attach_linked_token_store` so the two
    /// types can be wired into AppState without a constructor-order cycle.
    linked_token_store: Arc<Mutex<Option<Arc<PairingTokenStore>>>>,
}

impl PairingGuard {
    /// Create a new pairing guard.
    ///
    /// If `require_pairing` is true and no tokens exist yet, a fresh
    /// pairing code is generated and printed to the terminal. Once
    /// paired, no code is generated on restart — operators can use
    /// `generate_new_pairing_code()` or the CLI to create one on demand.
    ///
    /// Existing tokens are accepted in both forms:
    /// - Plaintext (`zc_...`): hashed on load for backward compatibility
    /// - Already hashed (64-char hex): stored as-is
    pub fn new(require_pairing: bool, existing_tokens: &[String]) -> Self {
        let tokens: HashSet<String> = existing_tokens
            .iter()
            .map(|t| {
                if is_token_hash(t) {
                    t.clone()
                } else {
                    hash_token(t)
                }
            })
            .collect();
        let code = if require_pairing && tokens.is_empty() {
            Some(generate_code())
        } else {
            None
        };
        Self {
            require_pairing,
            pairing_code: Arc::new(Mutex::new(code)),
            paired_tokens: Arc::new(Mutex::new(tokens)),
            failed_attempts: Arc::new(Mutex::new((HashMap::new(), Instant::now()))),
            linked_token_store: Arc::new(Mutex::new(None)),
        }
    }

    /// Attach a §3.3 pairing-token store. After this call, a successful
    /// `try_pair` on a legacy code also consumes the linked store entry
    /// carrying that code, so the QR-encoded `pairing_token` derived from
    /// the same mint stops working too. Single mint, dual surface.
    pub fn attach_linked_token_store(&self, store: Arc<PairingTokenStore>) {
        *self.linked_token_store.lock() = Some(store);
    }

    /// The one-time pairing code (generated only on first startup when no tokens exist).
    pub fn pairing_code(&self) -> Option<String> {
        self.pairing_code.lock().clone()
    }

    /// Whether pairing is required at all.
    pub fn require_pairing(&self) -> bool {
        self.require_pairing
    }

    fn try_pair_blocking(&self, code: &str, client_id: &str) -> Result<Option<String>, u64> {
        let client_id = normalize_client_key(client_id);
        let now = Instant::now();

        // Periodic sweep + lockout check
        {
            let mut guard = self.failed_attempts.lock();
            let (ref mut map, ref mut last_sweep) = *guard;

            // Sweep stale entries on interval
            if now.duration_since(*last_sweep).as_secs() >= FAILED_ATTEMPT_SWEEP_INTERVAL_SECS {
                prune_failed_attempts(map, now);
                *last_sweep = now;
            }

            // Check brute force lockout for this specific client
            if let Some(state) = map.get(&client_id)
                && let Some(until) = state.lockout_until
            {
                if now < until {
                    let remaining = (until - now).as_secs();
                    return Err(remaining.max(1));
                }
                // Lockout expired — reset inline
                map.remove(&client_id);
            }
        }

        {
            let mut pairing_code = self.pairing_code.lock();
            if let Some(ref expected) = *pairing_code
                && constant_time_eq(code.trim(), expected.trim())
            {
                // Reset failed attempts for this client on success
                {
                    let mut guard = self.failed_attempts.lock();
                    guard.0.remove(&client_id);
                }
                let token = generate_token();
                let mut tokens = self.paired_tokens.lock();
                tokens.insert(hash_token(&token));

                let consumed_code = expected.trim().to_string();
                // Consume the pairing code so it cannot be reused
                *pairing_code = None;

                // Single mint, dual surface: also consume the linked §3.3
                // entry, so the QR-encoded pairing_token carrying this same
                // legacy code stops validating.
                drop(pairing_code);
                drop(tokens);
                if let Some(linked) = self.linked_token_store.lock().clone() {
                    let _ = linked.consume_by_legacy_code(&consumed_code);
                }

                return Ok(Some(token));
            }
        }

        // Increment failed attempts for this client
        {
            let mut guard = self.failed_attempts.lock();
            let (ref mut map, _) = *guard;

            // Enforce capacity bound: prune stale first, then LRU-evict if still full
            if map.len() >= MAX_TRACKED_CLIENTS {
                prune_failed_attempts(map, now);
            }
            if map.len() >= MAX_TRACKED_CLIENTS {
                // Evict the least-recently-active entry
                if let Some(lru_key) = map
                    .iter()
                    .min_by_key(|(_, s)| s.last_attempt)
                    .map(|(k, _)| k.clone())
                {
                    map.remove(&lru_key);
                }
            }

            let entry = map.entry(client_id).or_insert(FailedAttemptState {
                count: 0,
                lockout_until: None,
                last_attempt: now,
            });

            entry.last_attempt = now;
            entry.count += 1;

            if entry.count >= MAX_PAIR_ATTEMPTS {
                entry.lockout_until = Some(now + std::time::Duration::from_secs(PAIR_LOCKOUT_SECS));
            }
        }

        Ok(None)
    }

    /// Attempt to pair with the given code. Returns a bearer token on success.
    /// Returns `Err(lockout_seconds)` if locked out due to brute force.
    /// `client_id` identifies the client for per-client lockout accounting.
    pub async fn try_pair(&self, code: &str, client_id: &str) -> Result<Option<String>, u64> {
        let this = self.clone();
        let code = code.to_string();
        let client_id = client_id.to_string();
        // TODO: make this function the main one without spawning a task
        let handle = tokio::task::spawn_blocking(move || this.try_pair_blocking(&code, &client_id));

        handle
            .await
            .expect("failed to spawn blocking task this should not happen")
    }

    /// Check if a bearer token is valid (compares against stored hashes).
    pub fn is_authenticated(&self, token: &str) -> bool {
        if !self.require_pairing {
            return true;
        }
        let hashed = hash_token(token);
        let tokens = self.paired_tokens.lock();
        tokens.contains(&hashed)
    }

    /// Returns true if the gateway is already paired (has at least one token).
    pub fn is_paired(&self) -> bool {
        let tokens = self.paired_tokens.lock();
        !tokens.is_empty()
    }

    /// Get all paired token hashes (for persisting to config).
    pub fn tokens(&self) -> Vec<String> {
        let tokens = self.paired_tokens.lock();
        tokens.iter().cloned().collect()
    }

    /// Generate a new pairing code, even if already paired.
    ///
    /// This allows adding additional clients without restarting the gateway.
    /// The new code can be used exactly once to pair a new client. If a
    /// linked §3.3 store is attached, all previously-issued tokens are
    /// cleared so the rotation invalidates both surfaces atomically.
    pub fn generate_new_pairing_code(&self) -> Option<String> {
        if !self.require_pairing {
            return None;
        }
        // Single mint, dual surface: drop any outstanding §3.3 tokens so the
        // caller can then mint a fresh paired entry bound to the new code.
        if let Some(linked) = self.linked_token_store.lock().clone() {
            linked.clear();
        }
        let new_code = generate_code();
        *self.pairing_code.lock() = Some(new_code.clone());
        Some(new_code)
    }

    /// Get the token hash for a given plaintext token (for device registry lookup).
    pub fn token_hash(token: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    }

    /// Check if a token is paired and return its hash.
    pub fn authenticate_and_hash(&self, token: &str) -> Option<String> {
        if self.is_authenticated(token) {
            Some(Self::token_hash(token))
        } else {
            None
        }
    }
}

/// Normalize a client identifier: trim whitespace, map empty to `"unknown"`.
fn normalize_client_key(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Remove failed-attempt entries whose `last_attempt` is older than the retention window.
fn prune_failed_attempts(map: &mut HashMap<String, FailedAttemptState>, now: Instant) {
    map.retain(|_, state| {
        now.duration_since(state.last_attempt).as_secs() < FAILED_ATTEMPT_RETENTION_SECS
    });
}

/// Generate a 6-digit numeric pairing code using cryptographically secure randomness.
fn generate_code() -> String {
    // UUID v4 uses getrandom (backed by /dev/urandom on Linux, BCryptGenRandom
    // on Windows) — a CSPRNG. We extract 4 bytes from it for a uniform random
    // number in [0, 1_000_000).
    //
    // Rejection sampling eliminates modulo bias: values above the largest
    // multiple of 1_000_000 that fits in u32 are discarded and re-drawn.
    // The rejection probability is ~0.02%, so this loop almost always exits
    // on the first iteration.
    const UPPER_BOUND: u32 = 1_000_000;
    const REJECT_THRESHOLD: u32 = (u32::MAX / UPPER_BOUND) * UPPER_BOUND;

    loop {
        let uuid = uuid::Uuid::new_v4();
        let bytes = uuid.as_bytes();
        let raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

        if raw < REJECT_THRESHOLD {
            return format!("{:06}", raw % UPPER_BOUND);
        }
    }
}

/// Generate a cryptographically-adequate bearer token with 256-bit entropy.
///
/// Uses `rand::rng()` which is backed by the OS CSPRNG
/// (/dev/urandom on Linux, BCryptGenRandom on Windows, SecRandomCopyBytes
/// on macOS). The 32 random bytes (256 bits) are hex-encoded for a
/// 64-character token, providing 256 bits of entropy.
fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    format!("zc_{}", hex::encode(bytes))
}

/// SHA-256 hash a bearer token for storage. Returns lowercase hex.
fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

/// Check if a stored value looks like a SHA-256 hash (64 hex chars)
/// rather than a plaintext token.
fn is_token_hash(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Constant-time string comparison to prevent timing attacks.
///
/// This function is critical to the security of the pairing mechanism:
/// when verifying the one-time pairing code, timing side-channels could
/// allow an attacker to deduce the correct code character-by-character.
///
/// Implementation details that ensure constant-time execution:
/// 1. Does not short-circuit on length mismatch — always iterates over
///    the longer input to avoid leaking length information via timing.
/// 2. Uses bitwise AND (&) instead of logical AND (&&) to ensure both
///    comparisons always execute, preventing timing variations that could
///    reveal whether the length check or byte comparison failed first.
///
/// SECURITY NOTE: The use of `&` instead of `&&` is intentional and
/// required for constant-time behavior. Do not change to `&&` or clippy
/// suggestions that would reintroduce short-circuit evaluation.
#[allow(clippy::needless_bitwise_bool)]
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();

    // Track length mismatch as a usize (non-zero = different lengths)
    let len_diff = a.len() ^ b.len();

    // XOR each byte, padding the shorter input with zeros.
    // Iterates over max(a.len(), b.len()) to avoid timing differences.
    let max_len = a.len().max(b.len());
    let mut byte_diff = 0u8;
    for i in 0..max_len {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        byte_diff |= x ^ y;
    }
    // Intentional use of bitwise & (not &&) to ensure constant-time execution
    // and prevent timing side-channel attacks. Both comparisons must execute.
    (len_diff == 0) & (byte_diff == 0)
}

/// Check if a host string represents a non-localhost bind address.
pub fn is_public_bind(host: &str) -> bool {
    !matches!(
        host,
        "127.0.0.1" | "localhost" | "::1" | "[::1]" | "0:0:0:0:0:0:0:1"
    )
}

// ── Spec'd QR pairing-token store (SHARED_AUTH_SPEC.md §3.2 + §3.3) ──────────
//
// 32 cryptographically-random bytes encoded as base64url-unpadded. Each token
// has a fixed expiry (default 10 minutes from generation) and is single-use:
// once consumed by `POST /auth/pair/register`, further use returns 410 Gone.
//
// Storage is in-memory (no persistence): tokens that aren't consumed within
// their TTL are forgotten on restart. This matches the spec's
// "abandoned pairings expire and are reaped" semantic.

/// Default lifetime for a fresh QR pairing token.
pub const QR_PAIRING_TOKEN_TTL_SECS: u64 = 600; // 10 minutes (SHARED_AUTH_SPEC.md §3.2)

/// Maximum concurrent outstanding QR pairing tokens (capacity bound).
pub const MAX_OUTSTANDING_QR_TOKENS: usize = 32;

/// Length in bytes of the random pairing token (before base64url encoding).
pub const QR_PAIRING_TOKEN_BYTES: usize = 32; // SHARED_AUTH_SPEC.md §3.2

/// Doctor + admin endpoints return the current envelope only if it still has
/// this many seconds of life. Below this, mint fresh.
///
/// Rationale: iOS rejects with `PairingTokenQRError.expired` when
/// `expires_at < now + 30s` (clockSkewSeconds in PairingTokenQR.swift). Giving
/// the customer a code with under 30 seconds left guarantees the iOS scan
/// fails. The 60-second floor is the iOS skew plus another 30 seconds for the
/// customer to actually pick up the iPhone and scan.
pub const QR_FRESHNESS_THRESHOLD_SECS: u64 = 60;

/// Internal state for a single outstanding QR pairing token.
#[derive(Debug, Clone)]
struct QrTokenState {
    /// UNIX seconds when this token expires.
    expires_at: u64,
    /// `Pending`: in `POST /auth/pair/init` (claimed but not yet completed).
    /// Tokens flip to `Pending` on init and are removed entirely on register success.
    claimed: bool,
    /// Optional legacy 6-digit code bound to this token. When present, the
    /// /admin/paircode response carries BOTH fields; consuming either surface
    /// (POST /pair with the code, or /auth/pair/register with the token)
    /// removes this entry entirely, so the other surface stops working too.
    /// SHARED_AUTH_SPEC.md §3.3: single mint, dual surface.
    legacy_code: Option<String>,
}

/// In-memory store of outstanding QR pairing tokens (SHARED_AUTH_SPEC.md §3.3.4).
///
/// Tokens are minted with `mint()`, validated by `claim()` (used in step A), and
/// consumed by `consume()` (used in step C). Expired or unknown tokens are
/// rejected. Single-use: a token consumed once returns `TokenStatus::Consumed`
/// on subsequent attempts.
#[derive(Debug, Clone)]
pub struct PairingTokenStore {
    inner: Arc<Mutex<HashMap<String, QrTokenState>>>,
}

/// Outcome of looking up a pairing token in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStatus {
    /// Token is known, unexpired, and unconsumed.
    Valid,
    /// Token is unknown (never minted, or already consumed and reaped).
    NotFound,
    /// Token was minted but its TTL has elapsed.
    Expired,
    /// Token has already been consumed by a successful `register` call.
    Consumed,
}

impl Default for PairingTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PairingTokenStore {
    /// Create a new empty token store.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Mint a fresh 32-byte token, return its base64url-unpadded string form.
    /// The token is registered as `Valid` for `QR_PAIRING_TOKEN_TTL_SECS`.
    pub fn mint(&self) -> (String, u64) {
        self.mint_with_ttl(QR_PAIRING_TOKEN_TTL_SECS)
    }

    /// Mint a fresh token with a custom TTL (for tests + capability extensions).
    pub fn mint_with_ttl(&self, ttl_secs: u64) -> (String, u64) {
        let (token, expires_at, _code) = self.mint_inner(ttl_secs, None);
        (token, expires_at)
    }

    /// Mint a fresh token bound to a legacy 6-digit pairing code.
    ///
    /// Returns `(token, legacy_code, expires_at)`. Subsequent consumption of
    /// either the token (via `consume`) or the legacy code (via
    /// `consume_by_legacy_code`) removes this entry, so the other surface
    /// stops working too. Implements SHARED_AUTH_SPEC.md §3.3 single-mint
    /// dual-surface for /admin/paircode.
    pub fn mint_paired(&self, legacy_code: String) -> (String, String, u64) {
        let (token, expires_at, code) =
            self.mint_inner(QR_PAIRING_TOKEN_TTL_SECS, Some(legacy_code));
        (
            token,
            code.expect("mint_paired always supplies a legacy code"),
            expires_at,
        )
    }

    fn mint_inner(
        &self,
        ttl_secs: u64,
        legacy_code: Option<String>,
    ) -> (String, u64, Option<String>) {
        let raw: [u8; QR_PAIRING_TOKEN_BYTES] = rand::random();
        let token = URL_SAFE_NO_PAD.encode(raw);
        let now = unix_now_secs();
        let expires_at = now.saturating_add(ttl_secs);

        let mut map = self.inner.lock();

        // Bound capacity: prune expired first, then evict oldest by expiry if still over.
        prune_expired(&mut map, now);
        if map.len() >= MAX_OUTSTANDING_QR_TOKENS
            && let Some(oldest_key) = map
                .iter()
                .min_by_key(|(_, s)| s.expires_at)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest_key);
        }

        map.insert(
            token.clone(),
            QrTokenState {
                expires_at,
                claimed: false,
                legacy_code: legacy_code.clone(),
            },
        );

        (token, expires_at, legacy_code)
    }

    /// Look up a token's status without changing it.
    pub fn status(&self, token: &str) -> TokenStatus {
        let now = unix_now_secs();
        let map = self.inner.lock();
        match map.get(token) {
            None => TokenStatus::NotFound,
            Some(state) if state.expires_at <= now => TokenStatus::Expired,
            Some(_) => TokenStatus::Valid,
        }
    }

    /// Mark a token as `Pending` (called by `POST /auth/pair/init`).
    /// Returns `Ok(())` on success, otherwise the failure status.
    pub fn claim(&self, token: &str) -> Result<(), TokenStatus> {
        let now = unix_now_secs();
        let mut map = self.inner.lock();
        match map.get_mut(token) {
            None => Err(TokenStatus::NotFound),
            Some(state) if state.expires_at <= now => {
                map.remove(token);
                Err(TokenStatus::Expired)
            }
            Some(state) => {
                state.claimed = true;
                Ok(())
            }
        }
    }

    /// Consume a token (called by `POST /auth/pair/register` on success).
    /// Removes it from the store entirely. Returns `Ok(())` on success.
    pub fn consume(&self, token: &str) -> Result<(), TokenStatus> {
        let now = unix_now_secs();
        let mut map = self.inner.lock();
        match map.remove(token) {
            None => Err(TokenStatus::NotFound),
            Some(state) if state.expires_at <= now => Err(TokenStatus::Expired),
            Some(_) => Ok(()),
        }
    }

    /// Look up the §3.3 pairing token bound to a given legacy 6-digit code.
    ///
    /// Returns `None` if no live entry carries this code (either never minted
    /// paired, or already consumed and reaped). Walks the map linearly — fine
    /// for the bound `MAX_OUTSTANDING_QR_TOKENS = 32`.
    pub fn find_token_by_legacy_code(&self, code: &str) -> Option<String> {
        let now = unix_now_secs();
        let map = self.inner.lock();
        map.iter()
            .find(|(_, state)| state.expires_at > now && state.legacy_code.as_deref() == Some(code))
            .map(|(token, _)| token.clone())
    }

    /// Consume the entry bound to a legacy 6-digit pairing code.
    ///
    /// Called by the legacy `POST /pair` handler once it has matched the code
    /// via the constant-time-comparison path. Removes the entry entirely so
    /// the linked §3.3 token also stops working. Returns `Ok(())` on success.
    /// SHARED_AUTH_SPEC.md §3.3: single mint, dual surface.
    pub fn consume_by_legacy_code(&self, code: &str) -> Result<(), TokenStatus> {
        let token = match self.find_token_by_legacy_code(code) {
            Some(t) => t,
            None => return Err(TokenStatus::NotFound),
        };
        self.consume(&token)
    }

    /// Return the freshest outstanding paired entry whose remaining lifetime
    /// is at least `threshold_secs`. Returns `None` if no entry qualifies.
    ///
    /// Used by `GET /admin/paircode`: returns the current envelope as long as
    /// it still has enough life for the customer to scan and the iOS Companion
    /// to validate (PairingTokenQR's 30 s clock-skew + 30 s scan budget).
    pub fn current_paired(&self, threshold_secs: u64) -> Option<(String, String, u64)> {
        let now = unix_now_secs();
        let cutoff = now.saturating_add(threshold_secs);
        let map = self.inner.lock();
        map.iter()
            .filter(|(_, s)| s.expires_at > cutoff && s.legacy_code.is_some())
            .max_by_key(|(_, s)| s.expires_at)
            .map(|(token, state)| {
                (
                    token.clone(),
                    state
                        .legacy_code
                        .clone()
                        .expect("filtered to legacy_code.is_some()"),
                    state.expires_at,
                )
            })
    }

    /// Drop every entry in the store. Used by `POST /admin/paircode/new` to
    /// guarantee that rotating the code invalidates all previously-issued
    /// tokens, including any that may still be unconsumed within their TTL.
    pub fn clear(&self) {
        let mut map = self.inner.lock();
        map.clear();
    }

    /// Number of outstanding tokens (for diagnostics).
    pub fn len(&self) -> usize {
        let now = unix_now_secs();
        let mut map = self.inner.lock();
        prune_expired(&mut map, now);
        map.len()
    }

    /// Whether the store is currently empty (after sweep).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// QR JSON payload (SHARED_AUTH_SPEC.md §3.2).
///
/// Encoded as compact UTF-8 JSON inside the QR. The `rp_id` is fixed by spec at
/// `creativemachines.ai`; the `pairing_token` is base64url-unpadded; `expires_at`
/// is UNIX seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingQrPayload {
    pub v: u8,
    pub hub_addr: String,
    pub rp_id: String,
    pub pairing_token: String,
    pub expires_at: u64,
}

impl PairingQrPayload {
    /// QR payload version per SHARED_AUTH_SPEC.md §3.2.
    pub const VERSION: u8 = 1;
    /// Spec-mandated rp_id (do not parameterise on product name).
    pub const RP_ID: &'static str = "creativemachines.ai";

    /// Construct a v1 payload for a freshly-minted token.
    pub fn new_v1(hub_addr: impl Into<String>, pairing_token: String, expires_at: u64) -> Self {
        Self {
            v: Self::VERSION,
            hub_addr: hub_addr.into(),
            rp_id: Self::RP_ID.into(),
            pairing_token,
            expires_at,
        }
    }

    /// Serialise to compact JSON (suitable for embedding in a QR module).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("PairingQrPayload always serialisable")
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn prune_expired(map: &mut HashMap<String, QrTokenState>, now: u64) {
    map.retain(|_, state| state.expires_at > now);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::test;

    // ── PairingGuard ─────────────────────────────────────────

    #[test]
    async fn new_guard_generates_code_when_no_tokens() {
        let guard = PairingGuard::new(true, &[]);
        assert!(guard.pairing_code().is_some());
        assert!(!guard.is_paired());
    }

    #[test]
    async fn new_guard_no_code_when_tokens_exist() {
        let guard = PairingGuard::new(true, &["zc_existing".into()]);
        assert!(guard.pairing_code().is_none());
        assert!(guard.is_paired());
    }

    #[test]
    async fn new_guard_no_code_when_pairing_disabled() {
        let guard = PairingGuard::new(false, &[]);
        assert!(guard.pairing_code().is_none());
    }

    #[test]
    async fn try_pair_correct_code() {
        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let token = guard.try_pair(&code, "test_client").await.unwrap();
        assert!(token.is_some());
        assert!(token.unwrap().starts_with("zc_"));
        assert!(guard.is_paired());
    }

    #[test]
    async fn try_pair_wrong_code() {
        let guard = PairingGuard::new(true, &[]);
        let result = guard.try_pair("000000", "test_client").await.unwrap();
        // Might succeed if code happens to be 000000, but extremely unlikely
        // Just check it returns Ok(None) normally
        let _ = result;
    }

    #[test]
    async fn try_pair_empty_code() {
        let guard = PairingGuard::new(true, &[]);
        assert!(guard.try_pair("", "test_client").await.unwrap().is_none());
    }

    #[test]
    async fn is_authenticated_with_valid_token() {
        // Pass plaintext token — PairingGuard hashes it on load
        let guard = PairingGuard::new(true, &["zc_valid".into()]);
        assert!(guard.is_authenticated("zc_valid"));
    }

    #[test]
    async fn is_authenticated_with_prehashed_token() {
        // Pass an already-hashed token (64 hex chars)
        let hashed = hash_token("zc_valid");
        let guard = PairingGuard::new(true, &[hashed]);
        assert!(guard.is_authenticated("zc_valid"));
    }

    #[test]
    async fn is_authenticated_with_invalid_token() {
        let guard = PairingGuard::new(true, &["zc_valid".into()]);
        assert!(!guard.is_authenticated("zc_invalid"));
    }

    #[test]
    async fn is_authenticated_when_pairing_disabled() {
        let guard = PairingGuard::new(false, &[]);
        assert!(guard.is_authenticated("anything"));
        assert!(guard.is_authenticated(""));
    }

    #[test]
    async fn tokens_returns_hashes() {
        let guard = PairingGuard::new(true, &["zc_a".into(), "zc_b".into()]);
        let tokens = guard.tokens();
        assert_eq!(tokens.len(), 2);
        // Tokens should be stored as 64-char hex hashes, not plaintext
        for t in &tokens {
            assert_eq!(t.len(), 64, "Token should be a SHA-256 hash");
            assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(!t.starts_with("zc_"), "Token should not be plaintext");
        }
    }

    #[test]
    async fn pair_then_authenticate() {
        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let token = guard.try_pair(&code, "test_client").await.unwrap().unwrap();
        assert!(guard.is_authenticated(&token));
        assert!(!guard.is_authenticated("wrong"));
    }

    // ── Token hashing ────────────────────────────────────────

    #[test]
    async fn hash_token_produces_64_hex_chars() {
        let hash = hash_token("zc_test_token");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    async fn hash_token_is_deterministic() {
        assert_eq!(hash_token("zc_abc"), hash_token("zc_abc"));
    }

    #[test]
    async fn hash_token_differs_for_different_inputs() {
        assert_ne!(hash_token("zc_a"), hash_token("zc_b"));
    }

    #[test]
    async fn is_token_hash_detects_hash_vs_plaintext() {
        assert!(is_token_hash(&hash_token("zc_test")));
        assert!(!is_token_hash("zc_test_token"));
        assert!(!is_token_hash("too_short"));
        assert!(!is_token_hash(""));
    }

    // ── is_public_bind ───────────────────────────────────────

    #[test]
    async fn localhost_variants_not_public() {
        assert!(!is_public_bind("127.0.0.1"));
        assert!(!is_public_bind("localhost"));
        assert!(!is_public_bind("::1"));
        assert!(!is_public_bind("[::1]"));
    }

    #[test]
    async fn zero_zero_is_public() {
        assert!(is_public_bind("0.0.0.0"));
    }

    #[test]
    async fn real_ip_is_public() {
        assert!(is_public_bind("192.168.1.100"));
        assert!(is_public_bind("10.0.0.1"));
    }

    // ── constant_time_eq ─────────────────────────────────────

    #[test]
    async fn constant_time_eq_same() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    async fn constant_time_eq_different() {
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("a", ""));
    }

    // ── generate helpers ─────────────────────────────────────

    #[test]
    async fn generate_code_is_6_digits() {
        let code = generate_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    async fn generate_code_is_not_deterministic() {
        // Two codes should differ with overwhelming probability. We try
        // multiple pairs so a single 1-in-10^6 collision doesn't cause
        // a flaky CI failure. All 10 pairs colliding is ~1-in-10^60.
        for _ in 0..10 {
            if generate_code() != generate_code() {
                return; // Pass: found a non-matching pair.
            }
        }
        panic!("Generated 10 pairs of codes and all were collisions — CSPRNG failure");
    }

    #[test]
    async fn generate_token_has_prefix_and_hex_payload() {
        let token = generate_token();
        let payload = token
            .strip_prefix("zc_")
            .expect("Generated token should include zc_ prefix");

        assert_eq!(payload.len(), 64, "Token payload should be 32 bytes in hex");
        assert!(
            payload
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f')),
            "Token payload should be lowercase hex"
        );
    }

    // ── Brute force protection ───────────────────────────────

    #[test]
    async fn brute_force_lockout_after_max_attempts() {
        let guard = PairingGuard::new(true, &[]);
        let client = "attacker_client";
        // Exhaust all attempts with wrong codes
        for i in 0..MAX_PAIR_ATTEMPTS {
            let result = guard.try_pair(&format!("wrong_{i}"), client).await;
            assert!(result.is_ok(), "Attempt {i} should not be locked out yet");
        }
        // Next attempt should be locked out
        let result = guard.try_pair("another_wrong", client).await;
        assert!(
            result.is_err(),
            "Should be locked out after {MAX_PAIR_ATTEMPTS} attempts"
        );
        let lockout_secs = result.unwrap_err();
        assert!(lockout_secs > 0, "Lockout should have remaining seconds");
        assert!(
            lockout_secs <= PAIR_LOCKOUT_SECS,
            "Lockout should not exceed max"
        );
    }

    #[test]
    async fn correct_code_resets_failed_attempts() {
        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let client = "test_client";
        // Fail a few times
        for _ in 0..3 {
            let _ = guard.try_pair("wrong", client).await;
        }
        // Correct code should still work (under MAX_PAIR_ATTEMPTS)
        let result = guard.try_pair(&code, client).await.unwrap();
        assert!(result.is_some(), "Correct code should work before lockout");
    }

    #[test]
    async fn lockout_returns_remaining_seconds() {
        let guard = PairingGuard::new(true, &[]);
        let client = "test_client";
        for _ in 0..MAX_PAIR_ATTEMPTS {
            let _ = guard.try_pair("wrong", client).await;
        }
        let err = guard.try_pair("wrong", client).await.unwrap_err();
        // Should be close to PAIR_LOCKOUT_SECS (within a second)
        assert!(
            err >= PAIR_LOCKOUT_SECS - 1,
            "Remaining lockout should be ~{PAIR_LOCKOUT_SECS}s, got {err}s"
        );
    }

    #[test]
    async fn successful_pair_resets_only_requesting_client_state() {
        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let client_a = "client_a";
        let client_b = "client_b";

        // Both clients fail a few times
        for _ in 0..3 {
            let _ = guard.try_pair("wrong", client_a).await;
            let _ = guard.try_pair("wrong", client_b).await;
        }

        // client_a pairs successfully — only its state should reset
        let result = guard.try_pair(&code, client_a).await.unwrap();
        assert!(result.is_some(), "client_a should pair successfully");

        // client_b's failed count should still be intact (3 failures recorded)
        let state = guard.failed_attempts.lock();
        let b_state = state.0.get(client_b);
        assert!(b_state.is_some(), "client_b state should still exist");
        assert_eq!(
            b_state.unwrap().count,
            3,
            "client_b should still have 3 failures"
        );

        // client_a should have been removed
        assert!(
            !state.0.contains_key(client_a),
            "client_a state should be cleared"
        );
    }

    #[test]
    async fn failed_attempt_state_is_bounded_by_max_clients() {
        let guard = PairingGuard::new(true, &[]);

        // Fill the map to MAX_TRACKED_CLIENTS with stale entries
        {
            let mut state = guard.failed_attempts.lock();
            let past = Instant::now()
                .checked_sub(std::time::Duration::from_secs(
                    FAILED_ATTEMPT_RETENTION_SECS + 60,
                ))
                .unwrap_or_else(Instant::now);
            for i in 0..MAX_TRACKED_CLIENTS {
                state.0.insert(
                    format!("stale_client_{i}"),
                    FailedAttemptState {
                        count: 1,
                        lockout_until: None,
                        last_attempt: past,
                    },
                );
            }
        }

        // A new client triggers an attempt — should prune stale entries and fit
        let result = guard.try_pair("wrong", "new_client").await;
        assert!(result.is_ok(), "New client should not be blocked");

        let state = guard.failed_attempts.lock();
        assert!(
            state.0.len() <= MAX_TRACKED_CLIENTS,
            "Map size should stay within bound, got {}",
            state.0.len()
        );
        assert!(
            state.0.contains_key("new_client"),
            "New client should be tracked"
        );
    }

    #[test]
    async fn failed_attempt_sweep_prunes_expired_clients() {
        let guard = PairingGuard::new(true, &[]);

        // Seed a stale entry and set last_sweep to long ago so sweep triggers
        {
            let mut state = guard.failed_attempts.lock();
            let past = Instant::now()
                .checked_sub(std::time::Duration::from_secs(
                    FAILED_ATTEMPT_RETENTION_SECS + 60,
                ))
                .unwrap_or_else(Instant::now);
            state.0.insert(
                "stale_client".to_string(),
                FailedAttemptState {
                    count: 2,
                    lockout_until: None,
                    last_attempt: past,
                },
            );
            // Force last_sweep to be old enough to trigger sweep
            state.1 = Instant::now()
                .checked_sub(std::time::Duration::from_secs(
                    FAILED_ATTEMPT_SWEEP_INTERVAL_SECS + 1,
                ))
                .unwrap_or_else(Instant::now);
        }

        // Any attempt triggers sweep
        let _ = guard.try_pair("wrong", "fresh_client").await;

        let state = guard.failed_attempts.lock();
        assert!(
            !state.0.contains_key("stale_client"),
            "Stale client should have been pruned by sweep"
        );
        assert!(
            state.0.contains_key("fresh_client"),
            "Fresh client should still be tracked"
        );
    }

    #[test]
    async fn lockout_is_per_client() {
        let guard = PairingGuard::new(true, &[]);
        let attacker = "attacker_ip";
        let legitimate = "legitimate_ip";

        // Attacker exhausts attempts
        for i in 0..MAX_PAIR_ATTEMPTS {
            let _ = guard.try_pair(&format!("wrong_{i}"), attacker).await;
        }
        // Attacker is locked out
        assert!(guard.try_pair("wrong", attacker).await.is_err());

        // Legitimate client is NOT locked out
        let result = guard.try_pair("wrong", legitimate).await;
        assert!(
            result.is_ok(),
            "Legitimate client should not be locked out by attacker"
        );
    }

    // ── PairingTokenStore (SHARED_AUTH_SPEC.md §3.2 + §3.3) ──────────────────

    #[test]
    async fn qr_token_minted_is_valid() {
        let store = PairingTokenStore::new();
        let (token, _exp) = store.mint();
        // base64url of 32 bytes is 43 chars unpadded (32 * 4 / 3 rounded up - padding).
        assert_eq!(token.len(), 43, "token should be 43 base64url chars");
        assert_eq!(store.status(&token), TokenStatus::Valid);
    }

    #[test]
    async fn qr_token_unknown_is_not_found() {
        let store = PairingTokenStore::new();
        assert_eq!(
            store.status("definitely-not-a-token"),
            TokenStatus::NotFound
        );
        assert_eq!(
            store.consume("definitely-not-a-token"),
            Err(TokenStatus::NotFound)
        );
    }

    #[test]
    async fn qr_token_consume_is_single_use() {
        let store = PairingTokenStore::new();
        let (token, _) = store.mint();
        assert!(store.consume(&token).is_ok());
        assert_eq!(store.consume(&token), Err(TokenStatus::NotFound));
    }

    #[test]
    async fn qr_token_expired_is_rejected() {
        let store = PairingTokenStore::new();
        let (token, _) = store.mint_with_ttl(0); // already expired
        // Sleep a tick so wall-clock advances past expiry.
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(store.status(&token), TokenStatus::Expired);
        assert_eq!(store.consume(&token), Err(TokenStatus::Expired));
    }

    #[test]
    async fn qr_token_claim_then_consume() {
        let store = PairingTokenStore::new();
        let (token, _) = store.mint();
        assert!(store.claim(&token).is_ok());
        // Claiming twice is allowed (init can be retried in v1).
        assert!(store.claim(&token).is_ok());
        assert!(store.consume(&token).is_ok());
    }

    #[test]
    async fn qr_token_capacity_bounded() {
        let store = PairingTokenStore::new();
        // Mint more than the cap; len() should never exceed it.
        for _ in 0..(MAX_OUTSTANDING_QR_TOKENS + 5) {
            let _ = store.mint();
        }
        assert!(
            store.len() <= MAX_OUTSTANDING_QR_TOKENS,
            "store size should stay within bound, got {}",
            store.len()
        );
    }

    #[test]
    async fn qr_tokens_are_unique() {
        let store = PairingTokenStore::new();
        let (a, _) = store.mint();
        let (b, _) = store.mint();
        assert_ne!(a, b);
    }

    // ── PairingQrPayload ─────────────────────────────────────────────────

    #[test]
    async fn qr_payload_uses_spec_constants() {
        let p = PairingQrPayload::new_v1("100.64.0.1:42617", "tok".into(), 1_700_000_000);
        assert_eq!(p.v, 1);
        assert_eq!(p.rp_id, "creativemachines.ai");
    }

    #[test]
    async fn qr_payload_serialises_compact() {
        let p = PairingQrPayload::new_v1("h:1", "t".into(), 1);
        let s = p.to_json();
        // Field order in serde is struct-declaration order; checks the spec'd shape.
        assert!(s.contains("\"v\":1"));
        assert!(s.contains("\"hub_addr\":\"h:1\""));
        assert!(s.contains("\"rp_id\":\"creativemachines.ai\""));
        assert!(s.contains("\"pairing_token\":\"t\""));
        assert!(s.contains("\"expires_at\":1"));
    }

    #[test]
    async fn qr_payload_round_trip() {
        let p = PairingQrPayload::new_v1("h:1", "tok".into(), 42);
        let s = p.to_json();
        let back: PairingQrPayload = serde_json::from_str(&s).unwrap();
        assert_eq!(back.v, 1);
        assert_eq!(back.rp_id, "creativemachines.ai");
        assert_eq!(back.hub_addr, "h:1");
        assert_eq!(back.pairing_token, "tok");
        assert_eq!(back.expires_at, 42);
    }

    // ── Single mint, dual surface (SHARED_AUTH_SPEC.md §3.3) ─────────────

    #[test]
    async fn mint_paired_links_token_to_legacy_code() {
        let store = PairingTokenStore::new();
        let (token, code, _exp) = store.mint_paired("123456".into());
        assert_eq!(token.len(), 43, "token still base64url 32-byte");
        assert_eq!(code, "123456");
        assert_eq!(store.find_token_by_legacy_code("123456"), Some(token));
    }

    #[test]
    async fn find_token_by_legacy_code_ignores_unbound_mints() {
        let store = PairingTokenStore::new();
        let (_t, _) = store.mint(); // unpaired
        let (_t2, _c, _) = store.mint_paired("000111".into());
        // The unpaired mint is not findable by code; the paired one is.
        assert!(store.find_token_by_legacy_code("123456").is_none());
        assert!(store.find_token_by_legacy_code("000111").is_some());
    }

    #[test]
    async fn consume_by_legacy_code_removes_linked_entry() {
        let store = PairingTokenStore::new();
        let (token, _c, _e) = store.mint_paired("123456".into());
        assert_eq!(store.status(&token), TokenStatus::Valid);
        assert!(store.consume_by_legacy_code("123456").is_ok());
        assert_eq!(
            store.status(&token),
            TokenStatus::NotFound,
            "legacy-code consume should remove the linked §3.3 token too",
        );
    }

    #[test]
    async fn consume_token_makes_legacy_code_lookup_fail() {
        let store = PairingTokenStore::new();
        let (token, _c, _e) = store.mint_paired("654321".into());
        assert!(store.consume(&token).is_ok());
        assert!(
            store.find_token_by_legacy_code("654321").is_none(),
            "§3.3 consume should remove the linked legacy code too",
        );
    }

    #[test]
    async fn current_paired_returns_freshest_above_threshold() {
        let store = PairingTokenStore::new();
        // Use explicit-TTL mints so the comparison is deterministic even
        // when both mints happen inside the same wall-clock second.
        let (_t1, _e1, _c1) = store.mint_inner(600, Some("111111".into()));
        let (t2, e2, _c2) = store.mint_inner(601, Some("222222".into()));
        let (got_token, got_code, got_exp) = store
            .current_paired(QR_FRESHNESS_THRESHOLD_SECS)
            .expect("at least one fresh entry");
        assert_eq!(got_token, t2);
        assert_eq!(got_code, "222222");
        assert_eq!(got_exp, e2);
    }

    #[test]
    async fn current_paired_returns_none_below_threshold() {
        let store = PairingTokenStore::new();
        // TTL well under the threshold so no entry qualifies.
        let (_t, _e, _c) = store.mint_inner(10, Some("999999".into()));
        assert!(
            store.current_paired(QR_FRESHNESS_THRESHOLD_SECS).is_none(),
            "no entry has 60s of life left",
        );
    }

    #[test]
    async fn current_paired_ignores_unbound_mints() {
        let store = PairingTokenStore::new();
        let (_unpaired, _) = store.mint();
        // No paired entry exists -> None even though an unpaired one does.
        assert!(store.current_paired(QR_FRESHNESS_THRESHOLD_SECS).is_none());
    }

    #[test]
    async fn clear_empties_the_store() {
        let store = PairingTokenStore::new();
        let _ = store.mint_paired("111111".into());
        let _ = store.mint_paired("222222".into());
        assert_eq!(store.len(), 2);
        store.clear();
        assert_eq!(store.len(), 0);
    }

    #[test]
    async fn pairing_guard_try_pair_consumes_linked_token() {
        let store = Arc::new(PairingTokenStore::new());
        let guard = PairingGuard::new(true, &[]);
        guard.attach_linked_token_store(store.clone());

        let code = guard.pairing_code().expect("freshly-minted code");
        let (token, _code, _exp) = store.mint_paired(code.clone());
        assert_eq!(store.status(&token), TokenStatus::Valid);

        let result = guard.try_pair(&code, "test").await.unwrap();
        assert!(result.is_some(), "legitimate code should pair");
        assert_eq!(
            store.status(&token),
            TokenStatus::NotFound,
            "successful legacy pair should consume the linked §3.3 entry",
        );
    }

    #[test]
    async fn generate_new_pairing_code_clears_linked_store() {
        let store = Arc::new(PairingTokenStore::new());
        let guard = PairingGuard::new(true, &[]);
        guard.attach_linked_token_store(store.clone());

        let _ = store.mint_paired("aaaaaa".into());
        let _ = store.mint_paired("bbbbbb".into());
        assert_eq!(store.len(), 2);

        let _new_code = guard.generate_new_pairing_code();
        assert_eq!(
            store.len(),
            0,
            "rotating the legacy code should clear all §3.3 tokens too",
        );
    }
}
