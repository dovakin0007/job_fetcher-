//! Keyless job providers: free, official JSON APIs that need no API key and
//! are always on. They cover discovery when no Exa/Tavily key is configured.
//!
//! - Remotive (`remotive.com/api/remote-jobs`): query per preferred title.
//! - Arbeitnow (`arbeitnow.com/api/job-board-api`): one fetch, filtered locally.
//!
//! Both respect the saved matching preferences and the search config
//! (country, max age) exactly like the keyed search providers.

use crate::models::{CandidateProfile, Job};
use crate::providers::JobProvider;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::time::Duration;

fn client() -> Client {
    Client::builder()
        .user_agent("RustJobAgent/0.6 (free job APIs)")
        .timeout(Duration::from_secs(30))
        .build()
        .expect("valid reqwest client")
}

fn max_jobs() -> usize {
    std::env::var("FREE_PROVIDER_MAX_JOBS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60)
        .clamp(1, 200)
}

async fn load_prefs(db: &SqlitePool) -> Option<CandidateProfile> {
    crate::store::load_preferences(db).await.ok().flatten()
}

/// Keep a posting when its title names a preferred role or a preference skill.
/// Server-side search already narrows results; this is the local pass used by
/// both providers (required for Arbeitnow, which has no query parameter).
fn relevant(profile: &CandidateProfile, title: &str, skill_hint: &str) -> bool {
    let title_lower = title.to_lowercase();
    if profile
        .preferred_titles
        .iter()
        .any(|t| !t.trim().is_empty() && title_lower.contains(&t.trim().to_lowercase()))
    {
        return true;
    }
    let hay = format!("{title} {skill_hint}").to_lowercase();
    profile.skills.iter().any(|s| {
        let s = s.trim().to_lowercase();
        s.len() >= 2 && hay.contains(&s)
    })
}

fn work_mode(location: Option<&str>) -> Option<String> {
    let loc = location.unwrap_or_default().to_lowercase();
    if loc.contains("remote") || loc.contains("anywhere") {
        Some("Remote".into())
    } else if loc.contains("hybrid") {
        Some("Hybrid".into())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Remotive
// ---------------------------------------------------------------------------

pub struct RemotiveProvider {
    db: SqlitePool,
    client: Client,
}

impl RemotiveProvider {
    pub fn new(db: SqlitePool) -> Self {
        Self {
            db,
            client: client(),
        }
    }
}

#[derive(serde::Deserialize)]
struct RemotiveResponse {
    #[serde(default)]
    jobs: Vec<RemotiveJob>,
}

#[derive(serde::Deserialize)]
struct RemotiveJob {
    id: u64,
    url: String,
    title: String,
    company_name: String,
    #[serde(default)]
    candidate_required_location: Option<String>,
    #[serde(default)]
    publication_date: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[async_trait]
impl JobProvider for RemotiveProvider {
    fn name(&self) -> &'static str {
        "remotive"
    }

    async fn discover(&self) -> anyhow::Result<Vec<Job>> {
        let Some(profile) = load_prefs(&self.db).await else {
            return Ok(Vec::new());
        };
        let titles: Vec<String> = profile
            .preferred_titles
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .take(6)
            .collect();
        if titles.is_empty() {
            return Ok(Vec::new());
        }
        let mut seen = HashSet::new();
        let mut jobs = Vec::new();
        for title in &titles {
            let response = self
                .client
                .get("https://remotive.com/api/remote-jobs")
                .query(&[("search", title), ("limit", &"25".to_string())])
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    tracing::warn!(%error, query = %title, "remotive request failed");
                    continue;
                }
            };
            if !response.status().is_success() {
                tracing::warn!(status = %response.status(), query = %title, "remotive search failed");
                continue;
            }
            let parsed: RemotiveResponse = match response.json().await {
                Ok(parsed) => parsed,
                Err(error) => {
                    tracing::warn!(%error, query = %title, "remotive response was not JSON");
                    continue;
                }
            };
            for item in parsed.jobs {
                if !seen.insert(item.id) {
                    continue;
                }
                if !relevant(&profile, &item.title, "") {
                    continue;
                }
                jobs.push(remotive_job(item, &profile));
            }
            if jobs.len() >= max_jobs() {
                break;
            }
        }
        jobs.truncate(max_jobs());
        Ok(jobs)
    }
}

fn remotive_job(item: RemotiveJob, profile: &CandidateProfile) -> Job {
    let description = crate::providers::strip_html(&item.description.unwrap_or_default());
    let text = format!("{} {description}", item.title);
    Job {
        external_id: format!("remotive-{}", item.id),
        source: "remotive".into(),
        title: item.title,
        company: item.company_name,
        location: item.candidate_required_location.clone(),
        description: text.chars().take(8_000).collect(),
        skills: crate::providers::skills_in_text(&text, &profile.skills),
        min_experience: None,
        max_experience: None,
        url: item.url,
        discovered_at: Utc::now(),
        published_at: item
            .publication_date
            .as_deref()
            .and_then(crate::models::parse_posted_at),
        work_mode: work_mode(item.candidate_required_location.as_deref()),
    }
}

// ---------------------------------------------------------------------------
// Arbeitnow
// ---------------------------------------------------------------------------

pub struct ArbeitnowProvider {
    db: SqlitePool,
    client: Client,
}

impl ArbeitnowProvider {
    pub fn new(db: SqlitePool) -> Self {
        Self {
            db,
            client: client(),
        }
    }
}

#[derive(serde::Deserialize)]
struct ArbeitnowResponse {
    #[serde(default)]
    data: Vec<ArbeitnowJob>,
}

#[derive(serde::Deserialize)]
struct ArbeitnowJob {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    company_name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    remote: bool,
    #[serde(default)]
    created_at: Option<i64>,
}

#[async_trait]
impl JobProvider for ArbeitnowProvider {
    fn name(&self) -> &'static str {
        "arbeitnow"
    }

    async fn discover(&self) -> anyhow::Result<Vec<Job>> {
        let Some(profile) = load_prefs(&self.db).await else {
            return Ok(Vec::new());
        };
        if profile.preferred_titles.iter().all(|t| t.trim().is_empty())
            && profile.skills.iter().all(|s| s.trim().len() < 2)
        {
            return Ok(Vec::new());
        }
        let response = self
            .client
            .get("https://www.arbeitnow.com/api/job-board-api")
            .send()
            .await?;
        if !response.status().is_success() {
            anyhow::bail!("arbeitnow fetch failed ({})", response.status());
        }
        let parsed: ArbeitnowResponse = response.json().await?;
        let mut seen = HashSet::new();
        let mut jobs = Vec::new();
        for item in parsed.data {
            if item.slug.is_empty() || !seen.insert(item.slug.clone()) {
                continue;
            }
            if !relevant(&profile, &item.title, &item.description) {
                continue;
            }
            jobs.push(arbeitnow_job(item, &profile));
            if jobs.len() >= max_jobs() {
                break;
            }
        }
        Ok(jobs)
    }
}

