// Copyright (C) 2026 Gane — SPDX-License-Identifier: GPL-3.0-only
//! In-memory sliding-window rate limiter for the login endpoint
//! (task 03-SEC-03).
//!
//! Brute-force mitigation: every `POST /api/v1/auth/login` attempt —
//! successful or not — consumes from two buckets, one keyed by client IP
//! and one by normalized email. Counting successes too keeps the limiter
//! from becoming a user-enumeration oracle (an attacker cannot learn
//! which emails exist by watching which bucket trips).
//!
//! ## Failure mode
//!
//! Fail-closed: if the limiter's internal lock is poisoned, attempts are
//! rejected with a `429` rather than bypassing the limit. A limiter bug
//! must never become an authentication bypass.
//!
//! ## Scope
//!
//! The limiter is per-process and in-memory, so it is exact only for a
//! single server instance. That matches the documented deployment shape
//! (one `crm-api` process); a multi-instance deployment needs a shared
//! store, which is called out in `docs/operations/threat-model.md`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crm_domain::audit::RateLimitBucket;

/// Tuning knobs for login rate limiting.
///
/// [`Default`] carries the production values; tests use
/// [`LoginRateLimitConfig::for_tests`] for fast-tripping limits.
#[derive(Debug, Clone, Copy)]
pub struct LoginRateLimitConfig {
    /// Maximum login attempts per client IP within [`Self::ip_window`].
    pub max_attempts_per_ip: u32,
    /// Sliding window for the per-IP bucket.
    pub ip_window: Duration,
    /// Maximum login attempts per normalized email within
    /// [`Self::email_window`].
    pub max_attempts_per_email: u32,
    /// Sliding window for the per-email bucket.
    pub email_window: Duration,
}

impl Default for LoginRateLimitConfig {
    fn default() -> Self {
        Self {
            max_attempts_per_ip: 30,
            ip_window: Duration::from_secs(60),
            max_attempts_per_email: 10,
            email_window: Duration::from_secs(300),
        }
    }
}

impl LoginRateLimitConfig {
    /// Builds the configuration from environment variables, falling back
    /// to the production defaults (task 04-AUD-03).
    ///
    /// | Variable | Default | Meaning |
    /// |---|---|---|
    /// | `CRM_RATE_LIMIT_IP_MAX` | `30` | Max attempts per IP per window |
    /// | `CRM_RATE_LIMIT_IP_WINDOW_SECS` | `60` | Per-IP window (seconds) |
    /// | `CRM_RATE_LIMIT_EMAIL_MAX` | `10` | Max attempts per email per window |
    /// | `CRM_RATE_LIMIT_EMAIL_WINDOW_SECS` | `300` | Per-email window (seconds) |
    ///
    /// Unparseable values fall back to the default (with no panic — a
    /// typo in an env var must not prevent startup).
    pub fn from_env() -> Self {
        Self {
            max_attempts_per_ip: env_u32("CRM_RATE_LIMIT_IP_MAX", 30),
            ip_window: Duration::from_secs(env_u64("CRM_RATE_LIMIT_IP_WINDOW_SECS", 60)),
            max_attempts_per_email: env_u32("CRM_RATE_LIMIT_EMAIL_MAX", 10),
            email_window: Duration::from_secs(env_u64("CRM_RATE_LIMIT_EMAIL_WINDOW_SECS", 300)),
        }
    }

    /// Tiny limits for tests: trips after 3 attempts per IP within a
    /// minute, so HTTP tests exercise the 429 path without burning CPU
    /// on dozens of Argon2 verifications.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            max_attempts_per_ip: 3,
            ip_window: Duration::from_secs(60),
            max_attempts_per_email: 100,
            email_window: Duration::from_secs(60),
        }
    }
}

/// Reads a `u32` env var, falling back to `default` when unset or
/// unparseable.
fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(default)
}

/// Reads a `u64` env var, falling back to `default` when unset or
/// unparseable.
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(default)
}

/// Attempt timestamps per bucket key.
#[derive(Debug, Default)]
struct LimiterState {
    by_ip: HashMap<String, VecDeque<Instant>>,
    by_email: HashMap<String, VecDeque<Instant>>,
}

/// In-memory sliding-window login rate limiter.
///
/// Share via [`std::sync::Arc`]; the interior is lock-protected.
#[derive(Debug)]
pub struct LoginRateLimiter {
    config: LoginRateLimitConfig,
    state: Mutex<LimiterState>,
}

impl LoginRateLimiter {
    /// Builds a limiter with the given configuration.
    pub fn new(config: LoginRateLimitConfig) -> Self {
        Self {
            config,
            state: Mutex::new(LimiterState::default()),
        }
    }

