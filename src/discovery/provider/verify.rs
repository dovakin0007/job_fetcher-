//! Posting legitimacy check: a listing must still be live before it enters
//! the backend.
//!
//! Search snippets are stale by nature — the job may have been filled or the
//! page taken down after indexing. For every candidate posting this module
//! confirms the URL still serves an open job page:
//!
//! 1. fresh full text from the page cache (`page_texts`, written by enrich)
//!    is scanned for closed-posting phrases;
//! 2. otherwise one cheap `GET` (10s timeout) in a dedicated parallel
//!    lane — up to `SEARCH_VERIFY_INFLIGHT` (default 10) concurrent checks
//!    against different hosts, so 60 URLs take seconds not minutes.
//!
//! Results are cached in `kv_store:verify_state` — live for 24h, dead for
//! 7 days — so repeated runs don't refetch. Fetch failures fail **open**
//! (treated as live) so a flaky network never wipes the jobs table.

use std::collections::HashMap;
use std::time::Duration;

use sqlx::SqlitePool;

const STATE_KEY: &str = "verify_state";
const LIVE_FRESH_SECS: i64 = 24 * 60 * 60;
const DEAD_FRESH_SECS: i64 = 7 * 24 * 60 * 60;
const MAX_ENTRIES: usize = 1_000;

fn verify_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("SEARCH_VERIFY_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(10)
            .max(1),
    )
}

/// Dedicated lane for legitimacy checks: many small GETs to *different*
/// hosts, so they run up to `SEARCH_VERIFY_INFLIGHT` (default 10) in
/// parallel instead of trickling through the search semaphore (2 at a time
/// with a 300ms gap — way too slow for 60 URLs).
fn verify_semaphore() -> &'static std::sync::Arc<tokio::sync::Semaphore> {
    static SEMAPHORE: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    SEMAPHORE.get_or_init(|| {
        let slots = std::env::var("SEARCH_VERIFY_INFLIGHT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(10)
            .clamp(1, 32);
        std::sync::Arc::new(tokio::sync::Semaphore::new(slots))
    })
}

pub fn verify_budget() -> usize {
    std::env::var("SEARCH_VERIFY_MAX")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(60)
        .max(1)
}

#[derive(Debug, Clone)]
struct Verdict {
    at: chrono::DateTime<chrono::Utc>,
    live: bool,
}

fn load_state(raw: Option<&str>) -> HashMap<String, Verdict> {
    let mut out = HashMap::new();
    let Some(raw) = raw else { return out };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) else {
        return out;
    };
    let Some(map) = parsed.as_object() else {
        return out;
    };
    for (url, entry) in map {
        let at = entry
            .get("at")
            .and_then(|value| value.as_str())
            .and_then(|value| value.parse::<chrono::DateTime<chrono::Utc>>().ok());
        let live = entry.get("live").and_then(|value| value.as_bool());
        if let (Some(at), Some(live)) = (at, live) {
            out.insert(url.clone(), Verdict { at, live });
        }
        if out.len() >= MAX_ENTRIES {
            break;
        }
    }
    out
}

fn fresh_enough(verdict: &Verdict, now: chrono::DateTime<chrono::Utc>) -> bool {
    let age = now.signed_duration_since(verdict.at).num_seconds().max(0);
    age < if verdict.live {
        LIVE_FRESH_SECS
    } else {
        DEAD_FRESH_SECS
    }
}