fn arbeitnow_job(item: ArbeitnowJob, profile: &CandidateProfile) -> Job {
    let description = crate::providers::strip_html(&item.description);
    let text = format!("{} {description}", item.title);
    let location = if item.location.trim().is_empty() && item.remote {
        "Remote".to_string()
    } else {
        item.location.clone()
    };
    Job {
        external_id: format!("arbeitnow-{}", item.slug),
        source: "arbeitnow".into(),
        title: item.title,
        company: if item.company_name.trim().is_empty() {
            "Unknown company".into()
        } else {
            item.company_name
        },
        location: (!location.trim().is_empty()).then_some(location),
        description: text.chars().take(8_000).collect(),
        skills: crate::providers::skills_in_text(&text, &profile.skills),
        min_experience: None,
        max_experience: None,
        url: item.url,
        discovered_at: Utc::now(),
        published_at: item
            .created_at
            .and_then(|ts| DateTime::from_timestamp(ts, 0).map(|dt| dt.with_timezone(&Utc))),
        work_mode: if item.remote {
            Some("Remote".into())
        } else {
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> CandidateProfile {
        CandidateProfile {
            name: "Test".into(),
            summary: String::new(),
            skills: vec!["Rust".into(), "Go".into()],
            experience_years: 4.0,
            preferred_titles: vec!["Backend Engineer".into()],
            role_keywords: vec![],
            preferred_locations: vec!["Remote".into()],
            excluded_keywords: vec![],
            minimum_match_score: 60.0,
            projects: vec![],
            preferred_work_modes: vec![],
            auto_apply: false,
            email: String::new(),
            phone: String::new(),
            skill_weights: vec![],
        }
    }

    #[test]
    fn relevance_needs_role_or_skill_in_title() {
        assert!(relevant(&profile(), "Senior Backend Engineer", ""));
        assert!(relevant(&profile(), "Rust Developer", ""));
        assert!(!relevant(&profile(), "Sales Manager", "sells software"));
    }

    #[test]
    fn remotive_item_maps_to_job() {
        let item = RemotiveJob {
            id: 1,
            url: "https://remotive.com/remote-jobs/1".into(),
            title: "Senior Backend Engineer".into(),
            company_name: "Acme".into(),
            candidate_required_location: Some("Remote, India".into()),
            publication_date: Some("2026-11-18T00:00:00Z".into()),
            description: Some("<p>Build <b>Rust</b> services.</p>".into()),
        };
        let job = remotive_job(item, &profile());
        assert_eq!(job.external_id, "remotive-1");
        assert_eq!(
            job.description,
            "Senior Backend Engineer Build Rust services."
        );
        assert!(job.skills.contains(&"Rust".to_string()));
        assert_eq!(job.work_mode.as_deref(), Some("Remote"));
    }

    #[test]
    fn arbeitnow_item_maps_to_job() {
        let item = ArbeitnowJob {
            slug: "abc".into(),
            company_name: "Beta".into(),
            title: "Backend Engineer (Go)".into(),
            description: "Build Go services.".into(),
            url: "https://www.arbeitnow.com/jobs/abc".into(),
            location: "".into(),
            remote: true,
            created_at: Some(1_763_000_000),
        };
        let job = arbeitnow_job(item, &profile());
        assert_eq!(job.external_id, "arbeitnow-abc");
        assert_eq!(job.location.as_deref(), Some("Remote"));
        assert!(job.published_at.is_some());
    }

    #[test]
    fn strips_html() {
        assert_eq!(
            crate::providers::strip_html("<p>Hi&nbsp;<b>there</b></p>"),
            "Hi there"
        );
    }
}
