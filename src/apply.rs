use crate::models::{CandidateProfile, Job};
use crate::providers::greenhouse::{self, GreenhouseApplicant};
use crate::providers::{ashby, lever, StandardApplicant};
use crate::resume::CandidateProfile as ResumeProfile;
use crate::store;
use crate::AppState;

#[derive(Debug, Clone, Copy, Default)]
pub struct AutoApplyReport {
    pub drafted: usize,
    pub submitted: usize,
    pub needs_manual: usize,
    pub skipped: usize,
}

/// Drafts an application for every job at or above the minimum score.
/// Greenhouse/Lever/Ashby postings are submitted through their official APIs
/// (status moves to `applied`) when an email address and resume file are
/// available. Every other site stays as a draft: approve it in the dashboard,
/// fill it with the browser extension, submit, and the extension reports back
/// (`applied_manually`) — that is the universal path. Recruiter replies picked
/// up by email sync later advance `interview` / `offer` / `rejected`.
pub async fn auto_apply(
    state: &AppState,
    prefs: &CandidateProfile,
) -> anyhow::Result<AutoApplyReport> {
    let jobs = store::jobs(&state.db).await?;
    let config = store::load_search_config(&state.db).await?;
    let resume = state.profile.read().await.clone();
    let experience = crate::matching::experience_text(resume.as_ref());
    let indexed: Vec<_> = jobs.iter().map(|result| &result.job).collect();
    let ranked =
        crate::matching::rank_jobs_with_resume(prefs, resume.as_ref(), &indexed, &experience);
    let mut score_of = std::collections::HashMap::new();
    for item in ranked {
        score_of.insert(indexed[item.position].external_id.clone(), item.breakdown);
    }
    let mut report = AutoApplyReport::default();
    for result in jobs {
        let scored = score_of
            .get(&result.job.external_id)
            .copied()
            .unwrap_or_default();
        if scored.excluded || scored.total < prefs.minimum_match_score {
            report.skipped += 1;
            continue;
        }
        if !crate::models::is_applicable_posting(
            result.job.published_at,
            &format!(
                "{} {} {}",
                result.job.title,
                result.job.location.as_deref().unwrap_or(""),
                result.job.description
            ),
            &result.job.url,
            &config,
        ) {
            continue;
        }
        if store::has_application(&state.db, &result.job.external_id).await? {
            report.skipped += 1;
            continue;
        }
        let matched = &result.matched_skills;
        let bullets = resume
            .as_ref()
            .map(|profile| supporting_bullets(profile, &result.job, matched))
            .unwrap_or_default();
        let summary = application_summary(&result.job, &bullets);
        let application =
            store::create_application(&state.db, &result.job, summary.clone(), bullets).await?;
        report.drafted += 1;
        match submit_if_supported(state, prefs, &resume, &result.job, &summary).await {
            Ok(Some(note)) => {
                store::update_application(&state.db, &application.id, "applied", &note).await?;
                report.submitted += 1;
            }
            Ok(None) => {
                let note = manual_reason(prefs, &resume, &result.job);
                store::update_application(&state.db, &application.id, "pending_review", &note)
                    .await?;
                report.needs_manual += 1;
            }
            Err(error) => {
                let note = format!("Submission failed: {error}");
                store::update_application(&state.db, &application.id, "pending_review", &note)
                    .await?;
                report.needs_manual += 1;
                tracing::warn!(job = %result.job.external_id, error = %error, "auto-apply submission failed");
            }
        }
    }
    Ok(report)
}

/// Tries each supported application API in turn: Greenhouse, then Lever,
/// then Ashby. Returns the submission note, or None when the URL matches no
/// supported board (the job stays a manual draft).
async fn submit_if_supported(
    state: &AppState,
    prefs: &CandidateProfile,
    resume: &Option<ResumeProfile>,
    job: &Job,
    summary: &str,
) -> anyhow::Result<Option<String>> {
    if let Some(note) = submit_if_greenhouse(state, prefs, resume, job, summary).await? {
        return Ok(Some(note));
    }
    if let Some(note) = submit_if_lever(state, prefs, resume, job, summary).await? {
        return Ok(Some(note));
    }
    if let Some(note) = submit_if_ashby(state, prefs, resume, job, summary).await? {
        return Ok(Some(note));
    }
    Ok(None)
}

