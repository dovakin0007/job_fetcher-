//! Community knowledge sources for study guides.
//!
//! Two source classes, chosen for license safety:
//!
//! 1. **Ingested Q&A** — `bregman-arie/devops-exercises` (CC BY-NC: quoted
//!    verbatim with attribution + link, personal non-commercial use only).
//!    Topic READMEs use `<details><summary>Q</summary><b>A</b></details>`
//!    blocks, fetched from raw.githubusercontent.com and cached in SQLite.
//! 2. **Linked reading** — `donnemartin/system-design-primer` (no declared
//!    license, so content is never copied). Only deep links to solution pages
//!    whose directories were verified to exist.
//!
//! Set `KNOWLEDGE_OFFLINE=1` to skip all network fetching.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityQA {
    pub question: String,
    pub answer: String,
    pub topic: String,
    pub source_repo: String,
    pub source_license: String,
    pub source_url: String,
    /// True when the item was generated for you (LLM or deterministic),
    /// not quoted from a community repo.
    #[serde(default)]
    pub generated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimerLink {
    pub title: String,
    pub url: String,
}

const DEVOPS_REPO: &str = "bregman-arie/devops-exercises";
const DEVOPS_LICENSE: &str = "CC BY-NC 3.0 — personal non-commercial use, attribution required";
const DEVOPS_BRANCH: &str = "master";

fn raw_url(topic: &str) -> String {
    format!(
        "https://raw.githubusercontent.com/{DEVOPS_REPO}/{DEVOPS_BRANCH}/topics/{topic}/README.md"
    )
}

fn page_url(topic: &str) -> String {
    format!("https://github.com/{DEVOPS_REPO}/blob/{DEVOPS_BRANCH}/topics/{topic}/README.md")
}

/// Topic files to fetch for the given skills, in priority order (cap 4).
pub fn topics_for_skills(skills: &[String]) -> Vec<String> {
    let mut topics = Vec::new();
    let mut push = |topic: &str| {
        if topics.len() < 4 && !topics.iter().any(|t: &String| t == topic) {
            topics.push(topic.to_string());
        }
    };
    for skill in skills {
        let s = skill.to_ascii_lowercase();
        let s = s.as_str();
        if s.contains("kubernetes") || s == "k8s" {
            push("kubernetes");
        } else if s.contains("docker") || s.contains("container") {
            push("containers");
        } else if s.contains("python") {
            push("python");
        } else if s.contains("sql") || s.contains("postgres") || s.contains("mysql") {
            push("sql");
            push("databases");
        } else if s == "git" {
            push("git");
        } else if s.contains("linux") || s.contains("bash") || s == "shell" {
            push("linux");
        } else if s.contains("kafka") || s.contains("rabbitmq") || s.contains("messaging") {
            push("kafka");
        } else if s.contains("terraform") || s.contains("iac") {
            push("terraform");
        } else if s.contains("aws") {
            push("aws");
        } else if (s.contains("gcp") || s.contains("google cloud")) && !s.contains("bigquery") {
            push("gcp");
        } else if s.contains("azure") {
            push("azure");
        } else if s.contains("jenkins") || s.contains("cicd") || s.contains("ci/cd") {
            push("cicd");
        } else if s.contains("prometheus")
            || s.contains("datadog")
            || s.contains("observability")
            || s.contains("opentelemetry")
        {
            push("observability");
        } else if s.contains("sre") || s.contains("reliability") {
            push("sre");
        }
    }
    topics
}

/// Community Q&A with a self-generated fallback chain:
/// quoted repo questions first; when those come up empty (offline, no matching
/// topics, fetch failure), an LLM writes grounded Q&A; when no LLM is available,
/// deterministic questions built from your own resume bullets.
pub async fn community_qa_with_fallback(
    db: &sqlx::SqlitePool,
    resume: &crate::resume::CandidateProfile,
    skills: &[String],
    topics: &[String],
    max_items: usize,
) -> Vec<CommunityQA> {
    let quoted = fetch_community_qa(db, skills, max_items).await;
    if !quoted.is_empty() {
        return quoted;
    }
    if let Some(generated) = generate_qa(resume, skills, topics, max_items).await {
        if !generated.is_empty() {
            return generated;
        }
    }
    fallback_qa(resume, skills, max_items)
}

