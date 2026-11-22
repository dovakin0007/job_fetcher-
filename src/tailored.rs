use crate::models::Job;
use crate::resume::CandidateProfile as ResumeProfile;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailoredBullet {
    pub text: String,
    pub hits: usize,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailoredRole {
    pub company: String,
    pub title: String,
    pub dates: Option<String>,
    pub location: Option<String>,
    pub bullets: Vec<TailoredBullet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailoredProject {
    pub name: String,
    pub description: String,
    pub technologies: Vec<String>,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailoredResume {
    pub headline: String,
    pub summary: String,
    pub skills: Vec<String>,
    pub roles: Vec<TailoredRole>,
    pub projects: Vec<TailoredProject>,
    pub markdown: String,
    pub matched_skills: Vec<String>,
    pub missing_requirements: Vec<String>,
    pub omitted_bullets: usize,
    pub omitted_skills: usize,
    pub integrity_note: String,
    /// True when an LLM provider rewrote the headline/summary/picks.
    #[serde(default)]
    pub llm_used: bool,
    /// Which provider produced the rewrite ("ollama", "openai", "anthropic", "gemini").
    #[serde(default)]
    pub llm_provider: Option<String>,
}

const MAX_BULLETS_PER_ROLE: usize = 4;
const MAX_TOTAL_BULLETS: usize = 10;
const MAX_PROJECTS: usize = 3;
const MAX_SKILLS: usize = 32;

/// Builds a per-job CV from the SAME base resume: nothing is invented.
/// Skills are reordered so job-matched ones come first; bullets/projects are
/// ranked by how many matched skills they evidence; the summary/headline are
/// rephrased from resume facts only.
pub fn build_tailored_resume(
    resume: &ResumeProfile,
    prefs: Option<&crate::models::CandidateProfile>,
    job: &Job,
) -> TailoredResume {
    let job_text = format!("{} {} {}", job.title, job.description, job.skills.join(" "));

    // Matched = resume skills named in the job text, ranked by confidence then evidence.
    let mut scored: Vec<(f32, usize, String)> = resume
        .skills
        .iter()
        .filter(|s| has_term(&job_text, &s.name))
        .map(|s| (s.confidence, s.evidence.len(), s.name.clone()))
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.cmp(&a.1))
    });
    let matched: Vec<String> = scored.into_iter().map(|(_, _, n)| n).collect();

    let missing: Vec<String> = job
        .skills
        .iter()
        .filter(|r| !resume.skills.iter().any(|s| s.name.eq_ignore_ascii_case(r)))
        .cloned()
        .collect();

    // Skills: matched first (in rank order), then the rest in resume order.
    let mut skills: Vec<String> = matched.clone();
    for s in &resume.skills {
        if !skills.iter().any(|m| m.eq_ignore_ascii_case(&s.name)) {
            skills.push(s.name.clone());
        }
    }
    let omitted_skills = skills.len().saturating_sub(MAX_SKILLS);
    skills.truncate(MAX_SKILLS);

    // Bullets: per role, rank by matched-skill hits (stable for ties).
    let mut roles: Vec<TailoredRole> = Vec::new();
    let mut global: Vec<(usize, usize, usize)> = Vec::new(); // (hits, role_idx, bullet_idx)
    for (ri, role) in resume.experience.iter().enumerate() {
        let mut bullets: Vec<(usize, usize, String)> = role
            .bullets
            .iter()
            .enumerate()
            .map(|(bi, b)| {
                let hits = matched.iter().filter(|m| has_term(&b.text, m)).count();
                (hits, bi, b.text.clone())
            })
            .collect();
        bullets.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        for (hits, _, _) in &bullets {
            global.push((*hits, ri, 0));
        }
        roles.push(TailoredRole {
            company: role.company.clone(),
            title: role.title.clone(),
            dates: role.dates.clone(),
            location: role.location.clone(),
            bullets: bullets
                .into_iter()
                .map(|(hits, _, text)| TailoredBullet {
                    text,
                    hits,
                    selected: false,
                })
                .collect(),
        });
    }
    // Select: top scoring bullets overall, capped per role and globally.
    let mut order: Vec<(usize, usize, usize)> = Vec::new(); // (hits, ri, pos_in_role)
    for (ri, role) in roles.iter().enumerate() {
        for (pi, b) in role.bullets.iter().enumerate() {
            order.push((b.hits, ri, pi));
        }
    }
    order.sort_by(|a, b| b.0.cmp(&a.0));
    let mut taken_per_role = vec![0usize; roles.len()];
    let mut taken_total = 0usize;
    for (hits, ri, pi) in order {
        if hits == 0 {
            break;
        }
        if taken_total >= MAX_TOTAL_BULLETS || taken_per_role[ri] >= MAX_BULLETS_PER_ROLE {
            continue;
        }
        roles[ri].bullets[pi].selected = true;
        taken_per_role[ri] += 1;
        taken_total += 1;
    }
    // Keep(role) visible even with no hits: select the first bullet so the
    // role history isn't dropped, but only if we still have budget.
    for (ri, role) in roles.iter_mut().enumerate() {
        if !role.bullets.is_empty()
            && taken_per_role[ri] == 0
            && taken_total < MAX_TOTAL_BULLETS
            && !role.bullets.is_empty()
        {
            role.bullets[0].selected = true;
            taken_total += 1;
        }
        let _ = ri;
    }
    let omitted_bullets = roles
        .iter()
        .flat_map(|r| r.bullets.iter())
        .filter(|b| !b.selected)
        .count();

    // Projects ranked by matched-skill/technology hits.
    let mut ranked: Vec<(usize, usize, &crate::resume::Project)> = resume
        .projects
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let hay = format!("{} {} {}", p.name, p.description, p.technologies.join(" "));
            let hits = matched.iter().filter(|m| has_term(&hay, m)).count();
            (hits, i, p)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let projects: Vec<TailoredProject> = ranked
        .into_iter()
        .take(MAX_PROJECTS)
        .map(|(_, _, p)| TailoredProject {
            name: p.name.clone(),
            description: p.description.clone(),
            technologies: p.technologies.clone(),
            selected: true,
        })
        .collect();

    let headline = build_headline(resume, job, &matched);
    let summary = build_summary(resume, prefs, job, &matched);
    let markdown = render_markdown(
        resume, prefs, job, &headline, &summary, &skills, &roles, &projects,
    );

    TailoredResume {
        headline,
        summary,
        skills,
        roles,
        projects,
        markdown,
        matched_skills: matched,
        missing_requirements: missing,
        omitted_bullets,
        omitted_skills,
        integrity_note: "Generated only from lines in your uploaded resume. Review before sending; nothing was invented.".into(),
        llm_used: false,
        llm_provider: None,
    }
}