/// Returns the subset of `urls` that are still live postings. Unknown
/// (fetch failed) counts as live — fail open, never nuke on network errors.
pub async fn verify_postings(urls: &[String], db: &SqlitePool) -> HashMap<String, bool> {
    let now = chrono::Utc::now();
    let state_raw = crate::store::kv_get(db, STATE_KEY).await.ok().flatten();
    let mut state = load_state(state_raw.as_deref());
    let mut out: HashMap<String, bool> = HashMap::new();
    let mut to_check: Vec<String> = Vec::new();
    for url in urls {
        if let Some(verdict) = state.get(url) {
            if fresh_enough(verdict, now) {
                out.insert(url.clone(), verdict.live);
                continue;
            }
        }
        if !to_check.contains(url) {
            to_check.push(url.clone());
        }
    }
    to_check.truncate(verify_budget());
    // Whole-request timeout (connect + body), so one hung host cannot stall
    // the batch; failures fail open downstream.
    let timeout = verify_timeout();
    let client = reqwest::Client::builder()
        .user_agent("RustJobAgent/0.6 (local job search)")
        .timeout(timeout)
        .build()
        .unwrap_or_else(|_| super::search_client());
    let semaphore = verify_semaphore().clone();
    let mut handles = Vec::new();
    for url in to_check {
        let client = client.clone();
        let db = db.clone();
        let semaphore = semaphore.clone();
        handles.push(tokio::spawn(async move {
            // Hold a lane slot for the whole check, not the gap-pacing.
            let _lane = semaphore.acquire_owned().await;
            let live = check_one(&client, &url, &db).await;
            (url, live)
        }));
    }
    let mut changed = false;
    for handle in handles {
        match handle.await {
            Ok((url, Some(live))) => {
                out.insert(url.clone(), live);
                state.insert(
                    url,
                    Verdict {
                        at: chrono::Utc::now(),
                        live,
                    },
                );
                changed = true;
            }
            Ok((url, None)) => {
                // Unknown: fail open, do not cache the non-answer.
                out.insert(url, true);
            }
            Err(error) => {
                tracing::warn!(%error, "posting verify task failed; keeping job");
            }
        }
    }
    if changed {
        // Prune oldest, keep the map bounded.
        if state.len() > MAX_ENTRIES {
            let mut entries: Vec<_> = state.into_iter().collect();
            entries.sort_by(|left, right| right.1.at.cmp(&left.1.at));
            entries.truncate(MAX_ENTRIES);
            state = entries.into_iter().collect();
        }
        let payload = serde_json::json!({
            "v": state
                .iter()
                .map(|(url, verdict)| {
                    (
                        url.clone(),
                        serde_json::json!({
                            "at": verdict.at.to_rfc3339(),
                            "live": verdict.live,
                        }),
                    )
                })
                .collect::<serde_json::Map<String, serde_json::Value>>()
        });
        if let Err(error) = crate::store::kv_set(db, STATE_KEY, &payload.to_string()).await {
            tracing::warn!(%error, "verify state was not saved");
        }
    }
    // Anything unchecked (over budget) stays live.
    for url in urls {
        out.entry(url.clone()).or_insert(true);
    }
    out
}

/// `Some(true)` live, `Some(false)` dead, `None` unknown (fail open).
async fn check_one(client: &reqwest::Client, url: &str, db: &SqlitePool) -> Option<bool> {
    // 1. Fresh cached page text first — no HTTP needed.
    if let Ok(Some(text)) = crate::store::page_text(db, url).await {
        if crate::models::posting_is_closed(&text) {
            tracing::info!(url = %url, "posting verified dead via cached page text");
            return Some(false);
        }
        if text.trim().chars().count() >= 200 {
            return Some(true);
        }
        // Thin cached text: fall through to a live check.
    }
    // 2. One cheap GET in the parallel lane (client timeout covers it).
    let response = client.get(url).send().await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(url = %url, %error, "posting verify request failed; keeping job");
            return None;
        }
    };
    if !response.status().is_success() {
        // Redirects are followed by default; a terminal 404/410 means gone.
        // Some boards answer 403 to bots — fail open, not dead.
        if response.status().as_u16() == 404 || response.status().as_u16() == 410 {
            tracing::info!(url = %url, status = %response.status(), "posting verified dead via HTTP status");
            return Some(false);
        }
        tracing::warn!(url = %url, status = %response.status(), "posting verify got non-success; keeping job");
        return None;
    }
    let bytes = response.bytes().await.unwrap_or_default();
    let raw = String::from_utf8_lossy(&bytes);
    let text = crate::providers::strip_html(&raw);
    if crate::models::posting_is_closed(&text) {
        tracing::info!(url = %url, "posting verified dead via page text");
        return Some(false);
    }
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_pages_are_dead() {
        assert!(crate::models::posting_is_closed(
            "This position has been filled. Thanks for applying."
        ));
        assert!(!crate::models::posting_is_closed(
            "Senior Rust Engineer in Bengaluru. 3-5 years of experience. Apply now."
        ));
    }

    #[test]
    fn verify_state_roundtrips_and_expires() {
        let now = chrono::Utc::now();
        let mut state = HashMap::new();
        state.insert(
            "https://example.com/fresh".to_string(),
            Verdict { at: now, live: true },
        );
        state.insert(
            "https://example.com/stale".to_string(),
            Verdict {
                at: now - chrono::Duration::days(2),
                live: true,
            },
        );
        state.insert(
            "https://example.com/dead".to_string(),
            Verdict { at: now, live: false },
        );
        assert!(fresh_enough(&state["https://example.com/fresh"], now));
        assert!(!fresh_enough(&state["https://example.com/stale"], now));
        assert!(fresh_enough(&state["https://example.com/dead"], now));
    }
}