/// LLM-written Q&A grounded in the resume. Answers must share vocabulary with
/// your resume or the demanded skills, otherwise they are dropped.
async fn generate_qa(
    resume: &crate::resume::CandidateProfile,
    skills: &[String],
    topics: &[String],
    max_items: usize,
) -> Option<Vec<CommunityQA>> {
    let bullets: Vec<&str> = resume
        .experience
        .iter()
        .flat_map(|role| role.bullets.iter().map(|b| b.text.as_str()))
        .collect();
    if bullets.is_empty() {
        return None;
    }
    let system = "You write interview Q&A for ONE candidate. Hard rules:\n\
        1. Every answer must be grounded in the supplied resume bullets or the listed skills. Never invent experience, metrics, or employers.\n\
        2. Prefer questions the candidate can answer from their own bullets; say so in the answer by referencing the work.\n\
        3. Return valid JSON only, no markdown fences, exactly this shape:\n\
        {\"items\": [{\"question\": \"...\", \"answer\": \"...\", \"topic\": \"...\"}]}"
        .to_string();
    let user = format!(
        "Skills in demand: {skills}\nStudy topics: {topics}\nResume bullets:\n- {bullets}\n\nWrite up to {n} interview Q&A pairs the candidate can answer from these bullets.",
        skills = skills.iter().take(12).cloned().collect::<Vec<_>>().join(", "),
        topics = topics.join(", "),
        bullets = bullets.join("\n- "),
        n = max_items.max(1).min(12),
    );
    let value = crate::llm::chat_json(&system, &user).await.ok()?.1;
    let items = value.get("items")?.as_array()?;
    let vocab = vocab_of(resume, skills);
    let mut out = Vec::new();
    for item in items {
        let (Some(question), Some(answer), topic) = (
            item.get("question").and_then(|v| v.as_str()),
            item.get("answer").and_then(|v| v.as_str()),
            item.get("topic")
                .and_then(|v| v.as_str())
                .unwrap_or("general"),
        ) else {
            continue;
        };
        let question = question.trim();
        let answer = answer.trim();
        if question.len() < 8 || answer.len() < 8 || answer.len() > 800 {
            continue;
        }
        if !grounded(answer, &vocab) {
            continue;
        }
        out.push(CommunityQA {
            question: question.to_string(),
            answer: answer.to_string(),
            topic: topic.to_string(),
            source_repo: "generated for you".to_string(),
            source_license: String::new(),
            source_url: String::new(),
            generated: true,
        });
        if out.len() >= max_items.max(1).min(12) {
            break;
        }
    }
    Some(out)
}

fn vocab_of(
    resume: &crate::resume::CandidateProfile,
    skills: &[String],
) -> std::collections::HashSet<String> {
    let mut text = String::new();
    for skill in &resume.skills {
        text.push_str(&skill.name);
        text.push(' ');
    }
    for role in &resume.experience {
        text.push_str(&role.title);
        text.push(' ');
        text.push_str(&role.company);
        text.push(' ');
        for bullet in &role.bullets {
            text.push_str(&bullet.text);
            text.push(' ');
        }
    }
    for skill in skills {
        text.push_str(skill);
        text.push(' ');
    }
    normalize(&text)
        .split_whitespace()
        .filter(|w| !STOPWORDS.contains(w))
        .map(str::to_string)
        .collect()
}

fn grounded(answer: &str, vocab: &std::collections::HashSet<String>) -> bool {
    let normalized = normalize(answer);
    let words: Vec<&str> = normalized
        .split_whitespace()
        .filter(|w| !STOPWORDS.contains(w))
        .collect();
    if words.len() < 3 {
        return false;
    }
    words.iter().filter(|w| vocab.contains(**w)).count() >= 2
}

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "in", "on", "at", "to", "for", "with", "is", "are", "was",
    "were", "be", "been", "i", "my", "as", "by", "from", "that", "this", "it", "its", "into",
    "you", "your", "we", "they", "them", "what", "which", "when", "how", "why", "does", "would",
    "could", "should", "explain", "describe",
];

