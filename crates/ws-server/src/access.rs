use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

/// Requests per minute allowed across the whole process, for *authenticated*
/// workload. This is capacity management, not brute-force protection -- the
/// two are separated deliberately (see `AuthLimiter`), because conflating
/// them lets one hostile caller spend everybody's allowance.
const REQUESTS_PER_MINUTE: u32 = 120;
/// A tighter budget for endpoints that fan out to a paid upstream or an LLM.
const EXPENSIVE_REQUESTS_PER_MINUTE: u32 = 6;
/// Failed credential attempts tolerated from a single client IP per window.
/// Small on purpose: a legitimate client has one key and pastes it once, so
/// even a fat-fingered human stays far inside this. Ten leaves room for a
/// retry loop or a stale stored key across a couple of devices behind one
/// NAT without ever approaching a rate that makes guessing viable.
const AUTH_FAILURES_PER_WINDOW: u32 = 10;
const AUTH_WINDOW: Duration = Duration::from_secs(60);
/// Hard cap on tracked IPs. `AuthLimiter::record_failure` never inserts past
/// it, so `entries.len() <= MAX_TRACKED_IPS` holds at every exit -- pruning
/// lapsed windows is an optimisation that makes room, not the bound itself.
/// Bounds memory on a public endpoint without a background task or a cache
/// dependency.
const MAX_TRACKED_IPS: usize = 4096;

/// A configured API token. Non-empty by construction, so "is a token
/// configured?" is answered by `Option<ApiToken>` alone and no code below
/// has to re-check for the empty string. Deliberately does not implement
/// `Debug`/`Display`: the value should never reach a log line.
#[derive(Clone)]
pub struct ApiToken(String);

impl ApiToken {
    /// Returns `None` for an absent OR empty variable -- both mean "not
    /// configured", and collapsing them here is what keeps the emptiness
    /// check from being scattered through request handling.
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        (!raw.is_empty()).then_some(Self(raw))
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    /// Length of the configured token, for the startup check in `main` that
    /// enforces a minimum before any non-loopback listener binds. Exposed
    /// instead of the value itself so the secret has no accessor that could
    /// end up in a log line or an error message.
    #[allow(clippy::len_without_is_empty)] // non-empty by construction
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Clone)]
pub struct Access {
    token: Option<ApiToken>,
    auth: Arc<AuthLimiter>,
    slots: Arc<tokio::sync::Semaphore>,
    budget: Arc<Mutex<Budget>>,
    expensive_budget: Arc<Mutex<Budget>>,
}

impl Access {
    /// `auth` is shared with the WebSocket listener so a guesser cannot get a
    /// fresh allowance simply by switching protocol.
    pub fn from_env(auth: Arc<AuthLimiter>) -> Self {
        Self {
            token: configured_token(),
            auth,
            slots: Arc::new(tokio::sync::Semaphore::new(4)),
            budget: Arc::new(Mutex::new(Budget::new())),
            expensive_budget: Arc::new(Mutex::new(Budget::new())),
        }
    }
}

pub fn configured_token() -> Option<ApiToken> {
    std::env::var("STOCKSPOTTER_API_TOKEN")
        .ok()
        .and_then(ApiToken::new)
}

/// Locking that survives a poisoned mutex.
///
/// All state behind these locks is disposable operational counting, not
/// integrity-critical data. `lock().unwrap()` would turn a single panic
/// anywhere under one of them into a *permanent* outage: every subsequent
/// request would panic on the poison, the API would return errors until
/// someone restarted the process, and a plain TCP health check would still
/// look fine throughout. Recovering the inner value is strictly better here.
fn lock_recover<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------
// Client identity
// ---------------------------------------------------------------------

/// A single trusted-proxy entry: a bare address, or a CIDR block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cidr {
    base: IpAddr,
    prefix: u8,
}

