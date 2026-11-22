//! SerpApi job providers (keyed, `SERPAPI_API_KEY`).
//!
//! Google Jobs: structured postings (title, company, location, full
//! description, posted date) instead of web-search snippets.
//! Glassdoor Jobs: same key, worldwide incl. India, 30/page, with posting
//! age, salary, skills and company rating per job. Recency is enforced
//! locally by `is_applicable_posting`.

use super::SearchResult;
use reqwest::Client;
use serde::Deserialize;

#[derive(Clone)]
pub struct SerpApiProvider {
    client: Client,
    api_key: String,
}

#[derive(Deserialize)]
struct SerpApiResponse {
    #[serde(default)]
    jobs_results: Vec<SerpApiJob>,
}

#[derive(Deserialize, Default)]
struct SerpApiJob {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    company_name: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    job_highlights: Vec<SerpApiHighlight>,
    #[serde(default)]
    detected_extensions: SerpApiExtensions,
    #[serde(default)]
    apply_options: Vec<SerpApiApplyOption>,
    #[serde(default)]
    share_link: Option<String>,
}

#[derive(Deserialize, Default)]
struct SerpApiHighlight {
    #[serde(default)]
    items: Vec<String>,
}

#[derive(Deserialize, Default)]
struct SerpApiExtensions {
    #[serde(default)]
    posted_at: Option<String>,
    #[serde(default)]
    schedule_type: Option<String>,
}

#[derive(Deserialize, Default)]
struct SerpApiApplyOption {
    #[serde(default)]
    link: Option<String>,
}

fn country_name_for(code: &str) -> Option<&'static str> {
    match code.trim().to_ascii_uppercase().as_str() {
        "IN" => Some("India"),
        "US" => Some("United States"),
        "GB" => Some("United Kingdom"),
        "DE" => Some("Germany"),
        "CA" => Some("Canada"),
        "SG" => Some("Singapore"),
        "AU" => Some("Australia"),
        _ => None,
    }
}

fn google_domain_for(code: Option<&str>) -> &'static str {
    match code.map(|c| c.trim().to_ascii_uppercase()).as_deref() {
        Some("IN") => "google.co.in",
        Some("GB") => "google.co.uk",
        Some("DE") => "google.de",
        Some("CA") => "google.ca",
        Some("SG") => "google.com.sg",
        Some("AU") => "google.com.au",
        _ => "google.com",
    }
}

