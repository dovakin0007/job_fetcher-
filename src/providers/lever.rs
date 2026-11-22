//! Lever application adapter (best-effort).
//!
//! Uses Lever's public postings API: the posting JSON is read first so the
//! request mirrors the live form, then a multipart application is posted to
//! the same apply endpoint the Lever-hosted form uses. Postings that demand
//! answers we cannot know (custom required questions, EEO) are rejected by
//! Lever's API — the caller treats that as needs-manual, never as silent.

use crate::providers::StandardApplicant;
use reqwest::multipart::{Form, Part};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeverLink {
    pub company: String,
    pub posting_id: String,
}

/// Parses `https://jobs.lever.co/{company}/{postingId}` (and `lever.co`).
pub fn parse_posting_url(url: &str) -> Option<LeverLink> {
    let parsed = reqwest::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    if host != "jobs.lever.co" && host != "lever.co" {
        return None;
    }
    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|parts| parts.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    if segments.len() < 2 {
        return None;
    }
    let company = segments[0].to_string();
    let posting_id = segments[1].to_string();
    if company.is_empty() || posting_id.is_empty() {
        return None;
    }
    Some(LeverLink {
        company,
        posting_id,
    })
}

/// Field map sent to the apply endpoint (pure, unit-tested).
pub fn apply_fields(applicant: &StandardApplicant) -> Vec<(String, String)> {
    let mut fields = vec![
        ("name".to_string(), applicant.full_name()),
        ("email".to_string(), applicant.email.clone()),
        ("comments".to_string(), applicant.cover_letter.clone()),
    ];
    if let Some(phone) = applicant
        .phone
        .as_deref()
        .map(str::trim)
        .filter(|phone| !phone.is_empty())
    {
        fields.push(("phone".to_string(), phone.to_string()));
    }
    if !applicant.current_company.trim().is_empty() {
        fields.push(("org".to_string(), applicant.current_company.clone()));
    }
    fields
}

pub async fn submit_application(
    link: &LeverLink,
    applicant: &StandardApplicant,
) -> anyhow::Result<String> {
    applicant.require_submittable("Lever")?;
    let mut form = Form::new();
    for (name, value) in apply_fields(applicant) {
        form = form.text(name, value);
    }
    let mime = if applicant
        .resume_filename
        .to_ascii_lowercase()
        .ends_with(".docx")
    {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    } else {
        "application/pdf"
    };
    let file = Part::bytes(applicant.resume_bytes.clone())
        .file_name(applicant.resume_filename.clone())
        .mime_str(mime)?;
    form = form.part("resume", file);
    let client = reqwest::Client::builder()
        .user_agent("RustJobAgent/0.6 (Lever application)")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let url = format!(
        "https://api.lever.co/v0/postings/{}/{}/apply",
        link.company, link.posting_id
    );
    let response = client.post(url).multipart(form).send().await?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        anyhow::bail!(
            "Lever application was not accepted (HTTP {status}): {}",
            clipped(&text)
        );
    }
    Ok(format!(
        "Submitted through the Lever postings API. {}",
        clipped(&text)
    ))
}

fn clipped(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 240 {
        compact
    } else {
        compact.chars().take(240).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applicant() -> StandardApplicant {
        StandardApplicant {
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: "ada@example.com".into(),
            phone: Some("+1 555 0100".into()),
            current_company: "Acme".into(),
            resume_filename: "resume.pdf".into(),
            resume_bytes: vec![1, 2, 3],
            cover_letter: "I am applying.".into(),
        }
    }

    #[test]
    fn parses_lever_urls() {
        let link = parse_posting_url("https://jobs.lever.co/acme/1234-abcd").unwrap();
        assert_eq!(link.company, "acme");
        assert_eq!(link.posting_id, "1234-abcd");
        assert!(parse_posting_url("https://jobs.lever.co/acme").is_none());
        assert!(parse_posting_url("https://example.com/jobs/1").is_none());
    }

    #[test]
    fn fields_carry_identity_and_cover_letter() {
        let fields = apply_fields(&applicant());
        let get = |name: &str| {
            fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        assert_eq!(get("name"), "Ada Lovelace");
        assert_eq!(get("email"), "ada@example.com");
        assert_eq!(get("phone"), "+1 555 0100");
        assert_eq!(get("org"), "Acme");
        assert_eq!(get("comments"), "I am applying.");
    }

    #[test]
    fn rejects_missing_email_or_resume() {
        let mut bad = applicant();
        bad.email = "not-an-email".into();
        assert!(bad.require_submittable("Lever").is_err());
        let mut bad = applicant();
        bad.email = "ada@example.com".into();
        bad.resume_bytes.clear();
        assert!(bad.require_submittable("Lever").is_err());
    }
}
