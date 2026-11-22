//! Full-page description enrichment for thin search snippets.
//!
//! Search results only carry a few hundred characters. For postings whose
//! snippet is below `SEARCH_ENRICH_MIN_CHARS`, the full page text is fetched
//! through Tavily `/extract`, then cached in SQLite (`page_texts`, 30-day
//! freshness) so later discovery runs reuse it instead of paying for the
//! same page again.

use sqlx::SqlitePool;
use std::collections::HashMap;

fn min_chars() -> usize {
    std::env::var("SEARCH_ENRICH_MIN_CHARS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_200)
}

fn max_pages() -> usize {
    std::env::var("SEARCH_ENRICH_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(25)
}

/// Returns full page text for as many of `urls` as the cache or providers yield.
pub async fn enrich(urls: &[String], db: &SqlitePool) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut missing = Vec::new();
    for url in urls {
        match crate::store::page_text(db, url).await {
            Ok(Some(text)) => {
                out.insert(url.clone(), text);
            }
            Ok(None) => missing.push(url.clone()),
            Err(error) => {
                tracing::warn!(%error, url = %url, "page-text cache lookup failed");
                missing.push(url.clone());
            }
        }
    }
    missing.truncate(max_pages());
    if missing.is_empty() {
        return out;
    }
    let fetched = if super::env_secret("TAVILY_API_KEY").is_some() {
        fetch_tavily(&missing).await
    } else {
        tracing::info!("No Tavily key; skipping description enrichment");
        HashMap::new()
    };
    for (url, text) in fetched {
        if text.trim().chars().count() < 200 {
            continue;
        }
        if let Err(error) = crate::store::save_page_text(db, &url, &text).await {
            tracing::warn!(%error, url = %url, "page-text cache write failed");
        }
        out.insert(url, text);
    }
    out
}

pub fn needs_enrichment(snippet: Option<&str>) -> bool {
    snippet.map(|s| s.trim().chars().count()).unwrap_or(0) < min_chars()
}

async fn fetch_tavily(urls: &[String]) -> HashMap<String, String> {
    let Some(key) = super::env_secret("TAVILY_API_KEY") else {
        return HashMap::new();
    };
    let client = super::search_client();
    let mut out = HashMap::new();
    // Batches of 5 keep each extract call small and failures isolated.
    // Paced sequentially so extract calls don't add to search burst pressure.
    for chunk in urls.chunks(5) {
        let _permit = crate::discovery::rate_limit::acquire("tavily").await;
        let body = serde_json::json!({
            "urls": chunk,
            "query": "job description responsibilities requirements qualifications",
            "extract_depth": "advanced",
            "include_images": false,
        });
        let response = client
            .post("https://api.tavily.com/extract")
            .bearer_auth(&key)
            .json(&body)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%error, "tavily extract request failed");
                continue;
            }
        };
        if !response.status().is_success() {
            tracing::warn!(status = %response.status(), "tavily extract failed");
            continue;
        }
        let parsed: serde_json::Value = match response.json().await {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(%error, "tavily extract was not JSON");
                continue;
            }
        };
        if let Some(results) = parsed.get("results").and_then(|v| v.as_array()) {
            for item in results {
                let url = item.get("url").and_then(|v| v.as_str()).unwrap_or_default();
                let text = item
                    .get("content")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| {
                        item.get("raw_content")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.trim().is_empty())
                    })
                    .unwrap_or_default();
                if !url.is_empty() && !text.trim().is_empty() {
                    out.insert(url.to_string(), text.to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thin_snippets_need_enrichment() {
        assert!(needs_enrichment(None));
        assert!(needs_enrichment(Some("short")));
        assert!(!needs_enrichment(Some(&"x".repeat(5_000))));
    }
}