impl Cidr {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        match raw.split_once('/') {
            None => raw.parse::<IpAddr>().ok().map(|base| {
                let prefix = if base.is_ipv4() { 32 } else { 128 };
                Self { base, prefix }
            }),
            Some((addr, len)) => {
                let base: IpAddr = addr.trim().parse().ok()?;
                let prefix: u8 = len.trim().parse().ok()?;
                let max = if base.is_ipv4() { 32 } else { 128 };
                (prefix <= max).then_some(Self { base, prefix })
            }
        }
    }

    fn contains(&self, ip: IpAddr) -> bool {
        fn bits(ip: IpAddr) -> Option<u128> {
            match ip {
                IpAddr::V4(v4) => Some(u32::from(v4) as u128),
                IpAddr::V6(v6) => Some(u128::from(v6)),
            }
        }
        if self.base.is_ipv4() != ip.is_ipv4() {
            return false;
        }
        let (Some(base), Some(candidate)) = (bits(self.base), bits(ip)) else {
            return false;
        };
        let width: u32 = if self.base.is_ipv4() { 32 } else { 128 };
        if self.prefix == 0 {
            return true;
        }
        let shift = width - self.prefix as u32;
        (base >> shift) == (candidate >> shift)
    }
}

/// Deployment-supplied trust list, e.g.
/// `STOCKSPOTTER_TRUSTED_PROXIES=172.20.0.1` or `10.0.0.0/8,::1`.
///
/// When set it **replaces** the default heuristic entirely, which is the
/// narrow, stable boundary this code would prefer to use. It is opt-in
/// because the checked-in deployment cannot currently supply one: the VPS
/// compose file declares no explicit network, so Docker assigns the bridge
/// subnet dynamically and the gateway address is not knowable from
/// configuration. Pinning a subnet there (or setting this variable to the
/// observed gateway) would let the fallback below be dropped.
fn configured_trusted_proxies() -> Option<&'static Vec<Cidr>> {
    static PARSED: std::sync::OnceLock<Option<Vec<Cidr>>> = std::sync::OnceLock::new();
    PARSED
        .get_or_init(|| {
            let raw = std::env::var("STOCKSPOTTER_TRUSTED_PROXIES").ok()?;
            let entries: Vec<Cidr> = raw.split(',').filter_map(Cidr::parse).collect();
            (!entries.is_empty()).then_some(entries)
        })
        .as_ref()
}

/// Whether a peer address may be believed when it claims to be forwarding
/// for someone else.
///
/// With `STOCKSPOTTER_TRUSTED_PROXIES` set, exactly those entries are trusted
/// and nothing else. Unset, the fallback trusts loopback plus private/ULA/
/// link-local ranges -- broader than ideal, and used only because the
/// deployment cannot name a stable proxy address today (see
/// `configured_trusted_proxies`).
///
/// Why the fallback cannot simply be loopback: the container publishes its
/// ports as `127.0.0.1:8788:8788`, so the only route in is the host's own
/// proxy -- but Docker rewrites the source address on the way, and the
/// container sees the bridge gateway (observed in production as
/// `172.20.0.x`), never `127.0.0.1`. Trusting loopback alone would collapse
/// *every* client into one bucket and make the per-IP limiter useless.
///
/// The safety of the fallback rests entirely on that loopback-only
/// publishing. **If these ports were ever published on `0.0.0.0`, a LAN peer
/// could forge `X-Forwarded-For` and evade per-IP limiting** -- at which
/// point `STOCKSPOTTER_TRUSTED_PROXIES` must be set to the real proxy
/// address, and this fallback is no longer safe.
fn is_trusted_proxy(ip: IpAddr) -> bool {
    if let Some(configured) = configured_trusted_proxies() {
        return configured.iter().any(|cidr| cidr.contains(ip));
    }
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                // Unique-local (fc00::/7) and link-local (fe80::/10).
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// The IP a rate-limiting decision should be keyed by.
///
/// Anything arriving straight from an untrusted peer is keyed by its real TCP
/// source and its forwarded-for headers are ignored entirely -- otherwise an
/// attacker would rotate a header value and get a fresh allowance per request,
/// which is worse than having no limiter at all.
///
/// Behind a trusted proxy the **rightmost** `X-Forwarded-For` entry is used.
///
/// Note on why, because an earlier version of this comment had it wrong:
/// Caddy does **not** append to a client-supplied value by default. Its
/// documentation states it "will ignore their values from incoming requests,
/// to prevent spoofing", and `trusted_proxies` -- which would enable
/// appending -- is not configured in `ops/vps/Caddyfile.snippet`. So through
/// this deployment the header should arrive carrying exactly one entry: the
/// address Caddy itself observed. Rightmost and leftmost are then identical.
///
/// Rightmost is retained anyway because it is the choice that stays correct
/// if that assumption stops holding -- an older Caddy that appends, or a
/// `trusted_proxies` line added later -- whereas leftmost would silently
/// start reading attacker-controlled data. The repository pins no Caddy
/// version, so the option that is safe under both behaviours is the correct
/// engineering choice. If a real proxy *chain* is ever put in front (a CDN
/// ahead of Caddy), this must be revisited: rightmost would then name the
/// CDN rather than the client.
pub fn effective_client_ip(peer: IpAddr, headers: &HeaderMap) -> IpAddr {
    if !is_trusted_proxy(peer) {
        return peer;
    }
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .rsplit(',')
                .map(str::trim)
                .find(|entry| !entry.is_empty())
                .and_then(parse_forwarded_addr)
        })
        .unwrap_or(peer)
}

