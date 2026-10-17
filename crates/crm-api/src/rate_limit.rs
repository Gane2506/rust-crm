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
}
