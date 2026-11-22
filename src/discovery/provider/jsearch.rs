//! JSearch provider (keyed, `JSEARCH_API_KEY` — a RapidAPI key).
//!
//! Aggregated job postings (LinkedIn, Indeed, Glassdoor, ZipRecruiter) as
//! structured JSON: real titles, employers, locations, apply links, and
//! posted dates — complementary to SerpApi Google Jobs + Glassdoor.
//! `date_posted` mirrors `max_age_days`; the local
//! `is_applicable_posting` check still enforces the exact day count.

use super::SearchResult;
use reqwest::Client;
use serde::Deserialize;

const API_HOST: &str = "jsearch.p.rapidapi.com";

#[derive(Clone)]
pub struct JSearchProvider {
    client: Client,
    api_key: String,
}

#[derive(Deserialize)]
struct JSearchResponse {
    #[serde(default)]
    data: Vec<JSearchJob>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct JSearchJob {
    #[serde(default)]
    job_title: Option<String>,
    #[serde(default)]
    employer_name: Option<String>,
    #[serde(default)]
    job_apply_link: Option<String>,
    #[serde(default)]
    job_google_link: Option<String>,
    #[serde(default)]
    job_description: Option<String>,
    #[serde(default)]
    job_posted_at_datetime_utc: Option<String>,
    #[serde(default)]
    job_city: Option<String>,
    #[serde(default)]
    job_country: Option<String>,
}

/// Smallest JSearch `date_posted` bucket covering a day window.
fn date_posted_for(max_age_days: u32) -> &'static str {
    if max_age_days <= 1 {
        "today"
    } else if max_age_days <= 3 {
        "3days"
    } else if max_age_days <= 7 {
        "week"
    } else {
        "month"
    }
}

impl JSearchProvider {
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
        _location: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        // ~10 results per page.
        let num_pages = ((limit.clamp(1, 50) + 9) / 10).clamp(1, 5).to_string();
        let attempts = crate::discovery::rate_limit::max_attempts();
        let mut last_error = String::from("unknown JSearch error");
        for attempt in 0..attempts {
            let _permit = crate::discovery::rate_limit::acquire("jsearch").await;
            let mut request = self
                .client
                .get("https://jsearch.p.rapidapi.com/search")
                .header("x-rapidapi-key", &self.api_key)
                .header("x-rapidapi-host", API_HOST)
                .query(&[
                    ("query", query.to_string()),
                    ("page", "1".to_string()),
                    ("num_pages", num_pages.clone()),
                    ("date_posted", date_posted_for(max_age_days).to_string()),
                ]);
            if let Some(country) = country_code {
                request = request.query(&[("country", country.to_ascii_lowercase())]);
            }
            let response = request.send().await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    last_error = format!("JSearch request failed: {error}");
                    if attempt + 1 < attempts {
                        let wait = crate::discovery::rate_limit::backoff_secs(attempt);
                        tracing::warn!(%error, attempt = attempt + 1, wait_secs = wait, "JSearch request failed; retrying");
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    anyhow::bail!("JSearch search failed: {error}");
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
                    format!("JSearch search failed (429): {}", super::trim_err(&text));
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
                        "JSearch rate limited (429); backing off"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
                anyhow::bail!("{last_error}");
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                anyhow::bail!(
                    "JSearch search failed ({status} — bad key or quota): {}",
                    super::trim_err(&text)
                );
            }
            if !status.is_success() {
                anyhow::bail!("JSearch search failed ({status}): {}", super::trim_err(&text));
            }
            let parsed: JSearchResponse = serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("JSearch response was not search JSON: {e}"))?;
            return Ok(parsed
                .data
                .into_iter()
                .filter(|item| {
                    item.job_apply_link
                        .as_deref()
                        .or(item.job_google_link.as_deref())
                        .is_some_and(|url| {
                            reqwest::Url::parse(url)
                                .ok()
                                .and_then(|parsed| parsed.host_str().map(str::to_string))
                                .map(|host| {
                                    let host =
                                        host.trim_start_matches("www.").to_ascii_lowercase();
                                    !super::EXCLUDED_DOMAINS.iter().any(|domain| {
                                        host == *domain
                                            || host.ends_with(&format!(".{domain}"))
                                    })
                                })
                                .unwrap_or(false)
                        })
                })
                .filter_map(|item| {
                    let url = item
                        .job_apply_link
                        .or(item.job_google_link)
                        .filter(|link| !link.trim().is_empty())?;
                    let snippet = item.job_description.filter(|s| !s.trim().is_empty());
                    let published_at = item
                        .job_posted_at_datetime_utc
                        .as_deref()
                        .and_then(crate::models::parse_posted_at)
                        .map(|date| date.to_rfc3339());
                    let employer = item.employer_name.unwrap_or_default();
                    let mut title = item.job_title.unwrap_or_default();
                    if title.trim().is_empty() {
                        return None;
                    }
                    if !employer.trim().is_empty()
                        && !title
                            .to_ascii_lowercase()
                            .contains(&employer.to_ascii_lowercase())
                    {
                        title = format!("{title} at {employer}");
                    }
                    Some(SearchResult {
                        title,
                        url,
                        snippet,
                        published_at,
                        source: "jsearch",
                    })
                })
                .collect());
        }
        anyhow::bail!("{last_error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_posted_covers_any_window() {
        assert_eq!(date_posted_for(1), "today");
        assert_eq!(date_posted_for(3), "3days");
        assert_eq!(date_posted_for(7), "week");
        assert_eq!(date_posted_for(30), "month");
    }

    #[test]
    fn parses_jsearch_response() {
        let parsed: JSearchResponse = serde_json::from_str(
            r#"{"status":"OK","data":[
                {"job_title":"Rust Engineer","employer_name":"Acme","job_apply_link":"https://example.com/j/1","job_description":"Build Rust services.","job_posted_at_datetime_utc":"2026-11-20T10:00:00.000Z","job_country":"IN"},
                {"job_title":"Spam","job_apply_link":"https://linkedin.com/jobs/2"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(parsed.data.len(), 2);
        assert_eq!(parsed.data[0].employer_name.as_deref(), Some("Acme"));
    }
}
