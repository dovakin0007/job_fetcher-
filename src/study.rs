//! Company knowledge base + per-company study guides.
//!
//! Every application teaches the system about a company: roles pursued,
//! skills demanded, seniority, and the user's own notes (process intel,
//! known questions). The study guide layers DSA topics, a system-design
//! brief, and resume tips on top — inferred from that company's postings,
//! never presented as insider information. An LLM rewrite sharpens the
//! framing when configured; quoted evidence is validated like everywhere else.

use crate::models::Job;
use crate::resume::CandidateProfile as ResumeProfile;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsaTopic {
    pub topic: String,
    pub priority: String,
    pub why: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesignComponent {
    pub name: String,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompanyBundle {
    pub company: String,
    pub roles: Vec<String>,
    pub top_skills: Vec<String>,
    pub seniority: String,
    pub applications: usize,
    pub dsa_topics: Vec<DsaTopic>,
    pub system_design: Vec<DesignComponent>,
    pub capacity_notes: String,
    pub resume_tips: Vec<String>,
    /// Community Q&A quoted verbatim from CC BY-NC sources (attributed).
    #[serde(default)]
    pub community_qa: Vec<crate::knowledge::CommunityQA>,
    /// Links only — the primer has no declared license, so nothing is copied.
    #[serde(default)]
    pub primer_links: Vec<crate::knowledge::PrimerLink>,
    /// Static curated DSA practice lists (links only, verified URLs).
    #[serde(default)]
    pub dsa_lists: Vec<crate::knowledge::PrimerLink>,
    /// Classic LLD prompts (generic ideas, our own wording) + approach note.
    #[serde(default)]
    pub lld_practice: Vec<String>,
    /// LLD reading (links only, verified repo).
    #[serde(default)]
    pub lld_links: Vec<crate::knowledge::PrimerLink>,
    #[serde(default)]
    pub generated_by: Option<String>,
    pub integrity_note: String,
}

/// Normalised company key: case-insensitive, common suffixes stripped so
/// "Acme Inc." and "acme" accumulate into one profile.
pub fn company_key(name: &str) -> String {
    let mut key = name.trim().to_ascii_lowercase();
    for suffix in [
        " pvt. ltd.",
        " pvt ltd",
        " private limited",
        " technologies",
        " technology",
        " labs",
        " inc.",
        " inc",
        " llc",
        " ltd.",
        " ltd",
        " corp.",
        " corp",
        " co.",
        " gmbh",
        " pte. ltd.",
    ] {
        if let Some(stripped) = key.strip_suffix(suffix) {
            key = stripped.trim().to_string();
            break;
        }
    }
    key
}

fn seniority_of(titles: &[String]) -> String {
    let joined = titles.join(" ").to_lowercase();
    if joined.contains("staff") || joined.contains("principal") || joined.contains("lead") {
        "staff/lead".into()
    } else if joined.contains("senior") || joined.contains("sr ") || joined.contains("sr.") {
        "senior".into()
    } else if joined.contains("junior") || joined.contains("jr ") || joined.contains("intern") {
        "junior".into()
    } else {
        "mid-level".into()
    }
}

fn is_data_track(text: &str, skills: &[String]) -> bool {
    let hay = format!(
        "{} {}",
        text.to_lowercase(),
        skills.join(" ").to_lowercase()
    );
    [
        "pandas",
        "spark",
        "airflow",
        "dbt",
        "etl",
        "warehouse",
        "ml ",
        "machine learning",
        "numpy",
    ]
    .iter()
    .any(|marker| hay.contains(marker))
}

fn scale_hints(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut hints = Vec::new();
    for marker in [
        "distributed",
        "high-scale",
        "large-scale",
        "millions",
        "billions",
        "low-latency",
        "real-time",
        "realtime",
        "microservices",
        "multi-region",
        "high-availability",
        "event-driven",
    ] {
        if lower.contains(marker) && !hints.contains(&marker.to_string()) {
            hints.push(marker.to_string());
        }
    }
    hints
}

pub fn build_bundle(
    company: &str,
    jobs: &[&Job],
    applications: usize,
    resume: &ResumeProfile,
) -> CompanyBundle {
    let mut roles: Vec<String> = Vec::new();
    for job in jobs {
        if !roles.iter().any(|r| r.eq_ignore_ascii_case(&job.title)) {
            roles.push(job.title.clone());
        }
    }
    // Skill demand across this company's postings (posting order ~ relevance).
    let mut demand: Vec<(usize, String)> = Vec::new();
    for job in jobs {
        for skill in &job.skills {
            if let Some(entry) = demand
                .iter_mut()
                .find(|(_, s)| s.eq_ignore_ascii_case(skill))
            {
                entry.0 += 1;
            } else {
                demand.push((1, skill.clone()));
            }
        }
        // Also mine posting text for the candidate's own skills.
        for skill in &resume.skills {
            if has_term(&job.description, &skill.name)
                && !demand
                    .iter()
                    .any(|(_, s)| s.eq_ignore_ascii_case(&skill.name))
            {
                demand.push((1, skill.name.clone()));
            }
        }
    }
    demand.sort_by(|a, b| b.0.cmp(&a.0));
    let top_skills: Vec<String> = demand.into_iter().take(12).map(|(_, s)| s).collect();

    let seniority = seniority_of(&roles);
    let senior = seniority == "senior" || seniority == "staff/lead";
    let combined_text = jobs
        .iter()
        .map(|j| format!("{} {}", j.title, j.description))
        .collect::<Vec<_>>()
        .join(" ");
    let data_track = is_data_track(&combined_text, &top_skills);
    let scale = scale_hints(&combined_text);

    let mut dsa_topics = Vec::new();
    let push = |topics: &mut Vec<DsaTopic>, topic: &str, priority: &str, why: String| {
        topics.push(DsaTopic {
            topic: topic.into(),
            priority: priority.into(),
            why,
        });
    };
    if data_track {
        push(
            &mut dsa_topics,
            "SQL & window functions",
            "high",
            "Data roles screen on joins, aggregations, and window functions first.".into(),
        );
        push(
            &mut dsa_topics,
            "Python data wrangling",
            "high",
            format!(
                "Your postings name {} — practice real transformations.",
                top_skills
                    .iter()
                    .take(4)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        push(
            &mut dsa_topics,
            "Data modeling (star schema, normalization)",
            "medium",
            "Design-how-you-store-it questions follow the SQL round.".into(),
        );
    } else {
        push(
            &mut dsa_topics,
            "Arrays, hashing & two pointers",
            "high",
            "The default backend screen; warm up here regardless of level.".into(),
        );
        push(
            &mut dsa_topics,
            "Trees & BSTs",
            "high",
            "Recursion + traversal depth is the most common follow-up probe.".into(),
        );
        push(
            &mut dsa_topics,
            "Graphs (BFS/DFS, topo sort)",
            if senior { "high" } else { "medium" },
            if senior {
                "Senior+ loops lean on dependency and network problems.".into()
            } else {
                "Know the traversals; depth matters less below senior.".into()
            },
        );
        push(
            &mut dsa_topics,
            "Heaps & top-K",
            "medium",
            "Pairs with streaming/scale questions in backend interviews.".into(),
        );
        push(
            &mut dsa_topics,
            "Dynamic programming",
            if senior { "high" } else { "medium" },
            if senior {
                "Senior bars include at least one DP optimization.".into()
            } else {
                "Classic 1D/knapsack patterns suffice at this level.".into()
            },
        );
        push(
            &mut dsa_topics,
            "Concurrency (threads, locks, async)",
            if senior { "high" } else { "medium" },
            format!(
                "{} hints at concurrent systems.",
                if scale.is_empty() {
                    "Backend work".into()
                } else {
                    scale.join(", ")
                }
            ),
        );
        if senior {
            push(
                &mut dsa_topics,
                "Low-level design (patterns, SOLID, rate limiter / cache design)",
                "high",
                "Staff/lead rounds explicitly grade extensible design in code.".into(),
            );
        }
    }

    let mut system_design = vec![
        DesignComponent {
            name: "Requirements & capacity estimation".into(),
            notes: "Always start here: clarify reads vs writes, then estimate with YOUR assumed numbers — never quote company figures you don't have.".into(),
        },
        DesignComponent {
            name: "API & load balancing".into(),
            notes: "REST vs gRPC per the posting; gateway, LB, and rate limiting in front.".into(),
        },
    ];
    for (marker, component, notes) in [
        (
            "kafka",
            "Event streaming (Kafka)",
            "Topics/partitions, consumer groups, exactly-once tradeoffs.",
        ),
        (
            "rabbitmq",
            "Message queues",
            "Work distribution, retries, dead-lettering.",
        ),
        (
            "redis",
            "Caching",
            "Cache-aside vs write-through, TTLs, invalidation.",
        ),
        (
            "postgres",
            "Relational store",
            "Schema, indexes, read replicas, partitioning.",
        ),
        (
            "mysql",
            "Relational store",
            "Schema, indexes, read replicas, partitioning.",
        ),
        (
            "clickhouse",
            "Analytics store",
            "Columnar layout, sharding, materialized views.",
        ),
        (
            "duckdb",
            "Embedded analytics",
            "When in-process OLAP beats a warehouse.",
        ),
        (
            "kubernetes",
            "Orchestration (Kubernetes)",
            "Deployments, autoscaling, rolling updates.",
        ),
        (
            "docker",
            "Containers",
            "Image hygiene, resource limits, local parity.",
        ),
        ("grpc", "gRPC APIs", "Contracts, streaming, deadlines."),
        (
            "elasticsearch",
            "Search",
            "Inverted index, relevance tuning, scaling shards.",
        ),
        (
            "mongo",
            "Document store",
            "Embedding vs referencing, index strategy.",
        ),
    ] {
        if top_skills
            .iter()
            .any(|s| s.to_ascii_lowercase().contains(marker))
        {
            system_design.push(DesignComponent {
                name: component.into(),
                notes: notes.into(),
            });
        }
        if system_design.len() >= 8 {
            break;
        }
    }
    system_design.push(DesignComponent {
        name: "Consistency & failure handling".into(),
        notes: "Retries with backoff, idempotency, and what degrades first under load.".into(),
    });
    let component_names: Vec<String> = system_design.iter().map(|c| c.name.clone()).collect();
    let primer_links = crate::knowledge::primer_links(&top_skills, &component_names);

    let capacity_notes = if scale.is_empty() {
        "No scale signals in these postings — practice the method, not numbers: (1) state assumed DAU and read/write ratio, (2) derive storage and bandwidth, (3) name the bottleneck component and how you would scale it.".into()
    } else {
        format!("These postings mention {}. Anchor your estimates to those words: state the assumption out loud, then derive storage/bandwidth and the bottleneck as usual.", scale.join(", "))
    };

    let mut resume_tips = Vec::new();
    for skill in top_skills.iter().take(5) {
        resume_tips.push(format!(
            "Lead with {skill}: have a 2-minute story plus a metric from your resume ready."
        ));
    }
    let missing: Vec<String> = top_skills
        .iter()
        .filter(|s| !resume.skills.iter().any(|r| r.name.eq_ignore_ascii_case(s)))
        .take(3)
        .cloned()
        .collect();
    for skill in missing {
        resume_tips.push(format!("Close the {skill} gap before the interview: build one small thing and be able to explain it."));
    }

    CompanyBundle {
        company: company.to_string(),
        roles,
        top_skills,
        seniority,
        applications,
        dsa_topics,
        system_design,
        capacity_notes,
        resume_tips,
        community_qa: Vec::new(),
        primer_links,
        dsa_lists: dsa_practice_lists(),
        lld_practice: lld_prompts(senior),
        lld_links: lld_reading(),
        generated_by: None,
        integrity_note: "Inferred from this company's postings you saved, not insider information. Verify specifics against the actual job description.".into(),
    }
}

/// Curated DSA practice lists (links only — problem statements stay on
/// LeetCode). URLs verified against ashishps1/awesome-leetcode-resources.
fn dsa_practice_lists() -> Vec<crate::knowledge::PrimerLink> {
    vec![
        crate::knowledge::PrimerLink {
            title: "Blind 75 (essential patterns)".into(),
            url:
                "https://leetcode.com/discuss/general-discussion/460599/blind-75-leetcode-questions"
                    .into(),
        },
        crate::knowledge::PrimerLink {
            title: "LeetCode Top 100 Liked".into(),
            url: "https://leetcode.com/studyplan/top-100-liked/".into(),
        },
        crate::knowledge::PrimerLink {
            title: "LeetCode Top Interview 150".into(),
            url: "https://leetcode.com/studyplan/top-interview-150/".into(),
        },
        crate::knowledge::PrimerLink {
            title: "AlgoMaster 300 (pattern-sorted)".into(),
            url: "https://algomaster.io/practice/dsa-patterns".into(),
        },
    ]
}

/// Classic LLD prompts (generic problem ideas in our own wording) plus the
/// approach to rehearse. Seniors get a concurrency-flavored extra.
fn lld_prompts(senior: bool) -> Vec<String> {
    let mut prompts = vec![
        "Approach first: clarify entities, then relationships, then patterns (Strategy, Observer, Factory), then thread-safety.".to_string(),
        "Design a parking lot (spots, tickets, payments, concurrency at entry/exit).".to_string(),
        "Design an elevator system (requests, scheduling, multi-lift coordination).".to_string(),
        "Design Tic-Tac-Toe or Chess with move validation and win detection.".to_string(),
        "Design a library management system (books, members, lending rules).".to_string(),
        "Design an ATM (cash, accounts, failure handling).".to_string(),
        "Design a vending machine (state machine, change, inventory).".to_string(),
    ];
    if senior {
        prompts.push("Senior extra: design a rate limiter and a thread-safe LRU cache, then defend the tradeoffs.".to_string());
    }
    prompts
}

/// LLD reading (links only — verified repo).
fn lld_reading() -> Vec<crate::knowledge::PrimerLink> {
    vec![crate::knowledge::PrimerLink {
        title: "Awesome Low-Level Design (patterns, OOP, machine coding)".into(),
        url: "https://github.com/ashishps1/awesome-low-level-design".into(),
    }]
}

/// Prompt for the LLM rewrite: sharper framing grounded in the same postings.
pub fn study_prompts(bundle: &CompanyBundle, jobs_context: &str) -> (String, String) {
    let system = "You sharpen an interview study guide for ONE company. Hard rules:\n\
        1. Stay within the supplied topics, components, and skills. Never invent company facts, interview questions asked there, salaries, or metrics.\n\
        2. You may reorder by priority and rewrite the 'why'/'notes' to be company-specific, citing the supplied posting signals.\n\
        3. Keep every topic/component; do not add new technologies.\n\
        4. Return valid JSON only, no markdown fences, exactly this shape:\n\
        {\"dsa_topics\": [{\"topic\": \"...\", \"priority\": \"high|medium|low\", \"why\": \"...\"}], \
        \"system_design\": [{\"name\": \"...\", \"notes\": \"...\"}], \
        \"capacity_notes\": \"...\", \"resume_tips\": [\"...\"]}"
        .to_string();
    let user = format!(
        "Company: {company} ({seniority} roles: {roles}).\nDemanded skills: {skills}\nPosting signals:\n{context}\n\nCurrent guide:\n{guide}\n\nSharpen it for this company and level. Keep the same topics and components.",
        company = bundle.company,
        seniority = bundle.seniority,
        roles = bundle.roles.join("; "),
        skills = bundle.top_skills.join(", "),
        context = clip(jobs_context, 6_000),
        guide = serde_json::to_string_pretty(&serde_json::json!({
            "dsa_topics": bundle.dsa_topics,
            "system_design": bundle.system_design,
            "capacity_notes": bundle.capacity_notes,
            "resume_tips": bundle.resume_tips,
        })).unwrap_or_default(),
    );
    (system, user)
}

/// Merges an LLM rewrite: same topics/components (matched by name), rewritten
/// framing within length caps. Unknown technologies are dropped.
pub fn merge_study(bundle: &mut CompanyBundle, value: &serde_json::Value) {
    if let Some(items) = value.get("dsa_topics").and_then(|v| v.as_array()) {
        let mut merged = Vec::new();
        for item in items {
            let (Some(topic), Some(priority), Some(why)) = (
                item.get("topic").and_then(|v| v.as_str()),
                item.get("priority").and_then(|v| v.as_str()),
                item.get("why").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            if topic.trim().is_empty() || why.trim().is_empty() || why.len() > 300 {
                continue;
            }
            if !["high", "medium", "low"].contains(&priority.trim().to_ascii_lowercase().as_str()) {
                continue;
            }
            if !bundle
                .dsa_topics
                .iter()
                .any(|t| t.topic.eq_ignore_ascii_case(topic.trim()))
            {
                continue;
            }
            merged.push(DsaTopic {
                topic: topic.trim().to_string(),
                priority: priority.trim().to_ascii_lowercase(),
                why: why.trim().to_string(),
            });
        }
        if !merged.is_empty() {
            bundle.dsa_topics = merged;
        }
    }
    if let Some(items) = value.get("system_design").and_then(|v| v.as_array()) {
        let mut merged = Vec::new();
        for item in items {
            let (Some(name), Some(notes)) = (
                item.get("name").and_then(|v| v.as_str()),
                item.get("notes").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            if name.trim().is_empty() || notes.trim().is_empty() || notes.len() > 400 {
                continue;
            }
            if !bundle
                .system_design
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(name.trim()))
            {
                continue;
            }
            merged.push(DesignComponent {
                name: name.trim().to_string(),
                notes: notes.trim().to_string(),
            });
        }
        if !merged.is_empty() {
            bundle.system_design = merged;
        }
    }
    if let Some(notes) = value.get("capacity_notes").and_then(|v| v.as_str()) {
        if !notes.trim().is_empty() && notes.len() <= 600 {
            bundle.capacity_notes = notes.trim().to_string();
        }
    }
    if let Some(items) = value.get("resume_tips").and_then(|v| v.as_array()) {
        let kept: Vec<String> = items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 240)
            .take(8)
            .map(str::to_string)
            .collect();
        if !kept.is_empty() {
            bundle.resume_tips = kept;
        }
    }
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
    use crate::models::Job as ModelJob;
    use crate::resume::{EvidenceBullet, Experience, SkillEvidence};

    fn resume() -> ResumeProfile {
        ResumeProfile {
            name: Some("Test".into()),
            headline: None,
            summary: None,
            skills: vec![
                SkillEvidence {
                    name: "Rust".into(),
                    evidence: vec![],
                    confidence: 0.9,
                },
                SkillEvidence {
                    name: "Kafka".into(),
                    evidence: vec![],
                    confidence: 0.8,
                },
            ],
            experience: vec![Experience {
                company: "Acme".into(),
                title: "Senior Software Engineer".into(),
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

    fn job(title: &str, desc: &str, skills: &[&str]) -> ModelJob {
        ModelJob {
            external_id: "1".into(),
            source: "test".into(),
            title: title.into(),
            company: "Acme Inc.".into(),
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
    fn bundle_infers_level_and_stack() {
        let jobs = vec![job(
            "Senior Backend Engineer",
            "Build distributed Rust services with Kafka. Millions of events.",
            &["Rust", "Kafka"],
        )];
        let refs: Vec<&ModelJob> = jobs.iter().collect();
        let bundle = build_bundle("Acme Inc.", &refs, 2, &resume());
        assert_eq!(bundle.seniority, "senior");
        assert!(bundle.top_skills.contains(&"Rust".to_string()));
        assert!(bundle
            .system_design
            .iter()
            .any(|c| c.name.contains("Kafka")));
        assert!(bundle
            .dsa_topics
            .iter()
            .any(|t| t.topic.contains("Concurrency")));
        assert!(
            bundle.capacity_notes.contains("Millions")
                || bundle.capacity_notes.contains("millions")
                || bundle.capacity_notes.contains("distributed")
        );
    }

    #[test]
    fn company_key_merges_variants() {
        assert_eq!(company_key("Acme Inc."), company_key("acme"));
        assert_eq!(company_key("Foo Labs"), "foo");
    }

    #[test]
    fn merge_drops_unknown_tech_and_bad_shapes() {
        let jobs = vec![job("Backend Engineer", "Build Rust services.", &["Rust"])];
        let refs: Vec<&ModelJob> = jobs.iter().collect();
        let mut bundle = build_bundle("Acme", &refs, 1, &resume());
        let rewrite = serde_json::json!({
            "dsa_topics": [
                {"topic": "Arrays, hashing & two pointers", "priority": "high", "why": "Core screen."},
                {"topic": "Quantum algorithms", "priority": "high", "why": "Invented."},
                {"topic": "", "priority": "high", "why": "Empty."},
            ],
            "system_design": [],
            "capacity_notes": "",
            "resume_tips": []
        });
        merge_study(&mut bundle, &rewrite);
        assert_eq!(bundle.dsa_topics.len(), 1);
        assert!(bundle.dsa_topics[0].why.contains("Core screen"));
    }

    #[test]
    fn bundle_carries_dsa_lld_and_hld_links() {
        let jobs = vec![job("Backend Engineer", "Build Rust services.", &["Rust"])];
        let refs: Vec<&ModelJob> = jobs.iter().collect();
        let bundle = build_bundle("Acme", &refs, 1, &resume());
        assert!(bundle.dsa_lists.iter().any(|l| l.url.contains("blind-75")));
        assert!(bundle
            .lld_practice
            .iter()
            .any(|p| p.contains("parking lot")));
        assert!(bundle
            .lld_links
            .iter()
            .any(|l| l.url.contains("awesome-low-level-design")));
        assert!(bundle
            .primer_links
            .iter()
            .any(|l| l.url.contains("awesome-system-design-resources")));
        // Junior bundles skip the senior-only extra.
        assert!(!bundle
            .lld_practice
            .iter()
            .any(|p| p.contains("Senior extra")));
    }

    #[test]
    fn old_cached_guides_still_parse() {
        // Payloads cached before dsa_lists/lld_* existed must deserialize.
        let old = serde_json::json!({
            "company": "Acme",
            "roles": ["Backend Engineer"],
            "top_skills": ["Rust"],
            "seniority": "mid-level",
            "applications": 1,
            "dsa_topics": [],
            "system_design": [],
            "capacity_notes": "x",
            "resume_tips": [],
            "integrity_note": "x"
        });
        let bundle: CompanyBundle = serde_json::from_value(old).unwrap();
        assert!(bundle.dsa_lists.is_empty());
        assert!(bundle.lld_practice.is_empty());
    }
}
