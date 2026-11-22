//! Official Greenhouse Job Board API. Configured board tokens and Greenhouse URLs
//! already saved in `data/job_sources.json` are fetched from boards-api.greenhouse.io.
//! This does not scrape the career site and does not submit applications.
use crate::models::{CandidateProfile, Job};
use crate::providers::JobProvider;
use async_trait::async_trait;
use base64::Engine;
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GreenhouseLink {
    pub token: String,
    pub job_id: Option<u64>,
}

#[derive(Debug, Clone)]
struct BoardTarget {
    token: String,
    name: Option<String>,
    job_id: Option<u64>,
}

pub struct GreenhouseProvider {
    boards_path: PathBuf,
    sources_path: PathBuf,
    db: sqlx::SqlitePool,
    client: Client,
    max_jobs: usize,
}

impl GreenhouseProvider {
    pub fn new(
        boards_path: impl Into<PathBuf>,
        sources_path: impl Into<PathBuf>,
        db: sqlx::SqlitePool,
    ) -> Self {
        let max_jobs = std::env::var("GREENHOUSE_MAX_JOBS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(40)
            .clamp(1, 100);
        let client = Client::builder()
            .user_agent("RustJobAgent/0.6 (Greenhouse job board API)")
            .timeout(Duration::from_secs(30))
            .build()
            .expect("valid reqwest client");
        Self {
            boards_path: boards_path.into(),
            sources_path: sources_path.into(),
            db,
            client,
            max_jobs,
        }
    }
}

#[async_trait]
impl JobProvider for GreenhouseProvider {
    fn name(&self) -> &'static str {
        "greenhouse-job-board"
    }

    async fn discover(&self) -> anyhow::Result<Vec<Job>> {
        let targets = load_targets(&self.boards_path, &self.sources_path).await?;
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let profile = crate::store::load_preferences(&self.db)
            .await
            .ok()
            .flatten();
        let mut jobs = Vec::new();
        let mut errors = Vec::new();
        for target in &targets {
            let fetched = if let Some(job_id) = target.job_id {
                fetch_job(&self.client, &target.token, job_id, target.name.as_deref())
                    .await
                    .map(|job| vec![job])
            } else {
                fetch_board(&self.client, target, &profile, self.max_jobs).await
            };
            match fetched {
                Ok(found) => jobs.extend(found),
                Err(err) => {
                    tracing::warn!(token = %target.token, error = %err, "Greenhouse board fetch failed");
                    errors.push(format!("{}: {err}", target.token));
                }
            }
        }
        if jobs.is_empty() && !errors.is_empty() {
            anyhow::bail!(errors.join("; "));
        }
        tracing::info!(
            count = jobs.len(),
            boards = targets.len(),
            "Greenhouse discovery finished"
        );
        Ok(jobs)
    }
}

pub fn parse_board_url(url: &str) -> Option<GreenhouseLink> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if host != "boards.greenhouse.io" && host != "job-boards.greenhouse.io" {
        return None;
    }
    let segments = parsed
        .path_segments()
        .map(|parts| parts.filter(|s| !s.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.first().copied() == Some("embed") {
        let token = parsed
            .query_pairs()
            .find(|(key, _)| key == "for")
            .map(|(_, value)| value.into_owned())?;
        if !is_board_token(&token) {
            return None;
        }
        let job_id = parsed
            .query_pairs()
            .find(|(key, _)| key == "token")
            .and_then(|(_, value)| value.parse().ok());
        return Some(GreenhouseLink { token, job_id });
    }
    let token = segments.first()?.to_string();
    if !is_board_token(&token) {
        return None;
    }
    let job_id = if segments.get(1).copied() == Some("jobs") {
        segments.get(2).and_then(|value| value.parse().ok())
    } else {
        None
    };
    Some(GreenhouseLink { token, job_id })
}

pub struct GreenhouseApplicant {
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub resume_filename: String,
    pub resume_bytes: Vec<u8>,
    pub cover_letter: String,
}

/// Provider-neutral applicant used by the Lever/Ashby adapters.
pub struct StandardApplicant {
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub current_company: String,
    pub resume_filename: String,
    pub resume_bytes: Vec<u8>,
    pub cover_letter: String,
}

impl StandardApplicant {
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name.trim(), self.last_name.trim())
            .trim()
            .to_string()
    }

    pub fn require_submittable(&self, provider: &str) -> anyhow::Result<()> {
        if !self.email.trim().contains('@') {
            anyhow::bail!("Add an email address before submitting a {provider} application");
        }
        if self.resume_bytes.is_empty() {
            anyhow::bail!("Upload a resume before submitting a {provider} application");
        }
        Ok(())
    }
}

