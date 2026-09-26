//! Retry with exponential backoff for transient provider failures.
//!
//! Only errors for which [`Error::is_retryable`] is true (rate limiting,
//! server errors, network failures) are retried. A server supplied
//! `retry_after` hint takes precedence over the computed backoff, capped at
//! [`RetryPolicy::max_delay`] so a misbehaving server cannot stall a caller
//! for hours. When attempts are exhausted the last error is returned as is,
//! `retry_after` included, so callers can decide what to do next.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vibe_core::{Error, Result};

/// How many times, and how patiently, a failing call is retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total number of attempts, including the first one. `0` behaves like `1`.
    pub max_attempts: u32,
    /// Delay before the first retry; doubled after each failure.
    pub base_delay: Duration,
    /// Upper bound for any single delay, including server `retry_after` hints.
    pub max_delay: Duration,
    /// Whether to randomise delays by up to ±25% to avoid thundering herds.
    pub jitter: bool,
}

impl Default for RetryPolicy {
    /// Four attempts, 1 s base delay, 60 s cap, jitter on.
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            jitter: true,
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries.
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// A policy retrying up to `max_attempts` times without waiting. Handy in
    /// tests.
    #[must_use]
    pub fn immediate(max_attempts: u32) -> Self {
        Self {
            max_attempts,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            jitter: false,
        }
    }

    /// Set the number of attempts.
    #[must_use]
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Backoff before retry number `retry` (1 for the first retry), without
    /// jitter: `base_delay * 2^(retry-1)`, capped at `max_delay`.
    #[must_use]
    pub fn backoff(&self, retry: u32) -> Duration {
        let exponent = retry.saturating_sub(1).min(31);
        self.base_delay
            .saturating_mul(1u32 << exponent)
            .min(self.max_delay)
    }

    /// Delay to wait before retry number `retry` after `error`.
    #[must_use]
    pub fn delay_for(&self, retry: u32, error: &Error) -> Duration {
        match error.retry_after {
            Some(hint) => hint.min(self.max_delay),
            None if self.jitter => apply_jitter(self.backoff(retry)).min(self.max_delay),
            None => self.backoff(retry),
        }
    }
}

/// Scale `delay` by a pseudo-random factor in `[0.75, 1.25)`.
fn apply_jitter(delay: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // Cheap mixing of the clock so consecutive calls differ.
    let mixed = nanos.wrapping_mul(2_654_435_761) % 1000;
    let factor = 0.75 + f64::from(mixed) / 2000.0;
    delay.mul_f64(factor)
}

/// Run `operation`, retrying retryable failures according to `policy`.
///
/// ```
/// # use vibe_providers::{with_retry, RetryPolicy};
/// # tokio_test_block_on(async {
/// let value = with_retry(&RetryPolicy::immediate(3), || async { Ok::<_, vibe_core::Error>(42) })
///     .await
///     .unwrap();
/// assert_eq!(value, 42);
/// # });
/// # fn tokio_test_block_on<F: std::future::Future>(f: F) -> F::Output {
/// #     tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
/// # }
/// ```
pub async fn with_retry<T, F, Fut>(policy: &RetryPolicy, mut operation: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let max_attempts = policy.max_attempts.max(1);
    let mut attempt = 1;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(err) if err.is_retryable() && attempt < max_attempts => {
                let delay = policy.delay_for(attempt, &err);
                tracing::warn!(
                    attempt,
                    max_attempts,
                    delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    kind = ?err.kind,
                    error = %crate::classify::scrub_secrets(&err.message),
                    "provider call failed, retrying"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(err) => {
                if err.is_retryable() {
                    tracing::warn!(
                        attempts = attempt,
                        kind = ?err.kind,
                        error = %crate::classify::scrub_secrets(&err.message),
                        "provider call failed, retries exhausted"
                    );
                }
                return Err(err);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use vibe_core::ErrorKind;

    #[test]
    fn backoff_doubles_and_caps() {
        let p = RetryPolicy {
            max_attempts: 10,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
            jitter: false,
        };
        assert_eq!(p.backoff(1), Duration::from_millis(100));
        assert_eq!(p.backoff(2), Duration::from_millis(200));
        assert_eq!(p.backoff(3), Duration::from_millis(400));
        assert_eq!(p.backoff(4), Duration::from_millis(500));
        assert_eq!(p.backoff(64), Duration::from_millis(500));
    }

    #[test]
    fn retry_after_hint_wins_but_is_capped() {
        let p = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_secs(5),
            jitter: true,
        };
        let e = Error::new(ErrorKind::RateLimited, "slow down")
            .with_retry_after(Duration::from_secs(2));
        assert_eq!(p.delay_for(1, &e), Duration::from_secs(2));
        let e = Error::new(ErrorKind::RateLimited, "slow down")
            .with_retry_after(Duration::from_secs(3600));
        assert_eq!(p.delay_for(1, &e), Duration::from_secs(5));
    }

    #[test]
    fn jitter_stays_in_range() {
        for _ in 0..50 {
            let d = apply_jitter(Duration::from_millis(1000));
            assert!(d >= Duration::from_millis(750) && d < Duration::from_millis(1250));
        }
    }

    #[tokio::test]
    async fn retries_transient_errors_until_success() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let out = with_retry(&RetryPolicy::immediate(4), move || {
            let c = c.clone();
            async move {
                if c.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err(Error::new(ErrorKind::ServerError, "boom"))
                } else {
                    Ok("done")
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(out, "done");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_permanent_errors() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let err = with_retry(&RetryPolicy::immediate(5), move || {
            c.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(Error::new(ErrorKind::AuthFailed, "bad key")) }
        })
        .await
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::AuthFailed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exhaustion_returns_last_error_with_hint() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let err = with_retry(&RetryPolicy::immediate(3), move || {
            c.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(
                    Error::new(ErrorKind::RateLimited, "429")
                        .with_retry_after(Duration::from_secs(7)),
                )
            }
        })
        .await
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::RateLimited);
        assert_eq!(err.retry_after, Some(Duration::from_secs(7)));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn zero_attempts_still_runs_once() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let _ = with_retry(&RetryPolicy::immediate(0), move || {
            c.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(Error::new(ErrorKind::Network, "down")) }
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
