//! Conservative structured-data importer for public job pages that permit automated access.
//! Only explicitly configured URLs are fetched. robots.txt is checked before each page.
use crate::models::Job;
use crate::providers::JobProvider;
use async_trait::async_trait;
use chrono::Utc;
use scraper::{Html, Selector};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{hash_map::DefaultHasher, HashSet, VecDeque},
    hash::{Hash, Hasher},
    path::PathBuf,
    time::Duration,
};

#[derive(Debug, Deserialize)]
struct SourceConfig {
    sources: Vec<SourceEntry>,
}
#[derive(Debug, Deserialize, Clone)]
struct SourceEntry {
    url: String,
    #[serde(default)]
    name: Option<String>,
    /// `posting` (default) parses one URL; `listing` follows same-origin job links.
    #[serde(default)]
    kind: Option<String>,
    /// Maximum pages fetched for this source, including the listing page (1..=50).
    #[serde(default)]
    max_pages: Option<usize>,
}

pub struct PublicJobPageProvider {
    config_path: PathBuf,
    client: reqwest::Client,
}
impl PublicJobPageProvider {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("RustJobAgent/0.1 (+local job discovery; contact: user-configured)")
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("valid reqwest client");
        Self {
            config_path: path.into(),
            client,
        }
    }

    async fn robots_allows(&self, page: &reqwest::Url) -> anyhow::Result<bool> {
        let mut robots_url = page.clone();
        robots_url.set_path("/robots.txt");
        robots_url.set_query(None);
        robots_url.set_fragment(None);
        let response = self.client.get(robots_url).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(true);
        }
        if !response.status().is_success() {
            anyhow::bail!(
                "robots.txt could not be checked (HTTP {})",
                response.status()
            );
        }
        let body = response.text().await?;
        Ok(robots_permits(&body, page.path()))
    }

    async fn fetch_page(&self, url: &reqwest::Url) -> anyhow::Result<Option<String>> {
        if !self.robots_allows(url).await? {
            return Ok(None);
        }
        let response = self
            .client
            .get(url.clone())
            .send()
            .await?
            .error_for_status()?;
        // Do not follow cross-origin redirects, including redirect targets reached by reqwest.
        if !same_origin(url, response.url()) {
            anyhow::bail!("Cross-origin redirect refused");
        }
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains("text/html")
        {
            anyhow::bail!("Not an HTML page");
        }
        if response.content_length().is_some_and(|n| n > 2_000_000) {
            anyhow::bail!("Page too large");
        }
        let bytes = response.bytes().await?;
        if bytes.len() > 2_000_000 {
            anyhow::bail!("Page too large");
        }
        Ok(Some(String::from_utf8(bytes.to_vec())?))
    }
}

#[async_trait]
impl JobProvider for PublicJobPageProvider {
    fn name(&self) -> &'static str {
        "permitted-public-job-pages"
    }
    async fn discover(&self) -> anyhow::Result<Vec<Job>> {
        if !self.config_path.exists() {
            return Ok(Vec::new());
        }
        let cfg: SourceConfig = serde_json::from_slice(&tokio::fs::read(&self.config_path).await?)?;
        let mut jobs = Vec::new();
        let mut seen = HashSet::new();
        for source in cfg.sources {
            let root = match reqwest::Url::parse(&source.url) {
                Ok(u)
                    if matches!(u.scheme(), "http" | "https")
                        && u.username().is_empty()
                        && u.password().is_none() =>
                {
                    u
                }
                _ => {
                    tracing::warn!(url=%source.url, "Skipping invalid job source URL");
                    continue;
                }
            };
            // Only explicit, permitted sources. Do not use this against sites whose terms prohibit crawling.
            let listing = source.kind.as_deref() == Some("listing");
            let limit = if listing {
                source.max_pages.unwrap_or(15).clamp(1, 50)
            } else {
                1
            };
            if greenhouse_board(&root) {
                tracing::info!(url = %source.url, "Greenhouse URL left to the official job board provider");
                continue;
            }
            let mut queue = VecDeque::from([root.clone()]);
            let mut visited = HashSet::new();
            while let Some(url) = queue.pop_front() {
                if visited.len() >= limit {
                    break;
                }
                if !same_origin(&root, &url) || !visited.insert(url.to_string()) {
                    continue;
                }
                match self.fetch_page(&url).await {
                    Ok(Some(html)) => {
                        let source_name = source.name.as_deref().unwrap_or("public-career-page");
                        for job in parse_job_postings(&html, &url, source_name) {
                            if seen.insert(job.url.clone()) {
                                jobs.push(job);
                            }
                        }
                        if listing && visited.len() == 1 {
                            for link in extract_job_links(&html, &url) {
                                if same_origin(&root, &link)
                                    && !visited.contains(link.as_str())
                                    && queue.len() < limit * 4
                                {
                                    queue.push_back(link);
                                }
                            }
                        }
                    }
                    Ok(None) => tracing::info!(url=%url, "Skipped page disallowed by robots.txt"),
                    Err(err) => tracing::warn!(url=%url, error=%err, "Job source fetch failed"),
                }
                tokio::time::sleep(Duration::from_millis(1200)).await;
            }
        }
        Ok(jobs)
    }
}