/// Deterministic last resort: one question per top skill, answered with your
/// own resume bullet where one exists, honestly labeled where none does.
pub fn fallback_qa(
    resume: &crate::resume::CandidateProfile,
    skills: &[String],
    max_items: usize,
) -> Vec<CommunityQA> {
    let mut out = Vec::new();
    for skill in skills.iter().take(max_items.max(1).min(12)) {
        let evidence = resume
            .experience
            .iter()
            .flat_map(|role| role.bullets.iter())
            .find(|b| mentions(&b.text, skill))
            .map(|b| b.text.clone());
        let (question, answer) = match evidence {
            Some(bullet) => (
                format!("Walk me through your experience with {}.", skill.trim()),
                format!("{bullet} Be ready to go two levels deeper: why this approach, what broke, what you would change."),
            ),
            None => (
                format!("How would you ramp up on {} quickly?", skill.trim()),
                "Study pointer — your resume shows no direct evidence here, so prepare a small demo project and be honest about the gap.".to_string(),
            ),
        };
        out.push(CommunityQA {
            question,
            answer,
            topic: skill.clone(),
            source_repo: "generated for you".to_string(),
            source_license: String::new(),
            source_url: String::new(),
            generated: true,
        });
        if out.len() >= max_items.max(1).min(12) {
            break;
        }
    }
    out
}
/// Fetch + parse + rank community Q&A for the given skills. Never fails the
/// caller: offline mode, fetch errors, and parse misses yield fewer items.
pub async fn fetch_community_qa(
    db: &sqlx::SqlitePool,
    skills: &[String],
    max_items: usize,
) -> Vec<CommunityQA> {
    if std::env::var("KNOWLEDGE_OFFLINE")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return Vec::new();
    }
    let topics = topics_for_skills(skills);
    if topics.is_empty() {
        return Vec::new();
    }
    let client = reqwest::Client::builder()
        .user_agent("RustJobAgent/0.6 (community knowledge)")
        .timeout(std::time::Duration::from_secs(30))
        .build();
    let client = match client {
        Ok(client) => client,
        Err(_) => return Vec::new(),
    };
    let fetches = topics.iter().map(|topic| {
        let client = client.clone();
        let topic = topic.clone();
        async move {
            let url = raw_url(&topic);
            let text = match crate::store::page_text(db, &url).await {
                Ok(Some(cached)) => Some(cached),
                _ => {
                    let response = client
                        .get(&url)
                        .send()
                        .await
                        .ok()
                        .filter(|r| r.status().is_success());
                    let mut fetched = None;
                    if let Some(response) = response {
                        if let Ok(body) = response.text().await {
                            if body.trim().chars().count() >= 500 {
                                let _ = crate::store::save_page_text(db, &url, &body).await;
                                fetched = Some(body);
                            }
                        }
                    }
                    fetched
                }
            };
            text.map(|text| (topic, text))
        }
    });
    let mut scored: Vec<(usize, CommunityQA)> = Vec::new();
    for fetched in futures::future::join_all(fetches)
        .await
        .into_iter()
        .flatten()
    {
        let (topic, text) = fetched;
        for mut qa in parse_devops_qa(&text, &topic) {
            let score = score_qa(&qa, skills);
            if score > 0 {
                qa.answer = clip(&qa.answer, 800);
                scored.push((score, qa));
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.question.cmp(&b.1.question)));
    scored
        .into_iter()
        .take(max_items.max(1).min(12))
        .map(|(_, qa)| qa)
        .collect()
}