fn build_headline(resume: &ResumeProfile, job: &Job, matched: &[String]) -> String {
    let top: Vec<&str> = matched.iter().take(3).map(String::as_str).collect();
    let role = if job.title.trim().is_empty() {
        resume.headline.clone().unwrap_or_default()
    } else {
        job.title.trim().to_string()
    };
    if top.is_empty() {
        if role.is_empty() {
            resume.headline.clone().unwrap_or_default()
        } else {
            role
        }
    } else if role.is_empty() {
        top.join(" · ")
    } else {
        format!("{} · {}", role, top.join(" · "))
    }
}

fn build_summary(
    resume: &ResumeProfile,
    prefs: Option<&crate::models::CandidateProfile>,
    job: &Job,
    matched: &[String],
) -> String {
    let current = resume.experience.first();
    let who = match current {
        Some(r) => format!("{} at {}", r.title, r.company),
        None => resume
            .headline
            .clone()
            .unwrap_or_else(|| "Software engineer".into()),
    };
    let years = prefs
        .map(|p| p.experience_years)
        .filter(|y| *y > 0.0)
        .unwrap_or_else(|| crate::matching::estimated_years(resume));
    let years_part = if years > 0.0 {
        format!(
            " with {years:.1} years of experience",
            years = (years * 2.0).round() / 2.0
        )
    } else {
        String::new()
    };
    let skills_part = if matched.is_empty() {
        String::new()
    } else {
        format!(
            " Skilled in {}.",
            matched
                .iter()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let target = if job.title.trim().is_empty() || job.company.trim().is_empty() {
        String::new()
    } else {
        format!(
            " Applying for the {} role at {}.",
            job.title.trim(),
            job.company.trim()
        )
    };
    format!("{who}{years_part}.{skills_part}{target}")
        .replace("..", ".")
        .trim()
        .to_string()
}

fn render_markdown(
    resume: &ResumeProfile,
    prefs: Option<&crate::models::CandidateProfile>,
    job: &Job,
    headline: &str,
    summary: &str,
    skills: &[String],
    roles: &[TailoredRole],
    projects: &[TailoredProject],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# {}\n\n",
        resume.name.clone().unwrap_or_else(|| "Candidate".into())
    ));
    if !headline.trim().is_empty() {
        out.push_str(&format!("{headline}\n\n"));
    }
    if let Some(p) = prefs {
        let contact = format!("{} {}", p.email.trim(), p.phone.trim())
            .trim()
            .to_string();
        if !contact.is_empty() {
            out.push_str(&format!("{contact}\n\n"));
        }
    }
    out.push_str(&format!(
        "Target role: {} at {}\n\n",
        job.title.trim(),
        job.company.trim()
    ));
    out.push_str("## Summary\n\n");
    out.push_str(&format!("{summary}\n\n"));
    out.push_str("## Skills\n\n");
    out.push_str(&format!("{}\n\n", skills.join(", ")));
    out.push_str("## Experience\n\n");
    for role in roles {
        let dates = role.dates.clone().unwrap_or_default();
        let loc = role.location.clone().unwrap_or_default();
        let meta = format!(
            "{dates}{}",
            if loc.is_empty() {
                String::new()
            } else {
                format!(" · {loc}")
            }
        );
        out.push_str(&format!("### {} — {}\n", role.title, role.company));
        if !meta.trim().is_empty() && meta.trim() != "·" {
            out.push_str(&format!("{meta}\n"));
        }
        let chosen: Vec<&TailoredBullet> = role.bullets.iter().filter(|b| b.selected).collect();
        if chosen.is_empty() {
            out.push_str("- (No directly matching bullets; see full resume.)\n");
        } else {
            for b in chosen {
                out.push_str(&format!("- {}\n", b.text.trim()));
            }
        }
        out.push('\n');
    }
    if !projects.is_empty() {
        out.push_str("## Projects\n\n");
        for p in projects {
            out.push_str(&format!("### {}\n{}\n", p.name, p.description));
            if !p.technologies.is_empty() {
                out.push_str(&format!("Technologies: {}\n", p.technologies.join(", ")));
            }
            out.push('\n');
        }
    }
    if !resume.education.is_empty() {
        out.push_str("## Education\n\n");
        for e in &resume.education {
            out.push_str(&format!(
                "- {}, {}{}\n",
                e.qualification,
                e.institution,
                e.dates
                    .clone()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            ));
        }
        out.push('\n');
    }
    out
}