fn robots_permits(body: &str, path: &str) -> bool {
    let mut applies = false;
    let mut rules: Vec<(bool, String)> = Vec::new();
    for raw in body.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if key == "user-agent" {
            if value == "*" {
                applies = true;
            } else if applies {
                applies = false;
            }
        } else if applies && (key == "allow" || key == "disallow") && !value.is_empty() {
            rules.push((key == "allow", value.to_string()));
        }
    }
    let mut best: Option<(usize, bool)> = None;
    for (allow, rule) in rules {
        if path.starts_with(&rule) && best.map(|b| rule.len() > b.0).unwrap_or(true) {
            best = Some((rule.len(), allow));
        }
    }
    best.map(|(_, allow)| allow).unwrap_or(true)
}

fn parse_job_page(html: &str, page_url: &reqwest::Url, source: &str) -> Option<Job> {
    parse_job_postings(html, page_url, source)
        .into_iter()
        .next()
}
fn parse_job_postings(html: &str, page_url: &reqwest::Url, source: &str) -> Vec<Job> {
    let doc = Html::parse_document(html);
    let Ok(script_sel) = Selector::parse("script[type='application/ld+json']") else {
        return vec![];
    };
    let mut postings = Vec::new();
    for script in doc.select(&script_sel) {
        let raw = script.text().collect::<String>();
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            let val = &value;
            collect_job_postings(val, &mut postings);
        }
    }
    postings
        .into_iter()
        .filter_map(|p| parse_posting(&p, page_url, source))
        .collect()
}
fn parse_posting(p: &Value, page_url: &reqwest::Url, source: &str) -> Option<Job> {
    let title = p.get("title")?.as_str()?.trim().to_string();
    let company = p
        .get("hiringOrganization")
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("Unknown company")
        .to_string();
    let description_raw = p.get("description").and_then(Value::as_str).unwrap_or("");
    let description = Html::parse_fragment(description_raw)
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if description.is_empty() {
        return None;
    }
    let location = extract_location(p.get("jobLocation"));
    let skills = extract_skills(p.get("skills").or_else(|| p.get("qualifications")));
    let url = p
        .get("url")
        .and_then(Value::as_str)
        .and_then(|s| page_url.join(s).ok())
        .filter(|u| same_origin(page_url, u))
        .unwrap_or_else(|| page_url.clone())
        .to_string();
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let external_id = format!("{}-{:x}", slug(&company), hasher.finish());
    let (min_experience, max_experience) = experience_range(&description);
    let published_at = p
        .get("datePosted")
        .and_then(Value::as_str)
        .and_then(crate::models::parse_posted_at);
    let job = Job {
        external_id,
        source: source.to_string(),
        title,
        company,
        location,
        description,
        skills,
        min_experience,
        max_experience,
        url,
        discovered_at: Utc::now(),
        published_at,
        work_mode: None,
    };
    crate::models::is_recent_open_posting(
        job.published_at,
        &format!("{} {}", job.title, job.description),
    )
    .then_some(job)
}