/// Accepts a bare address or one carrying a port, in v4 or v6 form. A value
/// we cannot parse is not guessed at -- the caller falls back to the peer.
fn parse_forwarded_addr(raw: &str) -> Option<IpAddr> {
    raw.parse::<IpAddr>()
        .ok()
        .or_else(|| raw.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
        .or_else(|| raw.trim_matches(['[', ']']).parse::<IpAddr>().ok())
}

// ---------------------------------------------------------------------
// Authentication-abuse limiting (per client IP)
// ---------------------------------------------------------------------

struct Attempts {
    window_started: Instant,
    failures: u32,
}

impl Attempts {
    fn new() -> Self {
        Self { window_started: Instant::now(), failures: 0 }
    }
}

/// Per-IP budget for *failed* credential attempts.
///
/// Deliberately separate from the workload budget below. Batch 1 charged
/// authentication attempts to the shared process-wide budget, which closed
/// the guessing oracle but let one hostile IP spend everyone's allowance.
/// Splitting them means a guesser exhausts only their own.
///
/// Successful authentications are never counted, and never reset an existing
/// failure count. Not resetting is the safer of the two options: a reset on
/// success would let anyone holding one valid credential clear the record
/// between guessing bursts, turning the limiter into a formality. The window
/// expiring on its own is the only way back.
pub struct AuthLimiter {
    window: Duration,
    max_failures: u32,
    state: Mutex<LimiterState>,
}

#[derive(Default)]
struct LimiterState {
    entries: HashMap<IpAddr, Attempts>,
    /// Set when the map is full of *still-active* entries and a previously
    /// unseen IP failed authentication. See `record_failure`.
    saturated_until: Option<Instant>,
}

impl Default for AuthLimiter {
    fn default() -> Self {
        Self::with_policy(AUTH_WINDOW, AUTH_FAILURES_PER_WINDOW)
    }
}

impl AuthLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Short windows make the lifecycle testable without a real 60s wait.
    pub fn with_policy(window: Duration, max_failures: u32) -> Self {
        Self { window, max_failures, state: Mutex::new(LimiterState::default()) }
    }

    /// `None` when the IP may attempt authentication; `Some(seconds)` when it
    /// is throttled, carrying roughly how long until its window resets.
    pub fn retry_after(&self, ip: IpAddr) -> Option<u64> {
        let state = lock_recover(&self.state);
        if let Some(attempts) = state.entries.get(&ip) {
            let elapsed = attempts.window_started.elapsed();
            if elapsed >= self.window || attempts.failures < self.max_failures {
                return None;
            }
            // Round up so a caller never retries fractionally early.
            return Some((self.window - elapsed).as_secs() + 1);
        }
        // Unknown IP. Normally fine -- but while the limiter is saturated we
        // are unable to track anyone new, so admitting unknown IPs would hand
        // an attacker unlimited guesses simply by rotating source addresses.
        // Refuse conservatively until the saturation lapses.
        let until = state.saturated_until?;
        let now = Instant::now();
        (now < until).then(|| (until - now).as_secs() + 1)
    }

    /// Charges one failed credential attempt to `ip`.
    ///
    /// The map is hard-bounded at `MAX_TRACKED_IPS`: an entry is only ever
    /// inserted when there is room *after* pruning lapsed windows, so the
    /// invariant `entries.len() <= MAX_TRACKED_IPS` holds at every exit.
    ///
    /// At capacity with every tracked window still active, a previously
    /// unseen IP is deliberately **not** allocated an entry. Evicting an
    /// active entry to make room would be worse: rotating source addresses
    /// would erase an attacker's accumulated throttling, which is precisely
    /// the abuse this limiter exists to stop. Instead the limiter records
    /// that it is saturated, and `retry_after` treats unknown IPs as
    /// throttled until that lapses.
    pub fn record_failure(&self, ip: IpAddr) {
        let mut state = lock_recover(&self.state);
        let window = self.window;

        // An already-tracked IP is always charged, saturated or not.
        if let Some(attempts) = state.entries.get_mut(&ip) {
            if attempts.window_started.elapsed() >= window {
                *attempts = Attempts::new();
            }
            attempts.failures = attempts.failures.saturating_add(1);
            return;
        }

        if state.entries.len() >= MAX_TRACKED_IPS {
            state.entries.retain(|_, a| a.window_started.elapsed() < window);
        }

        if state.entries.len() < MAX_TRACKED_IPS {
            let mut attempts = Attempts::new();
            attempts.failures = 1;
            state.entries.insert(ip, attempts);
            return;
        }

        // Full, and nothing was prunable. Refuse to grow.
        state.saturated_until = Some(Instant::now() + window);
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        lock_recover(&self.state).entries.len()
    }

    #[cfg(test)]
    fn is_saturated(&self) -> bool {
        lock_recover(&self.state)
            .saturated_until
            .is_some_and(|until| Instant::now() < until)
    }
}