/// Parse `<details><summary>Q</summary><br><b>A</b></details>` blocks.
/// Pure and unit-tested; tolerates missing answers and nested markup.
pub fn parse_devops_qa(markdown: &str, topic: &str) -> Vec<CommunityQA> {
    let re = details_re();
    let mut out = Vec::new();
    for caps in re.captures_iter(markdown) {
        let question = clean(&caps[1]);
        let answer = clean(&caps[2]);
        if question.len() < 8 || answer.len() < 8 {
            continue;
        }
        out.push(CommunityQA {
            question: clip(&question, 300),
            answer: clip(&answer, 800),
            topic: topic.to_string(),
            source_repo: DEVOPS_REPO.to_string(),
            source_license: DEVOPS_LICENSE.to_string(),
            source_url: page_url(topic),
            generated: false,
        });
        if out.len() >= 60 {
            break;
        }
    }
    out
}

fn details_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?s)<details>\s*<summary>(.*?)</summary>(.*?)</b>\s*</details>")
            .expect("details pattern")
    })
}

fn clean(html: &str) -> String {
    crate::providers::strip_html(html)
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
}

/// Distinct skill terms (len >= 2) mentioned anywhere in the Q&A.
pub fn score_qa(qa: &CommunityQA, skills: &[String]) -> usize {
    let hay = format!("{} {}", qa.question, qa.answer);
    skills
        .iter()
        .filter(|s| s.trim().len() >= 2 && mentions(&hay, s))
        .count()
}