fn collect_job_postings(value: &Value, postings: &mut Vec<Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_job_postings(item, postings);
            }
        }

        Value::Object(object) => {
            if object.get("@type").is_some_and(|v| {
                v.as_str() == Some("JobPosting")
                    || v.as_array()
                        .is_some_and(|types| types.iter().any(|t| t.as_str() == Some("JobPosting")))
            }) {
                postings.push(value.clone());
            } else {
                for child in object.values() {
                    collect_job_postings(child, postings);
                }
            }
        }

        _ => {}
    }
}
fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}
fn extract_job_links(html: &str, base: &reqwest::Url) -> Vec<reqwest::Url> {
    let doc = Html::parse_document(html);
    let Ok(selector) = Selector::parse("a[href]") else {
        return vec![];
    };
    let mut seen = HashSet::new();
    doc.select(&selector)
        .filter_map(|a| {
            let href = a.value().attr("href")?;
            let link = base.join(href).ok()?;
            if !matches!(link.scheme(), "https" | "http") || !same_origin(base, &link) {
                return None;
            }
            let path = link.path().to_ascii_lowercase();
            let anchor_text = a.text().collect::<String>().to_ascii_lowercase();
            // Intentionally conservative; configurable adapters can add site-specific selectors later.
            let looks_like_job = [
                "/job/",
                "/jobs/",
                "/position/",
                "/positions/",
                "/opening/",
                "/openings/",
                "/vacancy/",
            ]
            .iter()
            .any(|p| path.contains(p))
                || (anchor_text.contains("apply") && path != base.path());
            if looks_like_job && seen.insert(link.to_string()) {
                Some(link)
            } else {
                None
            }
        })
        .collect()
}
fn is_job_posting(v: &Value) -> bool {
    v.as_str().map(|s| s == "JobPosting").unwrap_or(false)
        || v.as_array()
            .map(|a| a.iter().any(|x| x.as_str() == Some("JobPosting")))
            .unwrap_or(false)
}
fn extract_location(v: Option<&Value>) -> Option<String> {
    let v = v?;
    let arr = v.as_array().cloned().unwrap_or_else(|| vec![v.clone()]);
    let mut parts = Vec::new();
    for item in arr {
        let address = item.get("address").unwrap_or(&item);
        for key in ["addressLocality", "addressRegion", "addressCountry"] {
            if let Some(s) = address.get(key).and_then(|x| x.as_str()) {
                if !parts.iter().any(|p| p == s) {
                    parts.push(s.to_string());
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}
fn extract_skills(v: Option<&Value>) -> Vec<String> {
    let Some(v) = v else { return vec![] };
    let raw = if let Some(s) = v.as_str() {
        s.split(&[',', ';', '\n'][..])
            .map(str::trim)
            .map(str::to_string)
            .collect()
    } else if let Some(a) = v.as_array() {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    } else {
        vec![]
    };
    raw.into_iter().filter(|s| !s.is_empty()).take(40).collect()
}
fn experience_range(text: &str) -> (Option<f32>, Option<f32>) {
    let re = regex::Regex::new(
        r"(?i)(\d+(?:\.\d+)?)\s*(?:-|to)\s*(\d+(?:\.\d+)?)\s*(?:\+\s*)?(?:years?|yrs?)",
    )
    .ok();
    if let Some(c) = re.and_then(|r| r.captures(text)) {
        return (
            c.get(1).and_then(|m| m.as_str().parse().ok()),
            c.get(2).and_then(|m| m.as_str().parse().ok()),
        );
    }
    let one =
        regex::Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*\+?\s*(?:years?|yrs?)\s+(?:of\s+)?experience")
            .ok();
    (
        one.and_then(|r| r.captures(text))
            .and_then(|c| c.get(1)?.as_str().parse().ok()),
        None,
    )
}
fn greenhouse_board(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str()
            .map(|host| host.to_ascii_lowercase())
            .as_deref(),
        Some("boards.greenhouse.io") | Some("job-boards.greenhouse.io")
    )
}

fn slug(s: &str) -> String {
    s.to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_schema_job_posting() {
        let posted = (chrono::Utc::now() - chrono::Duration::days(1)).format("%Y-%m-%d");
        let html = format!(
            r#"<script type='application/ld+json'>{{"@context":"https://schema.org","@type":"JobPosting","title":"Rust Engineer","datePosted":"{posted}","description":"<p>5+ years experience with Rust and Tokio</p>","hiringOrganization":{{"name":"Example Co"}},"jobLocation":{{"address":{{"addressLocality":"Bengaluru","addressCountry":"IN"}}}},"skills":"Rust, Tokio","url":"https://example.com/jobs/1"}}</script>"#
        );
        let url = reqwest::Url::parse("https://example.com/jobs/1").unwrap();
        let j = parse_job_page(&html, &url, "test").unwrap();
        assert_eq!(j.title, "Rust Engineer");
        assert_eq!(j.company, "Example Co");
        assert_eq!(j.skills, vec!["Rust", "Tokio"]);
        assert_eq!(j.min_experience, Some(5.0));
    }
    #[test]
    fn robots_longest_rule_wins() {
        assert!(!robots_permits(
            "User-agent: *\nDisallow: /jobs\nAllow: /jobs/public",
            "/jobs/private"
        ));
        assert!(robots_permits(
            "User-agent: *\nDisallow: /jobs\nAllow: /jobs/public",
            "/jobs/public/1"
        ));
    }
}

#[cfg(test)]
mod listing_tests {
    use super::*;
    #[test]
    fn finds_same_origin_job_links_only() {
        let base = reqwest::Url::parse("https://example.com/careers").unwrap();
        let html = r#"<a href='/jobs/rust-engineer'>Rust</a><a href='https://evil.example/jobs/fake'>Fake</a><a href='/about'>About</a>"#;
        let links = extract_job_links(html, &base);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].as_str(), "https://example.com/jobs/rust-engineer");
    }
    #[test]
    fn parses_multiple_jsonld_postings() {
        let base = reqwest::Url::parse("https://example.com/careers").unwrap();
        let posted = (chrono::Utc::now() - chrono::Duration::days(1)).format("%Y-%m-%d");
        let html = format!(
            r#"<script type='application/ld+json'>{{"@graph":[{{"@type":"JobPosting","title":"Rust Engineer","datePosted":"{posted}","description":"Rust role","hiringOrganization":{{"name":"Example"}},"url":"/jobs/rust"}},{{"@type":"JobPosting","title":"Go Engineer","datePosted":"{posted}","description":"Go role","hiringOrganization":{{"name":"Example"}},"url":"/jobs/go"}}]}}</script>"#
        );
        assert_eq!(parse_job_postings(&html, &base, "test").len(), 2);
    }
}
