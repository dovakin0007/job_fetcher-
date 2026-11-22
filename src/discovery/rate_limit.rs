//! Polite HTTP pacing for third-party search APIs.
//!
//! Search providers fire many queries per discovery run. Without pacing a
//! single run can burst past per-team rate limits (HTTP 429). This module
//! centralises three behaviours:
//!
//! * a global cap on concurrent outbound search requests,
//! * a minimum gap between request starts,
//! * 429 handling that honours `Retry-After` and otherwise backs off, with a
//!   bounded number of retries.
//!
//! Tune with `SEARCH_MAX_INFLIGHT` (default 2), `SEARCH_MIN_GAP_MS` (default
//! 300), `SEARCH_MAX_RETRIES` (default 3) and `SEARCH_RETRY_CAP_SECS`
//! (default 30).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

fn env_usize(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(fallback)
}

fn env_u64(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(fallback)
}

struct Shared {
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
    gap: Duration,
    last_start: Mutex<HashMap<&'static str, Instant>>,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(
            env_usize("SEARCH_MAX_INFLIGHT", 2).clamp(1, 16),
        )),
        gap: Duration::from_millis(env_u64("SEARCH_MIN_GAP_MS", 300)),
        last_start: Mutex::new(HashMap::new()),
    })
}

/// Guard that holds one in-flight slot until dropped. Keep it alive across
/// the HTTP send the acquire was pacing.
pub struct Permit {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// Wait until `key` may start a request: at most `SEARCH_MAX_INFLIGHT`
/// requests in flight overall, and at least `SEARCH_MIN_GAP_MS` since the
/// last request start for the same key.
pub async fn acquire(key: &'static str) -> Permit {
    let shared = shared();
    let permit = shared
        .semaphore
        .clone()
        .acquire_owned()
        .await
        .expect("semaphore open");
    loop {
        let wait = {
            let mut last = shared.last_start.lock().expect("rate-limit clock");
            match last.get(key).and_then(|t| t.checked_add(shared.gap)) {
                Some(next) => {
                    let now = Instant::now();
                    if next > now {
                        next.duration_since(now)
                    } else {
                        last.insert(key, now);
                        Duration::ZERO
                    }
                }
                None => {
                    last.insert(key, Instant::now());
                    Duration::ZERO
                }
            }
        };
        if wait.is_zero() {
            return Permit { _permit: permit };
        }
        // Sleep while still holding the permit so a burst of waiters cannot
        // all stampede once the gap elapses.
        tokio::time::sleep(wait).await;
    }
}

/// Parse a `Retry-After` header value: either delay-seconds or an HTTP date.
/// Returns a duration clamped to `SEARCH_RETRY_CAP_SECS` (default 30s).
pub fn parse_retry_after(value: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let cap = Duration::from_secs(env_u64("SEARCH_RETRY_CAP_SECS", 30).max(1));
    if let Ok(secs) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(secs).min(cap));
    }
    // IMF-fixdate, e.g. `Wed, 21 Oct 2026 07:28:00 GMT`.
    if let Ok(date) = chrono::DateTime::parse_from_rfc2822(value.trim()) {
        let delta = date.with_timezone(&chrono::Utc) - now;
        if delta > chrono::Duration::zero() {
            return Some(delta.to_std().unwrap_or(Duration::ZERO).min(cap));
        }
    }
    None
}

/// Exponential backoff with a small deterministic jitter, in seconds.
pub fn backoff_secs(attempt: u32) -> u64 {
    let exp = 2u64.saturating_pow(attempt.min(6));
    (exp + (attempt as u64 * 137 % 500) / 100).min(env_u64("SEARCH_RETRY_CAP_SECS", 30).max(1))
}

/// Maximum number of attempts (initial try + retries) for a rate-limited call.
pub fn max_attempts() -> u32 {
    (env_usize("SEARCH_MAX_RETRIES", 3) as u32).clamp(1, 10)
}