/// Submits one application through Greenhouse's public job board API.
pub async fn submit_application(
    token: &str,
    job_id: u64,
    applicant: &GreenhouseApplicant,
) -> anyhow::Result<String> {
    if !is_board_token(token) {
        anyhow::bail!("Invalid Greenhouse board token");
    }
    let email = applicant.email.trim();
    if !email.contains('@') {
        anyhow::bail!("Add an email address before submitting a Greenhouse application");
    }
    if applicant.resume_bytes.is_empty() {
        anyhow::bail!("Upload a resume before submitting a Greenhouse application");
    }
    let mut body = serde_json::json!({
        "first_name": applicant.first_name,
        "last_name": applicant.last_name,
        "email": email,
        "resume_content": base64::engine::general_purpose::STANDARD.encode(&applicant.resume_bytes),
        "resume_content_filename": applicant.resume_filename,
        "cover_letter_text": applicant.cover_letter,
    });
    if let Some(phone) = applicant
        .phone
        .as_deref()
        .map(str::trim)
        .filter(|phone| !phone.is_empty())
    {
        body["phone"] = serde_json::json!(phone);
    }
    let client = Client::builder()
        .user_agent("RustJobAgent/0.6 (Greenhouse job board API)")
        .timeout(Duration::from_secs(30))
        .build()?;
    let url = format!("https://boards-api.greenhouse.io/v1/boards/{token}/jobs/{job_id}");
    let response = client.post(url).json(&body).send().await?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        anyhow::bail!(
            "Greenhouse application was not accepted (HTTP {status}): {}",
            clip(&text.split_whitespace().collect::<Vec<_>>().join(" "), 240)
        );
    }
    Ok(clip(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        240,
    ))
}

/// Loads one Greenhouse board and keeps only the requested job ids.
pub async fn fetch_jobs_by_id(
    client: &Client,
    token: &str,
    job_ids: &[u64],
) -> anyhow::Result<Vec<Job>> {
    if !is_board_token(token) || job_ids.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!("https://boards-api.greenhouse.io/v1/boards/{token}/jobs?content=true");
    let payload = get_json(client, &url).await?;
    let wanted = job_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let Some(entries) = payload.get("jobs").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    Ok(entries
        .iter()
        .filter(|value| {
            value
                .get("id")
                .and_then(Value::as_u64)
                .is_some_and(|id| wanted.contains(&id))
        })
        .filter_map(|value| job_from_value(token, value, None))
        .collect())
}

pub async fn fetch_job(
    client: &Client,
    token: &str,
    job_id: u64,
    company_hint: Option<&str>,
) -> anyhow::Result<Job> {
    if !is_board_token(token) {
        anyhow::bail!("Invalid Greenhouse board token");
    }
    let url =
        format!("https://boards-api.greenhouse.io/v1/boards/{token}/jobs/{job_id}?content=true");
    let value = get_json(client, &url).await?;
    let company = company_hint
        .map(str::to_string)
        .or_else(|| string_field(&value, "company_name"));
    job_from_value(token, &value, company.as_deref())
        .ok_or_else(|| anyhow::anyhow!("Greenhouse job {job_id} on {token} had no title"))
}