async fn standard_applicant(
    state: &AppState,
    prefs: &CandidateProfile,
    resume: &Option<ResumeProfile>,
    cover_letter: &str,
) -> Option<StandardApplicant> {
    if prefs.email.trim().is_empty() {
        return None;
    }
    let profile = resume.as_ref()?;
    let path = profile.source_file.as_deref()?;
    let bytes = tokio::fs::read(state.upload_dir.join(file_name(path)))
        .await
        .ok()?;
    if bytes.is_empty() {
        return None;
    }
    let (first_name, last_name) = split_name(prefs.name.as_str());
    Some(StandardApplicant {
        first_name,
        last_name,
        email: prefs.email.clone(),
        phone: Some(prefs.phone.clone()).filter(|phone| !phone.trim().is_empty()),
        current_company: profile
            .experience
            .first()
            .map(|role| role.company.clone())
            .unwrap_or_default(),
        resume_filename: file_name(path).to_string(),
        resume_bytes: bytes,
        cover_letter: cover_letter.to_string(),
    })
}

async fn submit_if_lever(
    state: &AppState,
    prefs: &CandidateProfile,
    resume: &Option<ResumeProfile>,
    job: &Job,
    summary: &str,
) -> anyhow::Result<Option<String>> {
    let Some(link) = lever::parse_posting_url(&job.url) else {
        return Ok(None);
    };
    let Some(applicant) = standard_applicant(state, prefs, resume, summary).await else {
        return Ok(None);
    };
    Ok(Some(lever::submit_application(&link, &applicant).await?))
}

async fn submit_if_ashby(
    state: &AppState,
    prefs: &CandidateProfile,
    resume: &Option<ResumeProfile>,
    job: &Job,
    summary: &str,
) -> anyhow::Result<Option<String>> {
    let Some(link) = ashby::parse_posting_url(&job.url) else {
        return Ok(None);
    };
    let Some(applicant) = standard_applicant(state, prefs, resume, summary).await else {
        return Ok(None);
    };
    Ok(Some(ashby::submit_application(&link, &applicant).await?))
}
async fn submit_if_greenhouse(
    state: &AppState,
    prefs: &CandidateProfile,
    resume: &Option<ResumeProfile>,
    job: &Job,
    summary: &str,
) -> anyhow::Result<Option<String>> {
    let Some(link) = greenhouse::parse_board_url(&job.url) else {
        return Ok(None);
    };
    let Some(job_id) = link.job_id else {
        return Ok(None);
    };
    if prefs.email.trim().is_empty() {
        return Ok(None);
    }
    let Some(profile) = resume else {
        return Ok(None);
    };
    let Some(path) = profile.source_file.as_deref() else {
        return Ok(None);
    };
    let bytes = tokio::fs::read(state.upload_dir.join(file_name(path))).await?;
    let (first_name, last_name) = split_name(prefs.name.as_str());
    let filename = file_name(path).to_string();
    let note = greenhouse::submit_application(
        &link.token,
        job_id,
        &GreenhouseApplicant {
            first_name,
            last_name,
            email: prefs.email.clone(),
            phone: Some(prefs.phone.clone()).filter(|phone| !phone.trim().is_empty()),
            resume_filename: filename,
            resume_bytes: bytes,
            cover_letter: summary.to_string(),
        },
    )
    .await?;
    Ok(Some(format!(
        "Submitted through the Greenhouse job board API. {note}"
    )))
}

fn manual_reason(prefs: &CandidateProfile, resume: &Option<ResumeProfile>, job: &Job) -> String {
    if greenhouse::parse_board_url(&job.url)
        .and_then(|link| link.job_id)
        .is_some()
    {
        if prefs.email.trim().is_empty() {
            return "Draft only. Add an email address to submit Greenhouse applications.".into();
        }
        if resume
            .as_ref()
            .and_then(|profile| profile.source_file.as_ref())
            .is_none()
        {
            return "Draft only. Upload a resume file before submitting Greenhouse applications."
                .into();
        }
    }
    if lever::parse_posting_url(&job.url).is_some() || ashby::parse_posting_url(&job.url).is_some()
    {
        if prefs.email.trim().is_empty() {
            return "Draft only. Add an email address to submit Lever/Ashby applications.".into();
        }
        if resume
            .as_ref()
            .and_then(|profile| profile.source_file.as_ref())
            .is_none()
        {
            return "Draft only. Upload a resume file before submitting Lever/Ashby applications."
                .into();
        }
    }
    "Draft only. This site has no authorized application API, so submit it yourself from the job page.".into()
}