/// How many HTTP fetches a single discovery run may perform. `0` means no
/// windowing: fetch every uncached query. Otherwise the run fetches at most
/// this many (engine, query) pairs — one paginated batch — stores the pages
/// in SQLite, and leaves the rest for later runs. The offset persists in
/// `kv_store:search_query_offset`, so each run (page load / scheduled tick)
/// continues with the next slice while cached pages serve the rest.
pub fn queries_per_run() -> usize {
    std::env::var("SEARCH_QUERIES_PER_RUN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2)
}

/// Slice `queries` into the window starting at `offset`, wrapping around once.
/// Returns the window and the offset for the next run. An empty list or a
/// window of 0 selects everything and leaves the offset at 0.
pub fn select_query_window(queries: &[String], offset: usize, window: usize) -> (Vec<String>, usize) {
    if queries.is_empty() || window == 0 {
        return (queries.to_vec(), 0);
    }
    let total = queries.len();
    let start = offset % total;
    let take = window.min(total);
    let mut out = Vec::with_capacity(take);
    for i in 0..take {
        out.push(queries[(start + i) % total].clone());
    }
    (out, (start + take) % total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_retry_after_seconds_and_dates() {
        let now = chrono::DateTime::parse_from_rfc2822("Wed, 21 Oct 2026 07:28:00 GMT")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            parse_retry_after("120", now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(parse_retry_after("7", now), Some(Duration::from_secs(7)));
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2026 07:28:10 GMT", now),
            Some(Duration::from_secs(10))
        );
        // Past dates and garbage yield nothing.
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2026 07:27:00 GMT", now),
            None
        );
        assert_eq!(parse_retry_after("soon", now), None);
    }

    #[test]
    fn backoff_grows_and_stays_capped() {
        let first = backoff_secs(0);
        let second = backoff_secs(1);
        assert!(second >= first);
        assert!(backoff_secs(100) <= 30);
    }

    #[test]
    fn query_window_wraps_around() {
        let queries: Vec<String> = ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let (page, next) = select_query_window(&queries, 0, 2);
        assert_eq!(page, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(next, 2);
        let (page, next) = select_query_window(&queries, 4, 2);
        assert_eq!(page, vec!["e".to_string(), "a".to_string()]);
        assert_eq!(next, 1);
        let (page, next) = select_query_window(&queries, 7, 10);
        assert_eq!(page.len(), 5);
        assert_eq!(next, 2);
        let (page, next) = select_query_window(&queries, 0, 0);
        assert_eq!(page.len(), 5);
        assert_eq!(next, 0);
        let (page, next) = select_query_window(&[], 3, 2);
        assert!(page.is_empty());
        assert_eq!(next, 0);
    }

    #[tokio::test]
    async fn gap_paces_starts() {
        // Direct construction bypasses the global singleton for determinism.
        let gate = Shared {
            semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(8)),
            gap: Duration::from_millis(60),
            last_start: Mutex::new(HashMap::new()),
        };
        let t0 = Instant::now();
        for _ in 0..2 {
            loop {
                let wait = {
                    let mut last = gate.last_start.lock().unwrap();
                    match last.get("k").and_then(|t| t.checked_add(gate.gap)) {
                        Some(next) => {
                            let now = Instant::now();
                            if next > now {
                                next.saturating_duration_since(now)
                            } else {
                                last.insert("k", now);
                                Duration::ZERO
                            }
                        }
                        None => {
                            last.insert("k", Instant::now());
                            Duration::ZERO
                        }
                    }
                };
                if wait.is_zero() {
                    break;
                }
                tokio::time::sleep(wait).await;
            }
        }
        assert!(t0.elapsed() >= Duration::from_millis(60));
    }

    #[tokio::test]
    async fn semaphore_caps_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let sem = Arc::new(tokio::sync::Semaphore::new(2));
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let (sem, live, peak) = (sem.clone(), live.clone(), peak.clone());
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                live.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= 2);
    }
}