async fn fetch_board(
    client: &Client,
    target: &BoardTarget,
    profile: &Option<CandidateProfile>,
    max_jobs: usize,
) -> anyhow::Result<Vec<Job>> {
    let info_url = format!(
        "https://boards-api.greenhouse.io/v1/boards/{}",
        target.token
    );
    let company = match get_json(client, &info_url).await {
        Ok(value) => string_field(&value, "name").or_else(|| target.name.clone()),
        Err(err) => {
            tracing::warn!(token = %target.token, error = %err, "Greenhouse board name lookup failed");
            target.name.clone()
        }
    };
    let list_url = format!(
        "https://boards-api.greenhouse.io/v1/boards/{}/jobs?content=true",
        target.token
    );
    let payload = get_json(client, &list_url).await?;
    let entries = payload
        .get("jobs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut ranked = entries
        .iter()
        .filter_map(|value| job_from_value(&target.token, value, company.as_deref()))
        .map(|job| {
            let rank = relevance(profile.as_ref(), &job);
            (rank, job)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(ranked
        .into_iter()
        .take(max_jobs)
        .map(|(_, job)| job)
        .collect())
}

fn job_from_value(token: &str, value: &Value, company_hint: Option<&str>) -> Option<Job> {
    let id = value.get("id").and_then(Value::as_u64)?;
    let title = string_field(value, "title").filter(|s| !s.is_empty())?;
    let content = string_field(value, "content").unwrap_or_default();
    let plain = plain_text(&content);
    let department = named_list(value, "departments");
    let office = named_list(value, "offices");
    let mut description = String::new();
    if !department.is_empty() {
        description.push_str(&format!("Department: {department}. "));
    }
    if !office.is_empty() {
        description.push_str(&format!("Office: {office}. "));
    }
    description.push_str(&plain);
    let location = value
        .get("location")
        .and_then(|loc| match loc {
            Value::String(text) => Some(text.clone()),
            Value::Object(_) => string_field(loc, "name"),
            _ => None,
        })
        .filter(|s| !s.trim().is_empty());
    let company = string_field(value, "company_name")
        .or_else(|| company_hint.map(str::to_string))
        .unwrap_or_else(|| prettify_slug(token));
    let url = string_field(value, "absolute_url")
        .filter(|s| s.starts_with("https://"))
        .unwrap_or_else(|| format!("https://job-boards.greenhouse.io/{token}/jobs/{id}"));
    let text = format!(
        "{title} {} {description}",
        location.as_deref().unwrap_or("")
    );
    let (min_experience, max_experience) = experience_bounds(&text);
    let work_mode = text
        .to_lowercase()
        .contains("remote")
        .then(|| "Remote".to_string());
    Some(Job {
        external_id: format!("gh-{token}-{id}"),
        source: "greenhouse".into(),
        title,
        company,
        location,
        description: clip(description.trim(), 8_000),
        skills: Vec::new(),
        min_experience,
        max_experience,
        url,
        discovered_at: Utc::now(),
        published_at: string_field(value, "first_published")
            .or_else(|| string_field(value, "updated_at"))
            .as_deref()
            .and_then(crate::models::parse_posted_at),
        work_mode,
    })
}

fn relevance(profile: Option<&CandidateProfile>, job: &Job) -> u32 {
    let Some(profile) = profile else {
        return 0;
    };
    let title = job.title.to_lowercase();
    let text = format!("{title} {}", job.description.to_lowercase());
    let mut score = 0;
    if profile.preferred_titles.iter().any(|wanted| {
        let wanted = wanted.trim().to_lowercase();
        !wanted.is_empty()
            && (title.contains(&wanted)
                || wanted
                    .split_whitespace()
                    .filter(|word| word.len() > 3)
                    .any(|word| title.contains(word)))
    }) {
        score += 100;
    }
    score += profile
        .skills
        .iter()
        .filter(|skill| {
            let skill = skill.trim().to_lowercase();
            skill.len() > 2 && text.contains(&skill)
        })
        .count() as u32;
    if profile.preferred_locations.iter().any(|loc| {
        let loc = loc.trim().to_lowercase();
        !loc.is_empty()
            && job
                .location
                .as_deref()
                .unwrap_or("")
                .to_lowercase()
                .contains(&loc)
    }) {
        score += 20;
    }
    score
}

async fn load_targets(boards_path: &Path, sources_path: &Path) -> anyhow::Result<Vec<BoardTarget>> {
    let mut targets = Vec::new();
    if boards_path.exists() {
        let file: BoardsFile = serde_json::from_slice(&tokio::fs::read(boards_path).await?)?;
        for entry in file.boards {
            let (token, name) = match entry {
                BoardConfig::Token(token) => (token, None),
                BoardConfig::Named { token, name } => (token, name),
            };
            push_target(&mut targets, token, name, None);
        }
    }
    if let Ok(raw) = std::env::var("GREENHOUSE_BOARDS") {
        for token in raw
            .split([',', ' ', '\n'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            push_target(&mut targets, token.to_string(), None, None);
        }
    }
    if sources_path.exists() {
        let file: SourcesFile = serde_json::from_slice(&tokio::fs::read(sources_path).await?)?;
        for source in file.sources {
            if let Some(link) = parse_board_url(&source.url) {
                push_target(&mut targets, link.token, source.name, link.job_id);
            }
        }
    }
    Ok(collapse(targets))
}

fn push_target(
    targets: &mut Vec<BoardTarget>,
    token: String,
    name: Option<String>,
    job_id: Option<u64>,
) {
    let token = token.trim().to_string();
    if is_board_token(&token) {
        targets.push(BoardTarget {
            token,
            name,
            job_id,
        });
    } else if !token.is_empty() {
        tracing::warn!(token, "Skipping invalid Greenhouse board token");
    }
}

fn collapse(targets: Vec<BoardTarget>) -> Vec<BoardTarget> {
    let mut boards: BTreeMap<String, BoardTarget> = BTreeMap::new();
    let mut singles = Vec::new();
    for target in targets {
        if target.job_id.is_none() {
            boards
                .entry(target.token.clone())
                .and_modify(|existing| {
                    if existing.name.is_none() {
                        existing.name = target.name.clone();
                    }
                })
                .or_insert(target);
        } else {
            singles.push(target);
        }
    }
    singles.retain(|target| !boards.contains_key(&target.token));
    let mut single_ids = BTreeMap::new();
    for target in singles {
        single_ids
            .entry((target.token.clone(), target.job_id))
            .or_insert(target);
    }
    boards
        .into_values()
        .chain(single_ids.into_values())
        .collect()
}

async fn get_json(client: &Client, url: &str) -> anyhow::Result<Value> {
    let response = client.get(url).send().await?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        anyhow::bail!(
            "HTTP {status}: {}",
            clip(&text.split_whitespace().collect::<Vec<_>>().join(" "), 240)
        );
    }
    Ok(serde_json::from_str(&text)?)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn named_list(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| string_field(item, "name"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

pub fn is_board_token(token: &str) -> bool {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {
            token.len() <= 80 && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        }
        _ => false,
    }
}

fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn experience_bounds(text: &str) -> (Option<f32>, Option<f32>) {
    let range = regex::Regex::new(
        r"(?i)(\d+(?:\.\d+)?)\s*(?:-|to)\s*(\d+(?:\.\d+)?)\s*(?:\+\s*)?(?:years?|yrs?)",
    )
    .ok();
    if let Some(captures) = range.and_then(|re| re.captures(text)) {
        return (
            captures.get(1).and_then(|m| m.as_str().parse().ok()),
            captures.get(2).and_then(|m| m.as_str().parse().ok()),
        );
    }
    let one = regex::Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*\+?\s*(?:years?|yrs?)").ok();
    (
        one.and_then(|re| re.captures(text))
            .and_then(|c| c.get(1)?.as_str().parse().ok()),
        None,
    )
}

fn prettify_slug(value: &str) -> String {
    value
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn clip(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    value.chars().take(max).collect::<String>() + "…"
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SavedBoard {
    pub token: String,
    pub name: Option<String>,
}

pub async fn saved_boards(path: &Path) -> anyhow::Result<Vec<SavedBoard>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file: BoardsFile = serde_json::from_slice(&tokio::fs::read(path).await?)?;
    Ok(file
        .boards
        .into_iter()
        .filter_map(|entry| {
            let (token, name) = match entry {
                BoardConfig::Token(token) => (token, None),
                BoardConfig::Named { token, name } => (token, name),
            };
            is_board_token(&token).then_some(SavedBoard { token, name })
        })
        .collect())
}

pub async fn add_saved_board(
    path: &Path,
    token: &str,
    name: Option<String>,
) -> anyhow::Result<(bool, Vec<SavedBoard>)> {
    let token = token.trim();
    if !is_board_token(token) {
        anyhow::bail!("Enter a Greenhouse board token such as the company slug in job-boards.greenhouse.io/your-token");
    }
    let mut boards = saved_boards(path).await?;
    if boards.iter().any(|board| board.token == token) {
        return Ok((true, boards));
    }
    let name = name.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    });
    boards.push(SavedBoard {
        token: token.to_string(),
        name,
    });
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let file = BoardsWriteFile {
        boards: boards.clone(),
    };
    tokio::fs::write(path, serde_json::to_vec_pretty(&file)?).await?;
    Ok((false, boards))
}

#[derive(Deserialize)]
struct BoardsFile {
    #[serde(default)]
    boards: Vec<BoardConfig>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BoardConfig {
    Token(String),
    Named {
        token: String,
        #[serde(default)]
        name: Option<String>,
    },
}

#[derive(serde::Serialize)]
struct BoardsWriteFile {
    boards: Vec<SavedBoard>,
}

#[derive(Deserialize)]
struct SourcesFile {
    #[serde(default)]
    sources: Vec<SourceLink>,
}

#[derive(Deserialize)]
struct SourceLink {
    url: String,
    #[serde(default)]
    name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_board_and_job_urls() {
        let board = parse_board_url("https://job-boards.greenhouse.io/exampleai").unwrap();
        assert_eq!(board.token, "exampleai");
        assert_eq!(board.job_id, None);
        let job = parse_board_url("https://boards.greenhouse.io/exampleai/jobs/445566").unwrap();
        assert_eq!(job.job_id, Some(445566));
        let embed =
            parse_board_url("https://boards.greenhouse.io/embed/job_app?for=exampleai&token=99")
                .unwrap();
        assert_eq!(embed.token, "exampleai");
        assert_eq!(embed.job_id, Some(99));
        assert!(parse_board_url("https://careers.example.com/jobs/1").is_none());
    }

    #[test]
    fn maps_a_board_job() {
        let value = serde_json::json!({
            "id": 42,
            "title": "Machine Learning Engineer",
            "company_name": "Example AI",
            "absolute_url": "https://job-boards.greenhouse.io/exampleai/jobs/42",
            "location": {"name": "Remote - Bengaluru"},
            "content": "<p>Build models. 3-5 years experience with Python.</p>",
            "departments": [{"name": "Research"}],
            "offices": [{"name": "Bengaluru"}]
        });
        let job = job_from_value("exampleai", &value, None).unwrap();
        assert_eq!(job.external_id, "gh-exampleai-42");
        assert_eq!(job.company, "Example AI");
        assert_eq!(job.source, "greenhouse");
        assert!(job.description.contains("Python"));
        assert!(!job.description.contains("<p>"));
        assert_eq!(job.min_experience, Some(3.0));
        assert_eq!(job.max_experience, Some(5.0));
        assert_eq!(job.work_mode.as_deref(), Some("Remote"));
    }

    #[test]
    fn rejects_path_like_tokens() {
        assert!(!is_board_token("../admin"));
        assert!(!is_board_token(""));
        assert!(is_board_token("example_ai"));
    }

    #[tokio::test]
    async fn saves_a_board_once() {
        let path = std::env::temp_dir().join(format!(
            "rust-job-agent-boards-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let (duplicate, boards) = add_saved_board(&path, "exampleai", Some("Example AI".into()))
            .await
            .unwrap();
        assert!(!duplicate);
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].token, "exampleai");
        let (duplicate, boards) = add_saved_board(&path, "exampleai", None).await.unwrap();
        assert!(duplicate);
        assert_eq!(boards.len(), 1);
        assert!(add_saved_board(&path, "../admin", None).await.is_err());
        let _ = std::fs::remove_file(&path);
    }
}