fn mentions(text: &str, skill: &str) -> bool {
    let skill = normalize(skill);
    if skill.is_empty() {
        return false;
    }
    let text = format!(" {} ", normalize(text));
    text.contains(&format!(" {skill} "))
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

const PRIMER_BASE: &str = "https://github.com/donnemartin/system-design-primer";

/// Verified solution directories (existence checked via the GitHub API).
/// License is undeclared, so these are links only — never ingested.
pub fn primer_links(top_skills: &[String], components: &[String]) -> Vec<PrimerLink> {
    let hay = format!("{} {}", top_skills.join(" "), components.join(" ")).to_lowercase();
    let mut links = vec![PrimerLink {
        title: "System Design Primer — full guide".into(),
        url: PRIMER_BASE.into(),
    }];
    links.push(PrimerLink {
        title: "Awesome System Design resources (videos, articles, lists)".into(),
        url: "https://github.com/ashishps1/awesome-system-design-resources".into(),
    });
    fn push(links: &mut Vec<PrimerLink>, title: &str, slug: &str) {
        if links.len() < 4 {
            links.push(PrimerLink {
                title: title.into(),
                url: format!("{PRIMER_BASE}/tree/master/solutions/system_design/{slug}"),
            });
        }
    }
    if ["social", "feed", "timeline", "fanout", "twitter"]
        .iter()
        .any(|m| hay.contains(m))
    {
        push(&mut links, "Design Twitter (fanout, timelines)", "twitter");
    }
    if ["blob", "storage", "file", "pastebin", "shortener", "url"]
        .iter()
        .any(|m| hay.contains(m))
    {
        push(
            &mut links,
            "Design Pastebin (storage-heavy services)",
            "pastebin",
        );
    }
    if ["crawl", "search", "index"].iter().any(|m| hay.contains(m)) {
        push(&mut links, "Design a web crawler", "web_crawler");
    }
    if ["cache", "redis", "memcache"]
        .iter()
        .any(|m| hay.contains(m))
    {
        push(&mut links, "Query caching design notes", "query_cache");
    }
    if links.len() < 3 {
        push(&mut links, "Scaling a web app on AWS", "scaling_aws");
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
### Kubernetes 101

<details>
<summary>What is Kubernetes? Why organizations are using it?</summary><br><b>

Kubernetes is an open-source system for managing containers.

</b></details>

<details>
<summary>What is kubectl?</summary><br><b>

Kubectl is the Kubernetes command line tool.

</b></details>

<details>
<summary>Broken block without answer</summary><br><b>
</b></details>
"#;

    #[test]
    fn parses_details_blocks() {
        let items = parse_devops_qa(SAMPLE, "kubernetes");
        assert_eq!(items.len(), 2);
        assert!(items[0].question.contains("What is Kubernetes"));
        assert!(items[0].answer.contains("open-source system"));
        assert_eq!(items[0].topic, "kubernetes");
        assert!(items[0].source_url.contains("devops-exercises"));
        assert!(items[0].source_license.contains("CC BY-NC"));
    }

    #[test]
    fn topics_follow_skills() {
        let topics = topics_for_skills(&["Kubernetes".into(), "Python".into(), "Rust".into()]);
        assert_eq!(topics, vec!["kubernetes", "python"]);
        let topics = topics_for_skills(&["PostgreSQL".into()]);
        assert!(topics.contains(&"sql".to_string()));
        assert!(topics.contains(&"databases".to_string()));
        // Cap and dedupe.
        let many: Vec<String> = [
            "Kubernetes",
            "Docker",
            "Python",
            "SQL",
            "Git",
            "Linux",
            "Kafka",
            "Terraform",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(topics_for_skills(&many).len() <= 4);
    }

    #[test]
    fn scoring_counts_skill_hits() {
        let qa = CommunityQA {
            question: "What is Kubernetes?".into(),
            answer: "A container orchestrator.".into(),
            topic: "kubernetes".into(),
            source_repo: DEVOPS_REPO.into(),
            source_license: DEVOPS_LICENSE.into(),
            source_url: page_url("kubernetes"),
            generated: false,
        };
        assert_eq!(score_qa(&qa, &["Kubernetes".into(), "Go".into()]), 1);
        assert_eq!(score_qa(&qa, &["Go".into()]), 0);
    }

    #[test]
    fn fallback_uses_resume_evidence_or_labels_gaps() {
        let resume = fallback_resume();
        let items = fallback_qa(&resume, &["Rust".to_string(), "Kubernetes".to_string()], 8);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|item| item.generated));
        assert!(items[0].answer.contains("Built Rust services"));
        assert!(items[1].answer.contains("no direct evidence"));
        assert!(items[1].question.contains("Kubernetes"));
    }

    #[test]
    fn grounded_rejects_invented_answers() {
        use crate::resume::{EvidenceBullet, Experience, SkillEvidence};
        let resume = crate::resume::CandidateProfile {
            name: None,
            headline: None,
            summary: None,
            skills: vec![SkillEvidence {
                name: "Rust".into(),
                evidence: vec![],
                confidence: 0.9,
            }],
            experience: vec![Experience {
                company: "Acme".into(),
                title: "Engineer".into(),
                dates: None,
                location: None,
                bullets: vec![EvidenceBullet {
                    text: "Built Rust services.".into(),
                    source_excerpt: String::new(),
                }],
            }],
            projects: vec![],
            education: vec![],
            open_source: vec![], layout: vec![], custom_fields: vec![],
            source_file: None,
            extraction_warnings: vec![],
        };
        let vocab = vocab_of(&resume, &["Rust".to_string()]);
        assert!(grounded(
            "Built Rust services for checkout with async runtimes",
            &vocab
        ));
        assert!(!grounded("Won the Nobel Prize for quantum physics", &vocab));
    }

    #[test]
    fn primer_links_match_stack() {
        let links = primer_links(&["Kafka".into()], &["Event streaming (Kafka)".into()]);
        assert!(links.iter().any(|l| l.url.contains("system-design-primer")));
        let links = primer_links(&["Redis".into()], &["Caching".into()]);
        assert!(links.iter().any(|l| l.url.contains("query_cache")));
    }

    fn fallback_resume() -> crate::resume::CandidateProfile {
        use crate::resume::{EvidenceBullet, Experience, SkillEvidence};
        crate::resume::CandidateProfile {
            name: None,
            headline: None,
            summary: None,
            skills: vec![SkillEvidence {
                name: "Rust".into(),
                evidence: vec![],
                confidence: 0.9,
            }],
            experience: vec![Experience {
                company: "Acme".into(),
                title: "Engineer".into(),
                dates: None,
                location: None,
                bullets: vec![EvidenceBullet {
                    text: "Built Rust services.".into(),
                    source_excerpt: String::new(),
                }],
            }],
            projects: vec![],
            education: vec![],
            open_source: vec![], layout: vec![], custom_fields: vec![],
            source_file: None,
            extraction_warnings: vec![],
        }
    }
}
