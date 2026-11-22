//! Per-job interview prep sheets.
//!
//! The deterministic base turns the job's requirements into direct technical
//! questions and pairs behavioral questions with the candidate's own resume
//! bullets as citable evidence. When a tailor LLM is configured the questions
//! are rewritten for the role; quoted evidence is validated back against the
//! resume exactly like the tailored-resume merge.

use crate::models::Job;
use crate::resume::CandidateProfile as ResumeProfile;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepQuestion {
    pub question: String,
    pub talking_points: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepSheet {
    pub overview: String,
    pub technical_questions: Vec<PrepQuestion>,
    pub behavioral_questions: Vec<PrepQuestion>,
    pub questions_to_ask: Vec<String>,
    /// Which LLM rewrote the sheet, if any.
    #[serde(default)]
    pub generated_by: Option<String>,
}

const MAX_TECH: usize = 10;
const MAX_BEHAVIORAL: usize = 5;

/// Top resume bullets by matched-skill hits — the evidence pool every
/// behavioral answer should draw from.
fn top_bullets(resume: &ResumeProfile, matched: &[String], take: usize) -> Vec<String> {
    let mut scored: Vec<(usize, usize, &str)> = resume
        .experience
        .iter()
        .flat_map(|role| role.bullets.iter())
        .enumerate()
        .map(|(index, bullet)| {
            let hits = matched.iter().filter(|m| has_term(&bullet.text, m)).count();
            (hits, index, bullet.text.as_str())
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(take)
        .map(|(_, _, text)| text.to_string())
        .collect()
}

fn bullets_for_skill(resume: &ResumeProfile, skill: &str, take: usize) -> Vec<String> {
    let mut out = Vec::new();
    for role in &resume.experience {
        for bullet in &role.bullets {
            if has_term(&bullet.text, skill) {
                out.push(bullet.text.clone());
            }
            if out.len() >= take {
                return out;
            }
        }
    }
    out
}

pub fn build_deterministic(resume: &ResumeProfile, job: &Job, matched: &[String]) -> PrepSheet {
    let location = job
        .location
        .clone()
        .unwrap_or_else(|| "unspecified location".into());
    let overview =
        format!(
        "Interview prep for the {} role at {} ({}). {} of your resume skills match this posting{}.",
        job.title.trim(),
        job.company.trim(),
        location,
        matched.len(),
        if matched.is_empty() {
            String::new()
        } else {
            format!(": {}", matched.iter().take(6).cloned().collect::<Vec<_>>().join(", "))
        }
    );
    let mut technical_questions = Vec::new();
    for skill in matched.iter().take(5) {
        let evidence = bullets_for_skill(resume, skill, 2);
        technical_questions.push(PrepQuestion {
            question: format!("Walk me through your experience with {skill}."),
            talking_points: evidence.clone(),
        });
        if technical_questions.len() >= MAX_TECH {
            break;
        }
        technical_questions.push(PrepQuestion {
            question: format!("What is the hardest problem you have solved with {skill}, and how?"),
            talking_points: evidence,
        });
        if technical_questions.len() >= MAX_TECH {
            break;
        }
    }
    let evidence = top_bullets(resume, matched, 3);
    let behavioral_questions = [
        "Tell me about yourself.",
        "Describe a time you dealt with a difficult stakeholder or teammate.",
        "Tell me about a mistake you made and what you changed afterwards.",
        "Describe a time you delivered under a tight deadline.",
        "Why do you want this role at this company?",
    ]
    .into_iter()
    .take(MAX_BEHAVIORAL)
    .map(|question| PrepQuestion {
        question: question.to_string(),
        talking_points: evidence.clone(),
    })
    .collect();
    PrepSheet {
        overview,
        technical_questions,
        behavioral_questions,
        questions_to_ask: vec![
            "How is the team structured, and what would my first 90 days look like?".into(),
            "What does success in this role look like after six months?".into(),
            "What is the hardest technical challenge the team is facing right now?".into(),
            "How does the team handle code review, testing, and on-call?".into(),
            "What are the growth and mentorship opportunities on this team?".into(),
        ],
        generated_by: None,
    }
}

/// Prompt for the LLM rewrite. Practice material only — but quoted evidence
/// must still come from the resume, enforced by `merge_llm_prep`.
pub fn prep_prompts(resume: &ResumeProfile, job: &Job, base: &PrepSheet) -> (String, String) {
    let system = "You write interview prep for ONE job posting. Hard rules:\n\
        1. Technical questions must target the posting's requirements and the candidate's matched skills.\n\
        2. Talking points must quote or closely paraphrase ONLY the supplied resume bullets. Never invent projects, metrics, or employers.\n\
        3. Behavioral questions are standard; map each to the most relevant resume bullets.\n\
        4. Return valid JSON only, no markdown fences, exactly this shape:\n\
        {\"overview\": \"...\", \
        \"technical_questions\": [{\"question\": \"...\", \"talking_points\": [\"<resume bullet>\"]}], \
        \"behavioral_questions\": [{\"question\": \"...\", \"talking_points\": [\"<resume bullet>\"]}], \
        \"questions_to_ask\": [\"...\"]}"
        .to_string();
    let bullets: Vec<&str> = resume
        .experience
        .iter()
        .flat_map(|role| role.bullets.iter().map(|b| b.text.as_str()))
        .collect();
    let user = format!(
        "Resume bullets (only citable evidence):\n{}\n\nSkills: {}\n\n\
         Job posting:\nTitle: {title}\nCompany: {company}\nDescription:\n{desc}\nRequired skills: {skills}\n\n\
         Draft overview: {overview}\n\n\
         Rewrite the prep for this exact role: up to 10 technical questions with evidence, \
         up to 5 behavioral questions with evidence, and up to 6 questions to ask them.",
        bullets.join("\n- "),
        resume.skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "),
        title = job.title,
        company = job.company,
        desc = clip(&job.description, 8_000),
        skills = job.skills.join(", "),
        overview = base.overview,
    );
    (system, user)
}

/// Merges an LLM rewrite into the deterministic base. Questions are advice, so
/// they pass through with length caps; every talking point must trace back to
/// a resume bullet or it is replaced with the base evidence pool.
pub fn merge_llm_prep(base: &mut PrepSheet, resume: &ResumeProfile, value: &serde_json::Value) {
    if let Some(overview) = value.get("overview").and_then(|v| v.as_str()) {
        let overview = overview.trim();
        if !overview.is_empty() && overview.len() <= 1_000 {
            base.overview = overview.to_string();
        }
    }
    let pool: Vec<String> = top_bullets_from_base(base);
    if let Some(items) = value.get("technical_questions").and_then(|v| v.as_array()) {
        let merged = merge_questions(items, resume, &pool, MAX_TECH);
        if !merged.is_empty() {
            base.technical_questions = merged;
        }
    }
    if let Some(items) = value.get("behavioral_questions").and_then(|v| v.as_array()) {
        let merged = merge_questions(items, resume, &pool, MAX_BEHAVIORAL);
        if !merged.is_empty() {
            base.behavioral_questions = merged;
        }
    }
    if let Some(items) = value.get("questions_to_ask").and_then(|v| v.as_array()) {
        let kept: Vec<String> = items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 200)
            .take(8)
            .map(str::to_string)
            .collect();
        if !kept.is_empty() {
            base.questions_to_ask = kept;
        }
    }
}

fn top_bullets_from_base(base: &PrepSheet) -> Vec<String> {
    let mut pool = Vec::new();
    for question in base
        .behavioral_questions
        .iter()
        .chain(base.technical_questions.iter())
    {
        for point in &question.talking_points {
            if !pool.contains(point) {
                pool.push(point.clone());
            }
            if pool.len() >= 3 {
                return pool;
            }
        }
    }
    pool
}

fn merge_questions(
    items: &[serde_json::Value],
    resume: &ResumeProfile,
    fallback: &[String],
    cap: usize,
) -> Vec<PrepQuestion> {
    let bullets: Vec<String> = resume
        .experience
        .iter()
        .flat_map(|role| role.bullets.iter().map(|b| b.text.clone()))
        .collect();
    let mut out = Vec::new();
    for item in items {
        let Some(question) = item
            .get("question")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 300)
        else {
            continue;
        };
        let cited: Vec<String> = item
            .get("talking_points")
            .and_then(|v| v.as_array())
            .map(|points| {
                points
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter(|point| bullets.iter().any(|b| bullet_match(b, point)))
                    .take(3)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        out.push(PrepQuestion {
            question: question.to_string(),
            talking_points: if cited.is_empty() {
                fallback.to_vec()
            } else {
                cited
            },
        });
        if out.len() >= cap {
            break;
        }
    }
    out
}

/// A cited point counts when it equals a resume bullet or clearly contains /
/// is contained by one (quotes and close paraphrases pass, inventions fail).
fn bullet_match(bullet: &str, cited: &str) -> bool {
    let a = normalize(bullet);
    let b = normalize(cited);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.contains(&b) || b.contains(&a)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

fn has_term(text: &str, term: &str) -> bool {
    let term = normalize(term);
    if term.len() < 2 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resume::{EvidenceBullet, Experience, SkillEvidence};

    fn resume() -> ResumeProfile {
        ResumeProfile {
            name: Some("Test".into()),
            headline: Some("Senior Software Engineer".into()),
            summary: None,
            skills: vec![
                SkillEvidence {
                    name: "Rust".into(),
                    evidence: vec![],
                    confidence: 0.9,
                },
                SkillEvidence {
                    name: "Go".into(),
                    evidence: vec![],
                    confidence: 0.9,
                },
            ],
            experience: vec![Experience {
                company: "Acme".into(),
                title: "Senior Software Engineer".into(),
                dates: None,
                location: None,
                bullets: vec![
                    EvidenceBullet {
                        text: "Built Rust services for checkout.".into(),
                        source_excerpt: String::new(),
                    },
                    EvidenceBullet {
                        text: "Maintained Go APIs for billing.".into(),
                        source_excerpt: String::new(),
                    },
                ],
            }],
            projects: vec![],
            education: vec![],
            open_source: vec![], layout: vec![], custom_fields: vec![],
            source_file: None,
            extraction_warnings: vec![],
        }
    }

    fn job() -> Job {
        Job {
            external_id: "1".into(),
            source: "test".into(),
            title: "Backend Engineer, Rust".into(),
            company: "Target".into(),
            location: None,
            description: "Build Rust services.".into(),
            skills: vec!["Rust".into()],
            min_experience: None,
            max_experience: None,
            url: "https://example.test".into(),
            discovered_at: chrono::Utc::now(),
            published_at: None,
            work_mode: None,
        }
    }

    #[test]
    fn deterministic_prep_names_matched_skills() {
        let sheet = build_deterministic(&resume(), &job(), &["Rust".to_string()]);
        assert!(sheet.overview.contains("Rust"));
        assert!(sheet
            .technical_questions
            .iter()
            .any(|q| q.question.contains("Rust")));
        assert_eq!(sheet.behavioral_questions.len(), 5);
        let rust_points: Vec<&str> = sheet.technical_questions[0]
            .talking_points
            .iter()
            .map(String::as_str)
            .collect();
        assert!(rust_points.contains(&"Built Rust services for checkout."));
    }

    #[test]
    fn merge_keeps_quotes_and_drops_inventions() {
        let r = resume();
        let mut sheet = build_deterministic(&r, &job(), &["Rust".to_string()]);
        let rewrite = serde_json::json!({
            "overview": "Prep for the Rust role at Target.",
            "technical_questions": [
                {"question": "Explain ownership in Rust.", "talking_points": ["Built Rust services for checkout.", "Scaled to 10M users overnight."]},
            ],
            "behavioral_questions": [],
            "questions_to_ask": ["What does success look like?"]
        });
        merge_llm_prep(&mut sheet, &r, &rewrite);
        assert_eq!(sheet.overview, "Prep for the Rust role at Target.");
        assert_eq!(sheet.technical_questions.len(), 1);
        assert_eq!(
            sheet.technical_questions[0].talking_points,
            vec!["Built Rust services for checkout."]
        );
        assert_eq!(sheet.questions_to_ask, vec!["What does success look like?"]);
        // Empty behavioral list from the model must not wipe the base.
        assert_eq!(sheet.behavioral_questions.len(), 5);
    }
}