/// Prompt for the LLM rewrite. The parsed resume (LLM-extracted, evidence-backed)
/// is the only source of truth; the deterministic picks show what already
/// matched so the model rephrases and prioritises instead of inventing.
pub fn tailor_prompts(
    resume: &ResumeProfile,
    prefs: Option<&crate::models::CandidateProfile>,
    job: &Job,
    base: &TailoredResume,
) -> (String, String) {
    let system = "You rewrite a resume for ONE job posting. Hard rules:\n\
        1. Use ONLY facts present in the supplied parsed resume. Never invent skills, dates, employers, metrics, or achievements.\n\
        2. Every bullet you pick MUST be copied word-for-word from the resume bullets list.\n\
        3. Every skill you list MUST come from the resume skills list.\n\
        4. The summary may rephrase resume facts but must not add new claims.\n\
        5. Return valid JSON only, no markdown fences, exactly this shape:\n\
        {\"headline\": \"...\", \"summary\": \"...\", \
        \"bullet_picks\": [\"<exact resume bullet>\", ...], \
        \"skills_order\": [\"<resume skill>\", ...]}"
        .to_string();
    let resume_json = serde_json::json!({
        "name": resume.name,
        "headline": resume.headline,
        "summary": resume.summary,
        "skills": resume.skills.iter().map(|s| &s.name).collect::<Vec<_>>(),
        "experience": resume.experience.iter().map(|e| serde_json::json!({
            "company": e.company, "title": e.title, "dates": e.dates,
            "location": e.location,
            "bullets": e.bullets.iter().map(|b| &b.text).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "projects": resume.projects.iter().map(|p| serde_json::json!({
            "name": p.name, "description": p.description, "technologies": p.technologies,
        })).collect::<Vec<_>>(),
        "education": resume.education,
    });
    let user = format!(
        "Parsed resume (only source of truth):\n{resume}\n\n\
         Job posting:\nTitle: {title}\nCompany: {company}\nLocation: {location}\nDescription:\n{desc}\nRequired skills: {skills}\n\n\
         Deterministic shortlist (already matched, prefer these):\nMatched skills: {matched}\n\
         Suggested headline: {headline}\nSuggested summary: {summary}\n\n\
         {years}Rewrite the headline and summary for this job, pick up to 10 bullets \
         (exact copies, most job-relevant first), and order all resume skills \
         most job-relevant first.",
        resume = clip_json(&resume_json.to_string(), 12_000),
        title = job.title,
        company = job.company,
        location = job.location.as_deref().unwrap_or("unspecified"),
        desc = clip_chars(&job.description, 8_000),
        skills = job.skills.join(", "),
        matched = base.matched_skills.join(", "),
        headline = base.headline,
        summary = base.summary,
        years = prefs
            .map(|p| p.experience_years)
            .filter(|y| *y > 0.0)
            .map(|y| format!("Candidate has {y:.1} years of experience. "))
            .unwrap_or_default(),
    );
    (system, user)
}

/// Merges an LLM rewrite into the deterministic base. Anything that cannot be
/// traced back to the parsed resume is dropped: unknown bullets/skills are
/// ignored and unsupported summary sentences fall back to the base text.
pub fn merge_llm_rewrite(
    base: &mut TailoredResume,
    resume: &ResumeProfile,
    prefs: Option<&crate::models::CandidateProfile>,
    job: &Job,
    value: &serde_json::Value,
) {
    if let Some(headline) = value.get("headline").and_then(|v| v.as_str()) {
        let headline = headline.trim();
        if !headline.is_empty() && headline.len() <= 300 {
            base.headline = headline.to_string();
        }
    }
    if let Some(summary) = value.get("summary").and_then(|v| v.as_str()) {
        let kept = supported_sentences(summary, resume, job);
        if !kept.trim().is_empty() {
            base.summary = kept;
        }
    }
    // Bullet picks must exactly match resume bullets (normalised).
    let mut index: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    for (ri, role) in base.roles.iter().enumerate() {
        for (pi, bullet) in role.bullets.iter().enumerate() {
            index.insert(normalize(&bullet.text), (ri, pi));
        }
    }
    if let Some(picks) = value.get("bullet_picks").and_then(|v| v.as_array()) {
        let mut chosen: Vec<(usize, usize)> = Vec::new();
        for pick in picks.iter().filter_map(|v| v.as_str()) {
            if let Some(&pos) = index.get(&normalize(pick)) {
                if !chosen.contains(&pos) {
                    chosen.push(pos);
                }
            }
            if chosen.len() >= MAX_TOTAL_BULLETS {
                break;
            }
        }
        if !chosen.is_empty() {
            for role in base.roles.iter_mut() {
                for bullet in role.bullets.iter_mut() {
                    bullet.selected = false;
                }
            }
            let mut per_role = vec![0usize; base.roles.len()];
            for (ri, pi) in chosen {
                if per_role[ri] < MAX_BULLETS_PER_ROLE {
                    base.roles[ri].bullets[pi].selected = true;
                    per_role[ri] += 1;
                }
            }
        }
    }
    // Skill order must be a subset of resume skills.
    if let Some(order) = value.get("skills_order").and_then(|v| v.as_array()) {
        let allowed: std::collections::HashSet<String> = resume
            .skills
            .iter()
            .map(|s| s.name.to_lowercase())
            .collect();
        let mut ranked: Vec<String> = Vec::new();
        for name in order.iter().filter_map(|v| v.as_str()) {
            if allowed.contains(&name.to_lowercase())
                && !ranked.iter().any(|m: &String| m.eq_ignore_ascii_case(name))
            {
                ranked.push(name.to_string());
            }
        }
        if !ranked.is_empty() {
            for s in resume.skills.iter().map(|s| &s.name) {
                if !ranked.iter().any(|m| m.eq_ignore_ascii_case(s)) {
                    ranked.push(s.clone());
                }
            }
            base.omitted_skills = ranked.len().saturating_sub(MAX_SKILLS);
            ranked.truncate(MAX_SKILLS);
            base.skills = ranked;
        }
    }
    base.omitted_bullets = base
        .roles
        .iter()
        .flat_map(|r| r.bullets.iter())
        .filter(|b| !b.selected)
        .count();
    base.markdown = render_markdown(
        resume,
        prefs,
        job,
        &base.headline.clone(),
        &base.summary.clone(),
        &base.skills.clone(),
        &base.roles.clone(),
        &base.projects.clone(),
    );
}

fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

fn clip_json(text: &str, max: usize) -> String {
    clip_chars(text, max)
}

/// Keeps only summary sentences grounded in the resume (or the job header).
/// A sentence survives when at least 60% of its content words appear in the
/// resume vocabulary, the job title/company, or it carries no content words.
fn supported_sentences(summary: &str, resume: &ResumeProfile, job: &Job) -> String {
    let mut resume_text = String::new();
    for part in resume
        .summary
        .iter()
        .chain(resume.headline.iter())
        .map(|s| s.as_str())
    {
        resume_text.push_str(part);
        resume_text.push(' ');
    }
    for s in resume.skills.iter().map(|s| s.name.as_str()) {
        resume_text.push_str(s);
        resume_text.push(' ');
    }
    for role in &resume.experience {
        resume_text.push_str(&role.title);
        resume_text.push(' ');
        resume_text.push_str(&role.company);
        for b in &role.bullets {
            resume_text.push_str(&b.text);
            resume_text.push(' ');
        }
    }
    let vocab: std::collections::HashSet<String> = normalize(&resume_text)
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let job_vocab: std::collections::HashSet<String> =
        normalize(&format!("{} {}", job.title, job.company))
            .split_whitespace()
            .map(str::to_string)
            .collect();
    let mut kept = Vec::new();
    for sentence in summary.split_inclusive(&['.', '!', '?'][..]) {
        let sentence = sentence.trim();
        if sentence.is_empty() {
            continue;
        }
        let words: Vec<String> = normalize(sentence)
            .split_whitespace()
            .filter(|w| !STOPWORDS.contains(w))
            .map(str::to_string)
            .collect();
        if words.is_empty() {
            kept.push(sentence.to_string());
            continue;
        }
        let supported = words
            .iter()
            .filter(|w| vocab.contains(w.as_str()) || job_vocab.contains(w.as_str()))
            .count();
        if supported >= 2 && supported as f32 / words.len() as f32 >= 0.6 {
            kept.push(sentence.to_string());
        }
    }
    kept.join(" ").trim().to_string()
}

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "in", "on", "at", "to", "for", "with", "is", "are", "was",
    "were", "be", "been", "i", "my", "as", "by", "from", "that", "this", "it", "its", "into",
    "over", "under", "both", "each", "years", "year",
];

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
            summary: Some("Backend engineer.".into()),
            skills: vec![skill("Rust", 0.95), skill("Go", 0.9), skill("React", 0.7)],
            experience: vec![Experience {
                company: "Acme".into(),
                title: "Senior Software Engineer".into(),
                dates: Some("2022 – Present".into()),
                location: Some("Bengaluru".into()),
                bullets: vec![
                    bullet("Built Rust services for checkout."),
                    bullet("Built React dashboards for analytics."),
                    bullet("Maintained Go APIs for billing."),
                ],
            }],
            projects: vec![],
            education: vec![],
            open_source: vec![], layout: vec![], custom_fields: vec![],
            source_file: None,
            extraction_warnings: vec![],
        }
    }

    fn skill(name: &str, conf: f32) -> SkillEvidence {
        SkillEvidence {
            name: name.into(),
            evidence: vec![format!("used {name}")],
            confidence: conf,
        }
    }

    fn bullet(text: &str) -> EvidenceBullet {
        EvidenceBullet {
            text: text.into(),
            source_excerpt: text.into(),
        }
    }

    fn job(title: &str, desc: &str, skills: &[&str]) -> Job {
        Job {
            external_id: "1".into(),
            source: "test".into(),
            title: title.into(),
            company: "Target".into(),
            location: None,
            description: desc.into(),
            skills: skills.iter().map(|s| (*s).to_string()).collect(),
            min_experience: None,
            max_experience: None,
            url: "https://example.test".into(),
            discovered_at: chrono::Utc::now(),
            published_at: None,
            work_mode: None,
        }
    }

    #[test]
    fn same_resume_changes_order_per_job() {
        let rust_job = job("Backend Engineer, Rust", "Build Rust services.", &["Rust"]);
        let react_job = job("Frontend Engineer, React", "Build React apps.", &["React"]);
        let a = build_tailored_resume(&resume(), None, &rust_job);
        let b = build_tailored_resume(&resume(), None, &react_job);
        assert_eq!(a.skills[0], "Rust");
        assert_eq!(b.skills[0], "React");
        assert!(a.summary.contains("Rust"));
        assert!(b.summary.contains("React"));
        assert_ne!(a.markdown, b.markdown);
    }

    #[test]
    fn output_invents_nothing() {
        let r = resume();
        let j = job(
            "Backend Engineer, Rust",
            "Build Rust services.",
            &["Rust", "Kubernetes"],
        );
        let t = build_tailored_resume(&r, None, &j);
        let allowed: std::collections::HashSet<String> =
            r.skills.iter().map(|s| s.name.to_lowercase()).collect();
        for s in &t.skills {
            assert!(
                allowed.contains(&s.to_lowercase()),
                "skill not in resume: {s}"
            );
        }
        let bullets: std::collections::HashSet<String> = r
            .experience
            .iter()
            .flat_map(|e| e.bullets.iter().map(|b| b.text.clone()))
            .collect();
        for role in &t.roles {
            for b in &role.bullets {
                assert!(bullets.contains(&b.text), "bullet not in resume");
            }
        }
        assert!(t.missing_requirements.contains(&"Kubernetes".to_string()));
    }

    #[test]
    fn merge_drops_invented_bullets_and_skills() {
        let r = resume();
        let j = job("Backend Engineer, Rust", "Build Rust services.", &["Rust"]);
        let mut t = build_tailored_resume(&r, None, &j);
        let rewrite = serde_json::json!({
            "headline": "Backend Engineer, Rust · Rust",
            "summary": "Senior Software Engineer at Acme with Rust experience.",
            "bullet_picks": [
                "Built Rust services for checkout.",
                "Led a team of 50 engineers to IPO." // invented: must be dropped
            ],
            "skills_order": ["Rust", "Kubernetes", "Go"] // Kubernetes must be dropped
        });
        merge_llm_rewrite(&mut t, &r, None, &j, &rewrite);
        assert!(!t.llm_used); // flag is set by the endpoint, not the merge
        let selected: Vec<&str> = t
            .roles
            .iter()
            .flat_map(|role| role.bullets.iter())
            .filter(|b| b.selected)
            .map(|b| b.text.as_str())
            .collect();
        assert_eq!(selected, vec!["Built Rust services for checkout."]);
        assert!(!t.skills.iter().any(|s| s == "Kubernetes"));
        assert_eq!(t.skills[0], "Rust");
    }

    #[test]
    fn merge_drops_unsupported_summary_sentences() {
        let r = resume();
        let j = job("Backend Engineer, Rust", "Build Rust services.", &["Rust"]);
        let mut t = build_tailored_resume(&r, None, &j);
        let rewrite = serde_json::json!({
            "headline": "",
            "summary": "Senior Software Engineer at Acme with Rust experience. Won the Nobel Prize in 2024.",
            "bullet_picks": [],
            "skills_order": []
        });
        merge_llm_rewrite(&mut t, &r, None, &j, &rewrite);
        assert!(!t.summary.contains("Nobel"));
        assert!(t.summary.contains("Rust"));
    }
}
