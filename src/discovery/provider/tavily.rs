use super::{SearchResult, EXCLUDED_DOMAINS};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct TavilyProvider {
    client: Client,
    api_key: String,
}

#[derive(Serialize)]
struct TavilyRequest<'a> {
    query: &'a str,
    search_depth: &'static str,
    max_results: usize,
    include_answer: bool,
    include_raw_content: bool,
    time_range: &'static str,
    exclude_domains: &'static [&'static str],
}

#[derive(Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    title: Option<String>,
    url: String,
    content: Option<String>,
    #[serde(default)]
    raw_content: Option<String>,
    published_date: Option<String>,
}

impl TavilyProvider {
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
        max_age_days: u32,
    ) -> anyhow::Result<Vec<SearchResult>> {
        // Dynamic range: Tavily only offers day/week/month/year buckets, so
        // pick the smallest bucket covering the window; the local
        // `is_applicable_posting` check then enforces the exact day count.
        let body = TavilyRequest {
            query,
            search_depth: "advanced",
            max_results: limit.clamp(1, 20),
            include_answer: false,
            include_raw_content: true,
            time_range: time_range_for(max_age_days),
            exclude_domains: EXCLUDED_DOMAINS,
        };
        let attempts = crate::discovery::rate_limit::max_attempts();
        let mut last_error = String::from("unknown Tavily error");
        for attempt in 0..attempts {
            let _permit = crate::discovery::rate_limit::acquire("tavily").await;
            let response = self
                .client
                .post("https://api.tavily.com/search")
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    last_error = format!("Tavily request failed: {error}");
                    if attempt + 1 < attempts {
                        let wait = crate::discovery::rate_limit::backoff_secs(attempt);
                        tracing::warn!(%error, attempt = attempt + 1, wait_secs = wait, "Tavily request failed; retrying");
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    anyhow::bail!("Tavily search failed: {error}");
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
                    format!("Tavily search failed (429): {}", super::trim_err(&text));
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
                        "Tavily rate limited (429); backing off"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
                anyhow::bail!("{last_error}");
            }
            if !status.is_success() {
                anyhow::bail!(
                    "Tavily search failed ({status}): {}",
                    super::trim_err(&text)
                );
            }
            let parsed: TavilyResponse = serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("Tavily response was not search JSON: {e}"))?;
            return Ok(parsed
                .results
                .into_iter()
                .map(|item| {
                // Prefer the cleaned content; fall back to raw content (HTML
                // stripped) when the summary is thin.
                let snippet = match item.content {
                    Some(clean) if clean.trim().chars().count() >= 500 => Some(clean),
                    clean => item
                        .raw_content
                        .map(|raw| crate::providers::strip_html(&raw))
                        .filter(|s| !s.trim().is_empty())
                        .or(clean),
                };
                SearchResult {
                    title: item.title.unwrap_or_default(),
                    url: item.url,
                    snippet,
                    published_at: item.published_date,
                    source: "tavily",
                }
            })
            .collect());
        }
        anyhow::bail!("{last_error}")
    }
}

/// Smallest Tavily `time_range` bucket covering a day window.
fn time_range_for(max_age_days: u32) -> &'static str {
    if max_age_days <= 1 {
        "day"
    } else if max_age_days <= 7 {
        "week"
    } else if max_age_days <= 31 {
        "month"
    } else {
        "year"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_range_covers_any_window() {
        assert_eq!(time_range_for(1), "day");
        assert_eq!(time_range_for(7), "week");
        assert_eq!(time_range_for(30), "month");
        assert_eq!(time_range_for(90), "year");
    }

    #[test]
    fn strips_tags_from_raw_content() {
        assert_eq!(
            crate::providers::strip_html(
                "<div><h1>Backend Engineer</h1><p>Build&nbsp;Rust &amp; Go.</p></div>"
            ),
            "Backend Engineer Build Rust & Go."
        );
    }
}