/// Strip filler words the generic query builder adds. Google Jobs matches
/// role + stack + place best without them:
/// "hiring Senior Rust Engineer job in Bengaluru India" ->
/// "Senior Rust Engineer in Bengaluru India".
pub(crate) fn google_jobs_query(query: &str) -> String {
    const FILLER: &[&str] = &["hiring", "job", "jobs", "posting", "postings"];
    query
        .split_whitespace()
        .filter(|word| {
            let lower = word.to_ascii_lowercase();
            !(FILLER.contains(&lower.as_str()) && word.chars().all(|c| c.is_ascii_alphabetic()))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Query shaping for structured job engines (SerpApi Google Jobs + Glassdoor):
/// the geography lives in `location`/`gl`, so the `in <place>` tail is dropped
/// (it zeroes results otherwise), and the query is trimmed to the title plus
/// one stack token — proven to return far more postings than the full
/// multi-skill string.
pub(crate) fn structured_query(query: &str) -> String {
    let cleaned = google_jobs_query(query);
    let without_place = match cleaned.find(" in ") {
        Some(at) => cleaned[..at].trim().to_string(),
        None => cleaned,
    };
    without_place
        .split_whitespace()
        .take(4)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse Google's relative posted dates: "Just posted", "Today",
/// "Yesterday", "3 days ago", "30+ days ago", "2 weeks ago", "5 months ago".
pub(crate) fn parse_relative_posted(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let lower = value.trim().to_ascii_lowercase();
    let now = chrono::Utc::now();
    if lower.contains("just posted") || lower == "today" {
        return Some(now);
    }
    if lower == "yesterday" {
        return Some(now - chrono::Duration::days(1));
    }
    let digits: String = lower.chars().take_while(|c| c.is_ascii_digit()).collect();
    let amount: i64 = digits.parse().ok()?;
    let days = if lower.contains("minute") || lower.contains("hour") {
        0
    } else if lower.contains("day") {
        amount
    } else if lower.contains("week") {
        amount * 7
    } else if lower.contains("month") {
        amount * 30
    } else if lower.contains("year") {
        amount * 365
    } else {
        return None;
    };
    Some(now - chrono::Duration::days(days))
}

impl SerpApiProvider {
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
        location: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        // SerpApi-specific query shaping: Google Jobs ranks clean role text
        // best, so filler words from the generic query builder are dropped
        // ("hiring", bare "job"/"posting"). Location bias + the matching
        // google_domain keep results in-country.
        let query = structured_query(query);
        let attempts = crate::discovery::rate_limit::max_attempts();
        let mut last_error = String::from("unknown SerpApi error");
        for attempt in 0..attempts {
            let _permit = crate::discovery::rate_limit::acquire("serpapi").await;
            let mut request = self
                .client
                .get("https://serpapi.com/search.json")
                .query(&[
                    ("engine", "google_jobs".to_string()),
                    ("q", query.clone()),
                    ("hl", "en".to_string()),
                    ("google_domain", google_domain_for(country_code).to_string()),
                    ("api_key", self.api_key.clone()),
                ]);
            if let Some(code) = country_code {
                request = request.query(&[("gl", code.to_ascii_lowercase())]);
                let place = location
                    .map(str::trim)
                    .filter(|place| !place.is_empty())
                    .map(|place| match country_name_for(code) {
                        Some(country) if !place.eq_ignore_ascii_case(country) => {
                            format!("{place}, {country}")
                        }
                        _ => place.to_string(),
                    })
                    .or_else(|| country_name_for(code).map(str::to_string));
                if let Some(place) = place {
                    request = request.query(&[("location", place)]);
                }
            }
            let _ = (limit, max_age_days);
            let response = request.send().await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    last_error = format!("SerpApi request failed: {error}");
                    if attempt + 1 < attempts {
                        let wait = crate::discovery::rate_limit::backoff_secs(attempt);
                        tracing::warn!(%error, attempt = attempt + 1, wait_secs = wait, "SerpApi request failed; retrying");
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    anyhow::bail!("SerpApi search failed: {error}");
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
                    format!("SerpApi search failed (429): {}", super::trim_err(&text));
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
                        "SerpApi rate limited (429); backing off"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
                anyhow::bail!("{last_error}");
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                anyhow::bail!(
                    "SerpApi search failed ({status} — bad key or quota): {}",
                    super::trim_err(&text)
                );
            }
            if !status.is_success() {
                anyhow::bail!("SerpApi search failed ({status}): {}", super::trim_err(&text));
            }
            let parsed: SerpApiResponse = serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("SerpApi response was not search JSON: {e}"))?;
            return Ok(parsed
                .jobs_results
                .into_iter()
                .filter_map(|item| {
                    let url = item
                        .apply_options
                        .into_iter()
                        .filter_map(|option| option.link)
                        .find(|link| !link.trim().is_empty())
                        .or(item.share_link)
                        .filter(|link| !link.trim().is_empty())?;
                    let snippet = match item.description.filter(|s| !s.trim().is_empty()) {
                        Some(full) => Some(full),
                        None => {
                            let highlights = item
                                .job_highlights
                                .into_iter()
                                .flat_map(|highlight| highlight.items)
                                .collect::<Vec<_>>()
                                .join(" ");
                            (!highlights.trim().is_empty()).then_some(highlights)
                        }
                    };
                    let published_at = item
                        .detected_extensions
                        .posted_at
                        .as_deref()
                        .and_then(parse_relative_posted)
                        .map(|date| date.to_rfc3339());
                    let mut title = item.title.unwrap_or_default();
                    if title.trim().is_empty() {
                        return None;
                    }
                    if let Some(company) = item.company_name.filter(|c| !c.trim().is_empty()) {
                        if !title.to_ascii_lowercase().contains(&company.to_ascii_lowercase()) {
                            title = format!("{title} at {company}");
                        }
                    }
                    Some(SearchResult {
                        title,
                        url,
                        snippet,
                        published_at,
                        source: "serpapi",
                    })
                })
                .collect());
        }
        anyhow::bail!("{last_error}")
    }
}

/// SerpApi Glassdoor Jobs provider (same `SERPAPI_API_KEY`).
///
/// Worldwide incl. India, 30 results per call, and first-class filters:
/// `date_posted` mirrors `max_age_days` (1/3/7/14/30), `remote=true` for
/// remote queries, city-level `location` bias. Returns skills, salary and
/// company rating per posting where Glassdoor has them.
#[derive(Clone)]
pub struct SerpApiGlassdoorProvider {
    client: Client,
    api_key: String,
}

fn glassdoor_field<'a>(item: &'a serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        item.get(*key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    })
}

