use crate::models::Job;
use async_trait::async_trait;
use std::{path::PathBuf, sync::Arc};

#[async_trait]
pub trait JobProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn discover(&self) -> anyhow::Result<Vec<Job>>;
}
pub struct JsonImportProvider {
    path: PathBuf,
}
impl JsonImportProvider {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}
#[async_trait]
impl JobProvider for JsonImportProvider {
    fn name(&self) -> &'static str {
        "json-import"
    }
    async fn discover(&self) -> anyhow::Result<Vec<Job>> {
        if !self.path.exists() {
            return Ok(vec![]);
        };
        Ok(serde_json::from_slice(&tokio::fs::read(&self.path).await?)?)
    }
}

/// Submission adapters must be backed by an authorized API/integration. The default
/// implementation intentionally refuses to submit anything.
#[async_trait]
pub trait ApplicationProvider: Send + Sync {
    async fn submit(&self, _job: &Job, _answers: &serde_json::Value) -> anyhow::Result<String>;
}
pub struct DisabledSubmissionProvider;
#[async_trait]
impl ApplicationProvider for DisabledSubmissionProvider {
    async fn submit(&self, _: &Job, _: &serde_json::Value) -> anyhow::Result<String> {
        anyhow::bail!("No authorized application provider is configured. Review and apply on the employer/platform site.")
    }
}
pub type SharedProviders = Arc<Vec<Arc<dyn JobProvider>>>;

pub mod ashby;
pub mod free;
pub mod greenhouse;
pub mod lever;
pub mod web_pages;
pub use free::{ArbeitnowProvider, RemotiveProvider};
pub use greenhouse::{GreenhouseProvider, StandardApplicant};
pub use web_pages::PublicJobPageProvider;

/// Strip HTML tags/entities down to plain text. Shared by the Tavily raw-content
/// fallback and the keyless providers whose APIs return HTML descriptions.
pub(crate) fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                // Keep block boundaries separated once tags are gone.
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Preference skills named anywhere in `text` (cap 30). Shared by search
/// enrichment and the keyless providers.
pub(crate) fn skills_in_text(text: &str, skills: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for skill in skills {
        let skill = skill.trim();
        if skill.len() < 2 || found.iter().any(|s: &String| s.eq_ignore_ascii_case(skill)) {
            continue;
        }
        if mentions(text, skill) {
            found.push(skill.to_string());
        }
        if found.len() >= 30 {
            break;
        }
    }
    found
}

fn mentions(text: &str, skill: &str) -> bool {
    let skill = normalize_phrase(skill);
    if skill.is_empty() {
        return false;
    }
    let text = format!(" {} ", normalize_phrase(text));
    text.contains(&format!(" {skill} "))
}

fn normalize_phrase(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