    /// Returns the tuning configuration (for audit/anomaly reporting).
    pub fn config(&self) -> LoginRateLimitConfig {
        self.config
    }

    /// Records a login attempt from `ip_key` for `email_key`.
    ///
    /// Returns `Ok(())` and records the attempt when both buckets have
    /// capacity, or `Err((bucket, retry_after_secs))` naming the tripped
    /// bucket and the sooner of the two expiries when either bucket is
    /// exhausted. Rejected attempts are not recorded.
    ///
    /// Fail-closed: a poisoned lock rejects with a 60-second retry hint
    /// instead of bypassing the limit.
    pub fn check(&self, ip_key: &str, email_key: &str) -> Result<(), (RateLimitBucket, u64)> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| (RateLimitBucket::Ip, 60u64))?;
        let now = Instant::now();
        let ip_wait = bucket_wait(
            &mut state.by_ip,
            ip_key,
            now,
            self.config.max_attempts_per_ip,
            self.config.ip_window,
        );
        let email_wait = bucket_wait(
            &mut state.by_email,
            email_key,
            now,
            self.config.max_attempts_per_email,
            self.config.email_window,
        );
        match (ip_wait, email_wait) {
            (Ok(()), Ok(())) => {
                record(&mut state.by_ip, ip_key, now);
                record(&mut state.by_email, email_key, now);
                Ok(())
            }
            (Err(a), Err(b)) => Err(if a <= b {
                (RateLimitBucket::Ip, a)
            } else {
                (RateLimitBucket::Email, b)
            }),
            (Err(wait), _) => Err((RateLimitBucket::Ip, wait)),
            (_, Err(wait)) => Err((RateLimitBucket::Email, wait)),
        }
    }
}

/// Seconds until the oldest entry in the bucket expires, or `Ok(())`
/// when the bucket has capacity. Expired entries are pruned.
fn bucket_wait(
    buckets: &mut HashMap<String, VecDeque<Instant>>,
    key: &str,
    now: Instant,
    max: u32,
    window: Duration,
) -> Result<(), u64> {
    let bucket = buckets.entry(key.to_owned()).or_default();
    while bucket
        .front()
        .is_some_and(|seen| now.duration_since(*seen) >= window)
    {
        bucket.pop_front();
    }
    if bucket.len() as u32 >= max {
        let wait = bucket
            .front()
            .map(|seen| {
                window
                    .saturating_sub(now.duration_since(*seen))
                    .as_secs()
                    .max(1)
            })
            .unwrap_or(1);
        return Err(wait);
    }
    Ok(())
}

/// Appends an attempt timestamp to the bucket.
fn record(buckets: &mut HashMap<String, VecDeque<Instant>>, key: &str, now: Instant) {
    buckets.entry(key.to_owned()).or_default().push_back(now);
}

// ---------------------------------------------------------------------------
// Write-endpoint rate limiting (task 17-PERF-02)
// ---------------------------------------------------------------------------

/// Tuning knobs for the per-endpoint write rate limiter.
///
/// Every mutating API endpoint (`POST`/`PUT`/`PATCH`/`DELETE`) consumes
/// from a single per-client-IP sliding-window bucket. Writes are already
/// authenticated, so no email bucket is needed; the budget is generous
/// (legitimate staff bulk work) but finite (a compromised token or a
/// runaway client cannot flood the API unboundedly).
#[derive(Debug, Clone, Copy)]
pub struct WriteRateLimitConfig {
    /// Maximum write requests per client IP within [`Self::window`].
    pub max_writes_per_ip: u32,
    /// Sliding window for the per-IP write bucket.
    pub window: Duration,
}

impl Default for WriteRateLimitConfig {
    fn default() -> Self {
        Self {
            max_writes_per_ip: 300,
            window: Duration::from_secs(60),
        }
    }
}

impl WriteRateLimitConfig {
    /// Builds the configuration from environment variables, falling back
    /// to the production defaults.
    ///
    /// | Variable | Default | Meaning |
    /// |---|---|---|
    /// | `CRM_RATE_LIMIT_WRITE_MAX` | `300` | Max write requests per IP per window |
    /// | `CRM_RATE_LIMIT_WRITE_WINDOW_SECS` | `60` | Write window (seconds) |
    ///
    /// Unparseable values fall back to the default (with no panic — a
    /// typo in an env var must not prevent startup). A zero window is
    /// also treated as unset: a zero window would expire every recorded
    /// write before the capacity check, so the limiter would never trip.
    pub fn from_env() -> Self {
        let window_secs = env_u64("CRM_RATE_LIMIT_WRITE_WINDOW_SECS", 60);
        Self {
            max_writes_per_ip: env_u32("CRM_RATE_LIMIT_WRITE_MAX", 300).max(1),
            window: Duration::from_secs(if window_secs == 0 { 60 } else { window_secs }),
        }
    }