fn throttled(retry_after_secs: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after_secs.to_string())],
    )
        .into_response()
}

// ---------------------------------------------------------------------
// Authenticated workload limiting (process-wide)
// ---------------------------------------------------------------------

/// A fixed-window counter: the start of the current window, and how many
/// requests it has admitted.
struct Budget {
    window_started: Instant,
    used: u32,
}

impl Budget {
    fn new() -> Self {
        Self { window_started: Instant::now(), used: 0 }
    }

    /// Admits a request if the window has room, rolling the window over
    /// first when it has expired. Returns false once `max` is reached.
    fn admit(&mut self, max: u32) -> bool {
        if self.window_started.elapsed() >= Duration::from_secs(60) {
            *self = Self::new();
        }
        if self.used >= max {
            return false;
        }
        self.used += 1;
        true
    }
}

fn admit(budget: &Mutex<Budget>, max: u32) -> bool {
    lock_recover(budget).admit(max)
}

/// Whether `header` carries the configured bearer credential.
///
/// Fails closed: with no token configured NOTHING is authorized. An earlier
/// version returned `true` in that case and leaned on outer guards (the
/// server refuses to boot without `STOCKSPOTTER_API_TOKEN`, and the deploy
/// script asserts a >=32 character value) -- but the innermost
/// access-control decision should not depend on a caller elsewhere having
/// got its configuration right.
pub fn authorized(header: Option<&str>, token: Option<&ApiToken>) -> bool {
    let Some(token) = token else {
        return false;
    };
    let Some(value) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    let token = token.as_str();
    // Constant work for equal-length credentials.
    value.len() == token.len()
        && value
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

pub async fn protect(
    State(access): State<Access>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let ip = effective_client_ip(peer.ip(), req.headers());

    // 1. Is this IP already being throttled for credential abuse? Answered
    //    before the credential is even examined, so a throttled guesser
    //    learns nothing from the response beyond "not now".
    if let Some(retry_after) = access.auth.retry_after(ip) {
        return throttled(retry_after);
    }

    // 2. Validate, and charge failures to this IP alone. This is what stops
    //    /health being an unlimited oracle -- and unlike Batch 1's temporary
    //    fix, an attacker now exhausts only their own allowance rather than
    //    the shared workload budget everyone depends on.
    if !authorized(
        req.headers()
            .get("authorization")
            .and_then(|h| h.to_str().ok()),
        access.token.as_ref(),
    ) {
        access.auth.record_failure(ip);
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // 3. /health is a cheap liveness and credential check that every client
    //    calls on launch. Now that credential guessing is metered per IP, it
    //    no longer needs to consume shared workload capacity at all -- and
    //    charging it there (Batch 1) is precisely what made a flood able to
    //    starve legitimate clients.
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }

    // 4. Authenticated workload policy, unchanged.
    if !admit(&access.budget, REQUESTS_PER_MINUTE) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    if matches!(req.uri().path(), "/assess" | "/movers/gainers") || req.uri().path().starts_with("/replay/signals/") {
        if !admit(&access.expensive_budget, EXPENSIVE_REQUESTS_PER_MINUTE) {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
    }
    let Ok(_permit) = access.slots.try_acquire() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    match tokio::time::timeout(Duration::from_secs(120), next.run(req)).await {
        Ok(response) => response,
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        map
    }

    // ----- token / fail-closed (M5, preserved) -----

    #[test]
    fn requires_exact_bearer_credential_when_configured() {
        let configured = ApiToken::new("secret");
        assert!(!authorized(None, configured.as_ref()));
        assert!(!authorized(Some("Bearer wrong"), configured.as_ref()));
        assert!(authorized(Some("Bearer secret"), configured.as_ref()));
    }

    #[test]
    fn fails_closed_when_no_token_is_configured() {
        assert!(!authorized(None, None));
        assert!(!authorized(Some("Bearer anything"), None));
        assert!(!authorized(Some("Bearer "), None));
    }

    #[test]
    fn an_empty_token_is_not_a_configured_token() {
        assert!(ApiToken::new("").is_none());
        assert!(ApiToken::new("x").is_some());
    }

    // ----- client IP derivation -----

    #[test]
    fn an_untrusted_peer_cannot_spoof_its_identity() {
        let spoofed = headers(&[("x-forwarded-for", "1.2.3.4")]);
        assert_eq!(
            effective_client_ip(ip("203.0.113.9"), &spoofed),
            ip("203.0.113.9"),
            "a direct public peer's forwarded header must be ignored entirely"
        );
    }

    #[test]
    fn a_trusted_proxy_may_supply_the_client_ip() {
        let forwarded = headers(&[("x-forwarded-for", "198.51.100.7")]);
        assert_eq!(effective_client_ip(ip("127.0.0.1"), &forwarded), ip("198.51.100.7"));
        assert_eq!(effective_client_ip(ip("::1"), &forwarded), ip("198.51.100.7"));
        // The address Docker actually presents in this deployment.
        assert_eq!(effective_client_ip(ip("172.20.0.1"), &forwarded), ip("198.51.100.7"));
    }

    #[test]
    fn the_rightmost_forwarded_entry_wins() {
        // Caddy appends the address it saw, so a client forging a value ends
        // up on the LEFT. Trusting the leftmost would hand out a fresh
        // allowance per forged header.
        let forged = headers(&[("x-forwarded-for", "1.2.3.4, 198.51.100.7")]);
        assert_eq!(effective_client_ip(ip("127.0.0.1"), &forged), ip("198.51.100.7"));
    }

    #[test]
    fn a_malformed_forwarded_value_falls_back_to_the_peer() {
        for bad in ["not-an-ip", "", " , ", "999.999.999.999"] {
            let h = headers(&[("x-forwarded-for", bad)]);
            assert_eq!(effective_client_ip(ip("127.0.0.1"), &h), ip("127.0.0.1"), "value: {bad:?}");
        }
        assert_eq!(effective_client_ip(ip("127.0.0.1"), &HeaderMap::new()), ip("127.0.0.1"));
    }

    #[test]
    fn forwarded_entries_carrying_a_port_are_understood() {
        let v4 = headers(&[("x-forwarded-for", "198.51.100.7:44321")]);
        assert_eq!(effective_client_ip(ip("127.0.0.1"), &v4), ip("198.51.100.7"));
        let v6 = headers(&[("x-forwarded-for", "[2001:db8::1]:44321")]);
        assert_eq!(effective_client_ip(ip("127.0.0.1"), &v6), ip("2001:db8::1"));
    }

    #[test]
    fn the_shape_caddy_actually_forwards_resolves_to_the_real_client() {
        // Caddy ignores client-supplied X-Forwarded-* by default and sets a
        // single entry: the address it observed. This is the realistic
        // production shape, not a multi-hop chain.
        let caddy = headers(&[("x-forwarded-for", "198.51.100.7")]);
        assert_eq!(effective_client_ip(ip("172.20.0.1"), &caddy), ip("198.51.100.7"));
    }

    #[test]
    fn an_explicit_trusted_proxy_list_parses_addresses_and_cidrs() {
        assert!(Cidr::parse("172.20.0.1").unwrap().contains(ip("172.20.0.1")));
        assert!(!Cidr::parse("172.20.0.1").unwrap().contains(ip("172.20.0.2")));

        let block = Cidr::parse("172.20.0.0/16").unwrap();
        assert!(block.contains(ip("172.20.5.9")));
        assert!(!block.contains(ip("172.21.0.1")));

        let v6 = Cidr::parse("fd00::/8").unwrap();
        assert!(v6.contains(ip("fd00::1")));
        assert!(!v6.contains(ip("fe80::1")));

        // Families never cross-match, and junk is rejected rather than guessed.
        assert!(!Cidr::parse("10.0.0.0/8").unwrap().contains(ip("fd00::1")));
        assert!(Cidr::parse("not-an-ip").is_none());
        assert!(Cidr::parse("10.0.0.0/99").is_none());
    }

    #[test]
    fn loopback_and_private_ranges_are_trusted_but_public_ones_are_not() {
        assert!(is_trusted_proxy(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_trusted_proxy(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_trusted_proxy(ip("10.1.2.3")));
        assert!(is_trusted_proxy(ip("172.20.0.1")));
        assert!(is_trusted_proxy(ip("192.168.1.1")));
        assert!(is_trusted_proxy(ip("fd00::1")));
        assert!(!is_trusted_proxy(ip("8.8.8.8")));
        assert!(!is_trusted_proxy(ip("2001:db8::1")));
    }

    // ----- per-IP auth limiter -----

    #[test]
    fn an_ip_is_throttled_only_after_exhausting_its_own_allowance() {
        let limiter = AuthLimiter::with_policy(Duration::from_secs(60), 3);
        let attacker = ip("203.0.113.5");
        for _ in 0..3 {
            assert!(limiter.retry_after(attacker).is_none());
            limiter.record_failure(attacker);
        }
        assert!(limiter.retry_after(attacker).is_some());
    }

    #[test]
    fn one_hostile_ip_does_not_throttle_anyone_else() {
        let limiter = AuthLimiter::with_policy(Duration::from_secs(60), 2);
        let attacker = ip("203.0.113.5");
        let bystander = ip("198.51.100.7");
        for _ in 0..10 {
            limiter.record_failure(attacker);
        }
        assert!(limiter.retry_after(attacker).is_some());
        assert!(
            limiter.retry_after(bystander).is_none(),
            "a second IP must keep its own independent allowance"
        );
    }

    #[test]
    fn retry_after_reports_a_positive_whole_number_of_seconds() {
        let limiter = AuthLimiter::with_policy(Duration::from_secs(60), 1);
        let attacker = ip("203.0.113.5");
        limiter.record_failure(attacker);
        let retry = limiter.retry_after(attacker).expect("should be throttled");
        assert!((1..=61).contains(&retry), "unexpected retry-after: {retry}");
    }

    #[test]
    fn an_expired_window_becomes_usable_again() {
        let limiter = AuthLimiter::with_policy(Duration::from_millis(40), 1);
        let attacker = ip("203.0.113.5");
        limiter.record_failure(attacker);
        assert!(limiter.retry_after(attacker).is_some());
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            limiter.retry_after(attacker).is_none(),
            "the window must lapse on its own -- no permanent ban"
        );
    }

    #[test]
    fn expired_entries_are_pruned_so_the_map_does_not_creep() {
        let limiter = AuthLimiter::with_policy(Duration::from_millis(1), 1);
        for n in 0..(MAX_TRACKED_IPS + 64) {
            limiter.record_failure(IpAddr::V4(Ipv4Addr::from(n as u32)));
        }
        assert!(limiter.tracked() <= MAX_TRACKED_IPS, "saw {}", limiter.tracked());
    }

    /// The worst case the pruning path alone does NOT cover: every tracked
    /// window still active, so nothing is prunable, and thousands more
    /// distinct addresses arrive. The map must not grow by even one entry.
    #[test]
    fn the_map_is_hard_bounded_even_when_no_entry_can_be_pruned() {
        let limiter = AuthLimiter::with_policy(Duration::from_secs(600), 1);
        // Fill to capacity with entries that cannot expire during the test.
        for n in 0..MAX_TRACKED_IPS {
            limiter.record_failure(IpAddr::V4(Ipv4Addr::from(n as u32)));
        }
        assert_eq!(limiter.tracked(), MAX_TRACKED_IPS);

        // Substantially more than the cap, all previously unseen.
        for n in 0..10_000u32 {
            limiter.record_failure(IpAddr::V4(Ipv4Addr::from(u32::MAX - n)));
            assert!(
                limiter.tracked() <= MAX_TRACKED_IPS,
                "bound violated at iteration {n}: {}",
                limiter.tracked()
            );
        }
        assert_eq!(limiter.tracked(), MAX_TRACKED_IPS);
    }

    #[test]
    fn an_already_tracked_ip_stays_throttled_while_saturated() {
        let limiter = AuthLimiter::with_policy(Duration::from_secs(600), 1);
        let known = IpAddr::V4(Ipv4Addr::from(7u32));
        limiter.record_failure(known);
        assert!(limiter.retry_after(known).is_some());

        for n in 0..MAX_TRACKED_IPS {
            limiter.record_failure(IpAddr::V4(Ipv4Addr::from(1000 + n as u32)));
        }
        assert!(limiter.is_saturated(), "expected the limiter to report saturation");
        assert!(
            limiter.retry_after(known).is_some(),
            "saturation must never release an already-throttled IP"
        );
    }

    #[test]
    fn saturation_does_not_hand_a_new_ip_unlimited_guesses() {
        let limiter = AuthLimiter::with_policy(Duration::from_secs(600), 1);
        for n in 0..(MAX_TRACKED_IPS + 500) {
            limiter.record_failure(IpAddr::V4(Ipv4Addr::from(n as u32)));
        }
        assert!(limiter.is_saturated());
        let untracked: IpAddr = "203.0.113.200".parse().unwrap();
        assert!(
            limiter.retry_after(untracked).is_some(),
            "an untrackable IP must be refused, not admitted, while saturated"
        );
    }

    #[test]
    fn saturation_lapses_and_normal_admission_resumes() {
        let limiter = AuthLimiter::with_policy(Duration::from_millis(40), 1);
        for n in 0..(MAX_TRACKED_IPS + 100) {
            limiter.record_failure(IpAddr::V4(Ipv4Addr::from(n as u32)));
        }
        let fresh: IpAddr = "203.0.113.201".parse().unwrap();
        std::thread::sleep(Duration::from_millis(60));
        assert!(!limiter.is_saturated(), "saturation must be time-bounded, not permanent");
        assert!(
            limiter.retry_after(fresh).is_none(),
            "a new client must be admitted again once saturation lapses"
        );
    }

    #[test]
    fn a_poisoned_limiter_still_answers(  ) {
        let limiter = Arc::new(AuthLimiter::with_policy(Duration::from_secs(60), 2));
        let poisoner = limiter.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.state.lock().unwrap();
            panic!("poison the auth limiter");
        })
        .join();
        assert!(limiter.state.lock().is_err(), "expected the mutex to be poisoned");
        let addr = ip("203.0.113.5");
        limiter.record_failure(addr);
        limiter.record_failure(addr);
        assert!(limiter.retry_after(addr).is_some());
    }

    // ----- workload budget (M3, preserved) -----

    #[test]
    fn a_poisoned_budget_still_admits_requests() {
        let budget = Arc::new(Mutex::new(Budget::new()));
        let poisoner = budget.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the budget mutex");
        })
        .join();
        assert!(budget.lock().is_err(), "expected the mutex to be poisoned");
        assert!(admit(&budget, REQUESTS_PER_MINUTE));
        assert!(admit(&budget, REQUESTS_PER_MINUTE));
    }

    #[test]
    fn a_budget_refuses_once_its_window_is_exhausted() {
        let budget = Mutex::new(Budget::new());
        for _ in 0..3 {
            assert!(admit(&budget, 3));
        }
        assert!(!admit(&budget, 3));
    }

    // ----- end-to-end HTTP -----

    fn access_with(token_value: &str, auth: Arc<AuthLimiter>) -> Access {
        Access {
            token: ApiToken::new(token_value),
            auth,
            slots: Arc::new(tokio::sync::Semaphore::new(4)),
            budget: Arc::new(Mutex::new(Budget::new())),
            expensive_budget: Arc::new(Mutex::new(Budget::new())),
        }
    }

    async fn serve(access: Access) -> (String, tokio::task::JoinHandle<()>) {
        let app = axum::Router::new()
            .route("/test", axum::routing::get(|| async { "ok" }))
            .route("/health", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(access, protect));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        (base, server)
    }

    /// The test client connects over loopback, which is a trusted proxy
    /// address -- so `X-Forwarded-For` lets each request pretend to originate
    /// from a distinct client, exactly as Caddy would supply in production.
    fn as_client(
        client: &reqwest::Client,
        url: &str,
        from: &str,
    ) -> reqwest::RequestBuilder {
        client.get(url).header("x-forwarded-for", from)
    }

    #[tokio::test]
    async fn valid_credentials_still_work_and_workload_limits_still_apply() {
        let auth = Arc::new(AuthLimiter::new());
        let access = access_with("test-secret", auth);
        let (base, server) = serve(access.clone()).await;
        let url = format!("{base}/test");
        let client = reqwest::Client::new();

        assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
        assert_eq!(
            client.get(&url).bearer_auth("test-secret").send().await.unwrap().status(),
            200
        );
        let permits = access.slots.acquire_many(4).await.unwrap();
        assert_eq!(
            client.get(&url).bearer_auth("test-secret").send().await.unwrap().status(),
            429
        );
        drop(permits);
        lock_recover(&access.budget).used = REQUESTS_PER_MINUTE;
        assert_eq!(
            client.get(&url).bearer_auth("test-secret").send().await.unwrap().status(),
            429
        );
        server.abort();
    }

    #[tokio::test]
    async fn health_cannot_be_guessed_indefinitely() {
        let auth = Arc::new(AuthLimiter::with_policy(Duration::from_secs(60), 3));
        let access = access_with("test-secret", auth);
        let (base, server) = serve(access).await;
        let url = format!("{base}/health");
        let client = reqwest::Client::new();
        let attacker = "203.0.113.5";

        for n in 0..3 {
            let status = as_client(&client, &url, attacker)
                .bearer_auth(format!("guess-{n}"))
                .send()
                .await
                .unwrap()
                .status();
            assert_eq!(status, 401, "guess {n} should be a plain rejection");
        }
        let response = as_client(&client, &url, attacker)
            .bearer_auth("guess-4")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 429, "guessing must become throttled");
        let retry = response
            .headers()
            .get("retry-after")
            .expect("Retry-After should be present on an auth throttle");
        assert!(retry.to_str().unwrap().parse::<u64>().unwrap() > 0);
        server.abort();
    }

    #[tokio::test]
    async fn one_attacker_cannot_lock_out_another_client() {
        // The Batch 1 weakness this batch exists to remove: guessing used to
        // spend the shared budget, so a flood denied service to everyone.
        let auth = Arc::new(AuthLimiter::with_policy(Duration::from_secs(60), 2));
        let access = access_with("test-secret", auth);
        let (base, server) = serve(access).await;
        let health = format!("{base}/health");
        let client = reqwest::Client::new();

        for n in 0..6 {
            as_client(&client, &health, "203.0.113.5")
                .bearer_auth(format!("guess-{n}"))
                .send()
                .await
                .unwrap();
        }
        assert_eq!(
            as_client(&client, &health, "203.0.113.5").bearer_auth("again").send().await.unwrap().status(),
            429
        );
        assert_eq!(
            as_client(&client, &health, "198.51.100.7").bearer_auth("test-secret").send().await.unwrap().status(),
            200,
            "an unrelated client must be unaffected by someone else's guessing"
        );
        server.abort();
    }

    #[tokio::test]
    async fn successful_health_checks_do_not_consume_workload_capacity() {
        // Batch 1 charged /health to the global budget; that is superseded.
        let auth = Arc::new(AuthLimiter::new());
        let access = access_with("test-secret", auth);
        let (base, server) = serve(access.clone()).await;
        let client = reqwest::Client::new();

        for _ in 0..25 {
            assert_eq!(
                client
                    .get(format!("{base}/health"))
                    .bearer_auth("test-secret")
                    .send()
                    .await
                    .unwrap()
                    .status(),
                200
            );
        }
        assert_eq!(
            lock_recover(&access.budget).used,
            0,
            "/health must no longer draw down the shared workload budget"
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_successful_authentication_is_never_counted_as_a_failure() {
        let auth = Arc::new(AuthLimiter::with_policy(Duration::from_secs(60), 2));
        let access = access_with("test-secret", auth.clone());
        let (base, server) = serve(access).await;
        let client = reqwest::Client::new();
        for _ in 0..10 {
            as_client(&client, &format!("{base}/health"), "198.51.100.7")
                .bearer_auth("test-secret")
                .send()
                .await
                .unwrap();
        }
        assert!(auth.retry_after(ip("198.51.100.7")).is_none());
        server.abort();
    }
}