pub fn supporting_bullets(profile: &ResumeProfile, job: &Job, matched: &[String]) -> Vec<String> {
    let job_text = format!("{} {} {}", job.title, job.description, job.skills.join(" "));
    let skills: Vec<&str> = matched
        .iter()
        .map(String::as_str)
        .filter(|skill| has_term(&job_text, skill))
        .collect();
    let mut ranked: Vec<(usize, usize, String)> = profile
        .experience
        .iter()
        .flat_map(|item| item.bullets.iter())
        .enumerate()
        .filter_map(|(index, bullet)| {
            let hits = skills
                .iter()
                .filter(|skill| has_term(&bullet.text, skill))
                .count();
            (hits > 0).then(|| (hits, index, bullet.text.clone()))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked
        .into_iter()
        .take(4)
        .map(|(_, _, text)| text)
        .collect()
}

pub fn application_summary(job: &Job, bullets: &[String]) -> String {
    let opening = format!(
        "I am applying for the {} role at {}.",
        job.title.trim(),
        job.company.trim()
    );
    if bullets.is_empty() {
        return format!(
            "{opening}\n\nNone of my resume lines mention the skills in this posting, so this draft does not claim any."
        );
    }
    let body = bullets
        .iter()
        .take(4)
        .map(|line| line.trim().trim_end_matches('.'))
        .collect::<Vec<_>>()
        .join(". ");
    format!("{opening}\n\n{body}.")
}

fn has_term(text: &str, term: &str) -> bool {
    let term = normalize(term);
    if term.len() < 2 || STOP_TERMS.iter().any(|word| *word == term) {
        return false;
    }
    let text = format!(" {} ", normalize(text));
    text.contains(&format!(" {term} "))
}

fn normalize(value: &str) -> String {
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

const STOP_TERMS: &[&str] = &[
    "code",
    "design",
    "test",
    "tests",
    "testing",
    "software",
    "engineer",
    "engineering",
    "developer",
    "development",
    "senior",
    "team",
    "experience",
    "technical",
    "production",
    "application",
    "applications",
    "system",
    "systems",
    "data",
    "build",
    "built",
    "using",
    "work",
    "working",
    "role",
    "project",
    "projects",
    "support",
    "management",
];

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

pub fn split_name(name: &str) -> (String, String) {
    let mut parts = name.split_whitespace();
    let first = parts.next().unwrap_or("Candidate").to_string();
    let rest = parts.collect::<Vec<_>>().join(" ");
    if rest.is_empty() {
        (first, "Candidate".into())
    } else {
        (first, rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_uses_only_lines_that_name_the_job_skills() {
        use crate::resume::{EvidenceBullet, Experience};
        let profile = ResumeProfile {
            name: None,
            headline: None,
            summary: None,
            skills: vec![],
            experience: vec![Experience {
                company: "Lab".into(),
                title: "Engineer".into(),
                dates: None,
                location: None,
                bullets: vec![
                    EvidenceBullet {
                        text: "Migrated MATLAB models to Rust for ADAS modules.".into(),
                        source_excerpt: String::new(),
                    },
                    EvidenceBullet {
                        text: "Built React and Java services for checkout.".into(),
                        source_excerpt: String::new(),
                    },
                ],
            }],
            projects: vec![],
            education: vec![],
            open_source: vec![], layout: vec![], custom_fields: vec![],
            source_file: None,
            extraction_warnings: vec![],
        };
        let job = Job {
            external_id: "1".into(),
            source: "test".into(),
            title: "Sr Software Engineer (React / Java)".into(),
            company: "Paypal".into(),
            location: None,
            description: "React and Java services.".into(),
            skills: vec!["React".into(), "Java".into()],
            min_experience: None,
            max_experience: None,
            url: "https://example.test/job".into(),
            discovered_at: chrono::Utc::now(),
            published_at: None,
            work_mode: None,
        };
        let bullets = supporting_bullets(
            &profile,
            &job,
            &["React".into(), "Java".into(), "design".into()],
        );
        assert_eq!(
            bullets,
            vec!["Built React and Java services for checkout.".to_string()]
        );
        let summary = application_summary(&job, &bullets);
        assert!(summary.contains("Built React and Java services"));
        assert!(!summary.contains("Nothing beyond"));
        assert!(!summary.to_lowercase().contains("matlab"));
    }

    #[test]
    fn splits_a_full_name() {
        assert_eq!(
            split_name("Ada Lovelace"),
            ("Ada".into(), "Lovelace".into())
        );
        assert_eq!(split_name("Ada"), ("Ada".into(), "Candidate".into()));
    }
}
