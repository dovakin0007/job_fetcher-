//! Brave Search provider (keyed, `BRAVE_API_KEY`).
//!
//! Uses the official Web Search API: `X-Subscription-Token` auth, `country`
//! scoping, and `freshness` windows that mirror `max_age_days`. Brave does not
//! return machine-readable publish dates, so hits carry no date and the usual
//! "undated postings are kept when the country matches" rule applies.

use super::{SearchResult, EXCLUDED_DOMAINS};
use reqwest::Client;
use serde::Deserialize;

#[derive(Clone)]
pub struct BraveProvider {
    client: Client,
    api_key: String,
}

#[derive(Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveWeb>,
}

#[derive(Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    title: Option<String>,
    url: String,
    #[serde(default)]
    description: Option<String>,
}

impl BraveProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: super::search_client(),
            api_key,
        }
    }

    pub(crate) async fn search(
        &self,
        query: &str,
        limit: usize,
        country_code: Option<&str>,
        max_age_days: u32,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let attempts = crate::discovery::rate_limit::max_attempts();
        let mut last_error = String::from("unknown Brave error");
        for attempt in 0..attempts {
            let _permit = crate::discovery::rate_limit::acquire("brave").await;
            let mut request = self
                .client
                .get("https://api.search.brave.com/res/v1/web/search")
                .header("X-Subscription-Token", &self.api_key)
                .query(&[
                    ("q", query.to_string()),
                    ("count", limit.clamp(1, 20).to_string()),
                    // Dynamic range: Brave only offers pd/pw/pm/py buckets, so
                    // pick the smallest covering the window; the local
                    // `is_applicable_posting` check enforces the exact days.
                    ("freshness", freshness_for(max_age_days).into()),
                    ("search_lang", "en".into()),
                    ("spellcheck", "1".into()),
                ]);
            if let Some(country) = country_code {
                request = request.query(&[("country", country.to_string())]);
            }
            let response = request.send().await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    last_error = format!("Brave request failed: {error}");
                    if attempt + 1 < attempts {
                        let wait = crate::discovery::rate_limit::backoff_secs(attempt);
                        tracing::warn!(%error, attempt = attempt + 1, wait_secs = wait, "Brave request failed; retrying");
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    anyhow::bail!("Brave search failed: {error}");
                }
            };
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let text = response.text().await.unwrap_or_default();
            if status.as_u16() == 429 {
                last_error =
                    format!("Brave search failed (429): {}", super::trim_err(&text));
                if attempt + 1 < attempts {
                    let wait = retry_after
                        .as_deref()
                        .and_then(|value| {
                            crate::discovery::rate_limit::parse_retry_after(
                                value,
                                chrono::Utc::now(),
                            )
                        })
                        .unwrap_or_else(|| {
                            std::time::Duration::from_secs(
                                crate::discovery::rate_limit::backoff_secs(attempt),
                            )
                        });
                    tracing::warn!(
                        query = query,
                        attempt = attempt + 1,
                        wait_secs = wait.as_secs(),
                        "Brave rate limited (429); backing off"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
                anyhow::bail!("{last_error}");
            }
            if !status.is_success() {
                anyhow::bail!("Brave search failed ({status}): {}", super::trim_err(&text));
            }
            let parsed: BraveResponse = serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("Brave response was not search JSON: {e}"))?;
            return Ok(parsed
                .web
                .map(|web| web.results)
                .unwrap_or_default()
                .into_iter()
                .filter(|item| {
                    // Client-side domain filter (the API has no exclusion list).
                    reqwest::Url::parse(&item.url)
                        .ok()
                        .and_then(|url| url.host_str().map(str::to_string))
                        .map(|host| {
                            let host = host.trim_start_matches("www.").to_ascii_lowercase();
                            !EXCLUDED_DOMAINS
                                .iter()
                                .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
                        })
                        .unwrap_or(false)
                })
                .map(|item| SearchResult {
                    title: item.title.unwrap_or_default(),
                    url: item.url,
                    snippet: item.description.filter(|s| !s.trim().is_empty()),
                    published_at: None,
                    source: "brave",
                })
                .collect());
        }
        anyhow::bail!("{last_error}")
    }
}

/// Smallest Brave `freshness` bucket covering a day window
/// (pd = past day, pw = week, pm = month, py = year).
fn freshness_for(max_age_days: u32) -> &'static str {
    if max_age_days <= 1 {
        "pd"
    } else if max_age_days <= 7 {
        "pw"
    } else if max_age_days <= 31 {
        "pm"
    } else {
        "py"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_covers_any_window() {
        assert_eq!(freshness_for(1), "pd");
        assert_eq!(freshness_for(7), "pw");
        assert_eq!(freshness_for(30), "pm");
        assert_eq!(freshness_for(90), "py");
    }

    #[test]
    fn parses_brave_response() {
        let parsed: BraveResponse = serde_json::from_str(
            r#"{"web":{"results":[
                {"title":"Backend Engineer","url":"https://example.com/jobs/1","description":"Hiring in Bengaluru."},
                {"title":"Spam","url":"https://linkedin.com/jobs/2","description":"x"}
            ]}}"#,
        )
        .unwrap();
        let results = parsed.web.unwrap().results;
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].description.as_deref(),
            Some("Hiring in Bengaluru.")
        );
    }
}
