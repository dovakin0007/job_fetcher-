//! Ashby application adapter (best-effort).
//!
//! Uses Ashby's public posting API: job URLs carry the board and posting id,
//! and applications go to the same posting-api endpoint the Ashby-hosted form
//! uses. Anything the API rejects (custom required questions, EEO, changed
//! shapes) surfaces as an error so the caller files the job as needs-manual.

use crate::providers::StandardApplicant;
use reqwest::multipart::{Form, Part};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AshbyLink {
    pub board: String,
    pub posting_id: String,
}

/// Parses `https://jobs.ashbyhq.com/{board}/{postingId}`.
pub fn parse_posting_url(url: &str) -> Option<AshbyLink> {
    let parsed = reqwest::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    if host != "jobs.ashbyhq.com" && host != "ashbyhq.com" {
        return None;
    }
    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|parts| parts.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    if segments.len() < 2 {
        return None;
    }
    let (board, posting_id) = (segments[0].to_string(), segments[1].to_string());
    if board.is_empty() || posting_id.is_empty() {
        return None;
    }
    Some(AshbyLink { board, posting_id })
}

/// Field map sent to the application endpoint (pure, unit-tested).
pub fn apply_fields(applicant: &StandardApplicant, posting_id: &str) -> Vec<(String, String)> {
    let mut fields = vec![
        ("jobPostingId".to_string(), posting_id.to_string()),
        ("name".to_string(), applicant.full_name()),
        ("email".to_string(), applicant.email.clone()),
        ("coverLetter".to_string(), applicant.cover_letter.clone()),
    ];
    if let Some(phone) = applicant
        .phone
        .as_deref()
        .map(str::trim)
        .filter(|phone| !phone.is_empty())
    {
        fields.push(("phone".to_string(), phone.to_string()));
    }
    fields
}

pub async fn submit_application(
    link: &AshbyLink,
    applicant: &StandardApplicant,
) -> anyhow::Result<String> {
    applicant.require_submittable("Ashby")?;
    let mut form = Form::new();
    for (name, value) in apply_fields(applicant, &link.posting_id) {
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
        .user_agent("RustJobAgent/0.6 (Ashby application)")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let response = client
        .post("https://api.ashbyhq.com/posting-api/job-application")
        .multipart(form)
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        anyhow::bail!(
            "Ashby application was not accepted (HTTP {status}): {}",
            clipped(&text)
        );
    }
    Ok(format!(
        "Submitted through the Ashby posting API. {}",
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

    #[test]
    fn parses_ashby_urls() {
        let link =
            parse_posting_url("https://jobs.ashbyhq.com/acme/01234567-89ab-cdef-0123-456789abcdef")
                .unwrap();
        assert_eq!(link.board, "acme");
        assert_eq!(link.posting_id, "01234567-89ab-cdef-0123-456789abcdef");
        assert!(parse_posting_url("https://jobs.ashbyhq.com/acme").is_none());
        assert!(parse_posting_url("https://example.com/jobs/1").is_none());
    }

    #[test]
    fn fields_carry_posting_and_identity() {
        let applicant = StandardApplicant {
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: "ada@example.com".into(),
            phone: None,
            current_company: String::new(),
            resume_filename: "resume.pdf".into(),
            resume_bytes: vec![1],
            cover_letter: "Hello.".into(),
        };
        let fields = apply_fields(&applicant, "post-1");
        let get = |name: &str| {
            fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        assert_eq!(get("jobPostingId"), "post-1");
        assert_eq!(get("name"), "Ada Lovelace");
        assert!(!fields.iter().any(|(key, _)| key == "phone"));
    }
}