    /// Tiny limits for tests: trips after 3 writes per IP within a
    /// minute, so HTTP tests exercise the 429 path cheaply.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            max_writes_per_ip: 3,
            window: Duration::from_secs(60),
        }
    }
}

/// In-memory sliding-window rate limiter for mutating API endpoints.
///
/// Shares the bucket mechanics ([`bucket_wait`], [`record`]) with
/// [`LoginRateLimiter`]; the difference is a single per-IP bucket
/// (writes are already authenticated, so the email bucket does not
/// apply). Share via [`std::sync::Arc`]; the interior is lock-protected.
///
/// Fail-closed: a poisoned lock rejects with a 60-second retry hint
/// instead of bypassing the limit.
#[derive(Debug)]
pub struct WriteRateLimiter {
    config: WriteRateLimitConfig,
    state: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl WriteRateLimiter {
    /// Builds a limiter with the given configuration.
    pub fn new(config: WriteRateLimitConfig) -> Self {
        Self {
            config,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Records a write request from `ip_key`.
    ///
    /// Returns `Ok(())` and records the request when the bucket has
    /// capacity, or `Err(retry_after_secs)` when the bucket is exhausted.
    /// Rejected requests are not recorded.
    pub fn check(&self, ip_key: &str) -> Result<(), u64> {
        let mut state = self.state.lock().map_err(|_| 60u64)?;
        let now = Instant::now();
        match bucket_wait(
            &mut state,
            ip_key,
            now,
            self.config.max_writes_per_ip,
            self.config.window,
        ) {
            Ok(()) => {
                record(&mut state, ip_key, now);
                Ok(())
            }
            Err(wait) => Err(wait),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_config() -> LoginRateLimitConfig {
        LoginRateLimitConfig {
            max_attempts_per_ip: 2,
            ip_window: Duration::from_millis(200),
            max_attempts_per_email: 2,
            email_window: Duration::from_millis(200),
        }
    }

    #[test]
    fn allows_attempts_up_to_the_limit() {
        let limiter = LoginRateLimiter::new(tiny_config());
        assert!(limiter.check("ip", "a@example.com").is_ok());
        assert!(limiter.check("ip", "a@example.com").is_ok());
    }

    #[test]
    fn trips_when_either_bucket_is_exhausted() {
        let limiter = LoginRateLimiter::new(tiny_config());
        assert!(limiter.check("ip", "a@example.com").is_ok());
        assert!(limiter.check("ip", "a@example.com").is_ok());
        // IP bucket is full now; a different email from the same IP trips.
        let (bucket, wait) = limiter
            .check("ip", "b@example.com")
            .expect_err("third attempt from the IP must trip");
        assert_eq!(bucket, RateLimitBucket::Ip);
        assert!(wait >= 1, "retry hint must be positive");

        // A fresh IP trips on the exhausted email bucket instead.
        let limiter = LoginRateLimiter::new(tiny_config());
        assert!(limiter.check("ip-1", "a@example.com").is_ok());
        assert!(limiter.check("ip-2", "a@example.com").is_ok());
        let (bucket, _) = limiter
            .check("ip-3", "a@example.com")
            .expect_err("third attempt for the email must trip");
        assert_eq!(bucket, RateLimitBucket::Email);
    }

    #[test]
    fn buckets_are_independent() {
        let limiter = LoginRateLimiter::new(tiny_config());
        assert!(limiter.check("ip-1", "a@example.com").is_ok());
        assert!(limiter.check("ip-1", "a@example.com").is_ok());
        // A different IP/email pair is unaffected.
        assert!(limiter.check("ip-2", "b@example.com").is_ok());
    }

    #[test]
    fn window_expiry_restores_capacity() {
        let limiter = LoginRateLimiter::new(tiny_config());
        assert!(limiter.check("ip", "a@example.com").is_ok());
        assert!(limiter.check("ip", "a@example.com").is_ok());
        assert!(limiter.check("ip", "a@example.com").is_err());
        std::thread::sleep(Duration::from_millis(250));
        assert!(
            limiter.check("ip", "a@example.com").is_ok(),
            "expired entries must free capacity"
        );
    }

    #[test]
    fn retry_hint_shrinks_as_entries_expire() {
        let config = LoginRateLimitConfig {
            max_attempts_per_ip: 1,
            ip_window: Duration::from_millis(200),
            max_attempts_per_email: 100,
            email_window: Duration::from_secs(60),
        };
        let limiter = LoginRateLimiter::new(config);
        assert!(limiter.check("ip", "a@example.com").is_ok());
        let first_wait = limiter.check("ip", "a@example.com").expect_err("trips").1;
        std::thread::sleep(Duration::from_millis(150));
        let second_wait = limiter
            .check("ip", "a@example.com")
            .expect_err("still trips")
            .1;
        assert!(
            second_wait <= first_wait,
            "retry hint must not grow while the entry ages"
        );
    }

    #[test]
    fn config_from_env_reads_overrides_with_defaults() {
        // Defaults when nothing is set.
        let config = LoginRateLimitConfig::from_env();
        assert_eq!(config.max_attempts_per_ip, 30);
        assert_eq!(config.max_attempts_per_email, 10);

        std::env::set_var("CRM_RATE_LIMIT_IP_MAX", "5");
        std::env::set_var("CRM_RATE_LIMIT_EMAIL_WINDOW_SECS", "42");
        // Unparseable values fall back to the default, never panic.
        std::env::set_var("CRM_RATE_LIMIT_EMAIL_MAX", "not-a-number");
        let config = LoginRateLimitConfig::from_env();
        assert_eq!(config.max_attempts_per_ip, 5);
        assert_eq!(config.email_window, Duration::from_secs(42));
        assert_eq!(config.max_attempts_per_email, 10);
        std::env::remove_var("CRM_RATE_LIMIT_IP_MAX");
        std::env::remove_var("CRM_RATE_LIMIT_EMAIL_WINDOW_SECS");
        std::env::remove_var("CRM_RATE_LIMIT_EMAIL_MAX");
    }

    fn tiny_write_config() -> WriteRateLimitConfig {
        WriteRateLimitConfig {
            max_writes_per_ip: 2,
            window: Duration::from_millis(200),
        }
    }

    #[test]
    fn write_limiter_allows_up_to_the_limit() {
        let limiter = WriteRateLimiter::new(tiny_write_config());
        assert!(limiter.check("ip").is_ok());
        assert!(limiter.check("ip").is_ok());
    }

    #[test]
    fn write_limiter_trips_with_retry_hint() {
        let limiter = WriteRateLimiter::new(tiny_write_config());
        assert!(limiter.check("ip").is_ok());
        assert!(limiter.check("ip").is_ok());
        let wait = limiter.check("ip").expect_err("third write must trip");
        assert!(wait >= 1, "retry hint must be positive");
        // Rejected attempts are not recorded: the hint does not grow.
        let wait_again = limiter.check("ip").expect_err("still trips").max(1);
        assert!(wait_again <= wait.max(1) + 1);
    }

    #[test]
    fn write_limiter_buckets_are_per_ip() {
        let limiter = WriteRateLimiter::new(tiny_write_config());
        assert!(limiter.check("ip-1").is_ok());
        assert!(limiter.check("ip-1").is_ok());
        assert!(limiter.check("ip-1").is_err());
        // A different IP is unaffected.
        assert!(limiter.check("ip-2").is_ok());
    }

    #[test]
    fn write_limiter_window_expiry_restores_capacity() {
        let limiter = WriteRateLimiter::new(tiny_write_config());
        assert!(limiter.check("ip").is_ok());
        assert!(limiter.check("ip").is_ok());
        assert!(limiter.check("ip").is_err());
        std::thread::sleep(Duration::from_millis(250));
        assert!(
            limiter.check("ip").is_ok(),
            "expired entries must free capacity"
        );
    }

    #[test]
    fn write_config_from_env_reads_overrides_with_defaults() {
        let config = WriteRateLimitConfig::from_env();
        assert_eq!(config.max_writes_per_ip, 300);
        assert_eq!(config.window, Duration::from_secs(60));

        std::env::set_var("CRM_RATE_LIMIT_WRITE_MAX", "7");
        std::env::set_var("CRM_RATE_LIMIT_WRITE_WINDOW_SECS", "not-a-number");
        let config = WriteRateLimitConfig::from_env();
        assert_eq!(config.max_writes_per_ip, 7);
        assert_eq!(config.window, Duration::from_secs(60));
        std::env::remove_var("CRM_RATE_LIMIT_WRITE_MAX");
        std::env::remove_var("CRM_RATE_LIMIT_WRITE_WINDOW_SECS");
    }
}