/// Glassdoor `date_posted` bucket covering a day window (max 30).
fn glassdoor_date_posted(max_age_days: u32) -> &'static str {
    if max_age_days <= 1 {
        "1"
    } else if max_age_days <= 3 {
        "3"
    } else if max_age_days <= 7 {
        "7"
    } else if max_age_days <= 14 {
        "14"
    } else {
        "30"
    }
}

fn glassdoor_posted_at(item: &serde_json::Value) -> Option<String> {
    // Numeric "days since posted" first, then relative strings.
    if let Some(days) = item
        .get("days_ago")
        .or_else(|| item.get("days_since_posted"))
        .or_else(|| item.get("posted_days_ago"))
        .and_then(|value| value.as_u64())
    {
        return Some(
            (chrono::Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339(),
        );
    }
    let raw = glassdoor_field(
        item,
        &[
            "posted_date",
            "formatted_posted_date",
            "date_posted",
            "posted_at",
        ],
    )?;
    parse_relative_posted(&raw)
        .map(|date| date.to_rfc3339())
        .or_else(|| {
            crate::models::parse_posted_at(&raw).map(|date| date.to_rfc3339())
        })
}

impl SerpApiGlassdoorProvider {
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
        location: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        // Glassdoor is picky: "title + 2+ skills" returns nothing, while
        // "title + 1 skill" returns plenty. Try the 4-word shape first,
        // then fall back to 3 words before giving up.
        let cleaned = structured_query(query);
        let words: Vec<&str> = cleaned.split_whitespace().collect();
        let mut candidates: Vec<String> = vec![cleaned.clone()];
        if words.len() > 3 {
            candidates.push(words[..3].join(" "));
        }
        let mut last_error = String::new();
        for candidate in candidates {
            match self
                .fetch(&candidate, limit, country_code, max_age_days, location)
                .await
            {
                Ok(results) if !results.is_empty() => return Ok(results),
                Ok(_) => {
                    tracing::debug!(
                        query = candidate.as_str(),
                        "SerpApi Glassdoor returned no results; trying a shorter query"
                    );
                    last_error = format!("Glassdoor returned no results for \"{candidate}\"");
                }
                Err(error) => last_error = format!("{error}"),
            }
        }
        anyhow::bail!("{last_error}")
    }

    async fn fetch(
        &self,
        query: &str,
        limit: usize,
        country_code: Option<&str>,
        max_age_days: u32,
        location: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let attempts = crate::discovery::rate_limit::max_attempts();
        let mut last_error = String::from("unknown SerpApi Glassdoor error");
        for attempt in 0..attempts {
            let _permit = crate::discovery::rate_limit::acquire("serpapi").await;
            let mut request = self
                .client
                .get("https://serpapi.com/search.json")
                .query(&[
                    ("engine", "glassdoor_jobs".to_string()),
                    ("q", query.to_string()),
                    ("date_posted", glassdoor_date_posted(max_age_days).to_string()),
                    ("hl", "en".to_string()),
                    ("api_key", self.api_key.clone()),
                ]);
            if query.to_ascii_lowercase().contains("remote") {
                request = request.query(&[("remote", "true".to_string())]);
            }
            let place = location
                .map(str::trim)
                .filter(|place| !place.is_empty())
                .map(|place| match country_name_for(country_code.unwrap_or_default()) {
                    Some(country) if !place.eq_ignore_ascii_case(country) => {
                        format!("{place}, {country}")
                    }
                    _ => place.to_string(),
                })
                .or_else(|| country_name_for(country_code.unwrap_or_default()).map(str::to_string));
            if let Some(place) = place {
                request = request.query(&[("location", place)]);
            }
            if let Some(code) = country_code {
                request = request.query(&[("gl", code.to_ascii_lowercase())]);
            }
            let _ = limit;
            let response = request.send().await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    last_error = format!("SerpApi Glassdoor request failed: {error}");
                    if attempt + 1 < attempts {
                        let wait = crate::discovery::rate_limit::backoff_secs(attempt);
                        tracing::warn!(%error, attempt = attempt + 1, wait_secs = wait, "SerpApi Glassdoor request failed; retrying");
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        continue;
                    }
                    anyhow::bail!("SerpApi Glassdoor search failed: {error}");
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
                    format!("SerpApi Glassdoor search failed (429): {}", super::trim_err(&text));
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
                        "SerpApi Glassdoor rate limited (429); backing off"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
                anyhow::bail!("{last_error}");
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                anyhow::bail!(
                    "SerpApi Glassdoor search failed ({status} — bad key or quota): {}",
                    super::trim_err(&text)
                );
            }
            if !status.is_success() {
                anyhow::bail!(
                    "SerpApi Glassdoor search failed ({status}): {}",
                    super::trim_err(&text)
                );
            }
            let parsed: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("SerpApi Glassdoor response was not JSON: {e}"))?;
            let items = parsed
                .get("jobs_results")
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default();
            if items.is_empty() {
                // SerpApi returns HTTP 200 with an `error` field when the
                // query yields nothing ("no results"). Only auth/quota
                // problems are real failures; anything else is an empty
                // result set.
                let error_text = parsed
                    .get("error")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let fatal = error_text.contains("api key")
                    || error_text.contains("invalid")
                    || error_text.contains("quota")
                    || error_text.contains("unauthorized")
                    || error_text.contains("subscription");
                if fatal {
                    anyhow::bail!("SerpApi Glassdoor search failed: {error_text}");
                }
                tracing::debug!(
                    query = query,
                    error = %error_text,
                    "SerpApi Glassdoor returned no results"
                );
                return Ok(Vec::new());
            }
            return Ok(items
                .into_iter()
                .filter_map(|item| {
                    let url = glassdoor_field(
                        &item,
                        &["link", "job_link", "share_link", "apply_link"],
                    )
                    .filter(|url| {
                        reqwest::Url::parse(url)
                            .ok()
                            .and_then(|parsed| parsed.host_str().map(str::to_string))
                            .is_some()
                    })?;
                    let mut snippet = glassdoor_field(
                        &item,
                        &["description", "snippet", "job_description"],
                    );
                    if snippet.is_none() {
                        if let Some(skills) = item.get("skills").and_then(|value| {
                            value.as_array().map(|skills| {
                                skills
                                    .iter()
                                    .filter_map(|skill| skill.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                        }) {
                            snippet = (!skills.trim().is_empty())
                                .then(|| format!("Skills: {skills}"));
                        }
                    }
                    if let Some(pay) = glassdoor_field(
                        &item,
                        &["salary", "salary_range", "pay_range", "formatted_salary"],
                    ) {
                        let body = snippet.unwrap_or_default();
                        snippet = Some(if body.is_empty() {
                            format!("Pay: {pay}")
                        } else {
                            format!("Pay: {pay}\n{body}")
                        });
                    }
                    let published_at = glassdoor_posted_at(&item);
                    let mut title =
                        glassdoor_field(&item, &["title"]).filter(|t| !t.trim().is_empty())?;
                    if let Some(company) =
                        glassdoor_field(&item, &["company_name", "company"])
                            .filter(|c| !c.trim().is_empty())
                    {
                        if !title.to_ascii_lowercase().contains(&company.to_ascii_lowercase()) {
                            title = format!("{title} at {company}");
                        }
                    }
                    Some(SearchResult {
                        title,
                        url,
                        snippet,
                        published_at,
                        source: "serpapi-glassdoor",
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
    fn parses_relative_posted_dates() {
        let now = chrono::Utc::now();
        let days_ago = |text: &str| {
            let duration = now.signed_duration_since(parse_relative_posted(text).unwrap());
            (duration.num_seconds() as f64 / 86_400.0).round() as i64
        };
        assert_eq!(days_ago("Just posted"), 0);
        assert_eq!(days_ago("3 days ago"), 3);
        assert_eq!(days_ago("30+ days ago"), 30);
        assert!((days_ago("2 weeks ago") - 14).abs() <= 1);
        assert!(parse_relative_posted("sometime").is_none());
    }

    #[test]
    fn maps_country_codes() {
        assert_eq!(country_name_for("IN"), Some("India"));
        assert_eq!(country_name_for("us"), Some("United States"));
        assert_eq!(country_name_for("XX"), None);
    }

    #[test]
    fn google_jobs_query_drops_filler_words() {
        assert_eq!(
            google_jobs_query("hiring Senior Rust Engineer Rust Go job in Bengaluru Bangalore India"),
            "Senior Rust Engineer Rust Go in Bengaluru Bangalore India"
        );
        assert_eq!(
            google_jobs_query("Rust engineer job posting in Chennai India"),
            "Rust engineer in Chennai India"
        );
    }

    #[test]
    fn glassdoor_date_posted_covers_any_window() {
        assert_eq!(glassdoor_date_posted(1), "1");
        assert_eq!(glassdoor_date_posted(7), "7");
        assert_eq!(glassdoor_date_posted(30), "30");
    }

    #[test]
    fn glassdoor_reads_days_ago_and_strings() {
        let numeric = serde_json::json!({"days_ago": 5});
        assert!(glassdoor_posted_at(&numeric).is_some());
        let textual =
            serde_json::json!({"posted_date": "3 days ago"});
        assert!(glassdoor_posted_at(&textual).is_some());
        let empty = serde_json::json!({"title": "x"});
        assert!(glassdoor_posted_at(&empty).is_none());
    }

    #[test]
    fn structured_query_drops_place_and_trims() {
        assert_eq!(
            structured_query("hiring Senior Backend Engineer Go Rust Python job in Chennai India"),
            "Senior Backend Engineer Go"
        );
        assert_eq!(
            structured_query("Backend Engineer Go Rust Python in Bengaluru Bangalore India"),
            "Backend Engineer Go Rust"
        );
    }

    #[test]
    fn parses_google_jobs_response() {
        let parsed: SerpApiResponse = serde_json::from_str(
            r#"{"jobs_results":[
                {"title":"Rust Engineer","company_name":"Acme","location":"Bengaluru, India","description":"Build Rust services.","detected_extensions":{"posted_at":"2 days ago"},"apply_options":[{"title":"Apply","link":"https://example.com/j/1"}]},
                {"title":"Old job","company_name":"Acme","apply_options":[]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(parsed.jobs_results.len(), 2);
        assert_eq!(
            parsed.jobs_results[0]
                .detected_extensions
                .posted_at
                .as_deref(),
            Some("2 days ago")
        );
    }
}
