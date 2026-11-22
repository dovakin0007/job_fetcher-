use crate::models::{CandidateProfile, Job};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

pub mod index;

// Base policy weights. These are the starting point; the effective weights are
// derived per-resume by `dynamic_weights_*` (zero-signal components are dropped
// and the remainder is renormalised). Kept public for the weighting test.
pub const SKILLS_WEIGHT: f32 = 0.35;
pub const EXPERIENCE_WEIGHT: f32 = 0.20;
pub const TITLE_WEIGHT: f32 = 0.25;
pub const LOCATION_WEIGHT: f32 = 0.20;

#[derive(Clone, Copy, Default)]
pub struct Breakdown {
    pub total: f32,
    pub skill: f32,
    pub experience: f32,
    pub title: f32,
    pub location: f32,
    pub excluded: bool,
}

#[derive(Clone)]
pub struct Ranked {
    pub position: usize,
    pub rank: u32,
    pub breakdown: Breakdown,
    pub matched: Vec<String>,
    pub missing: Vec<String>,
}

// ---------------------------------------------------------------------------
// Resume-derived signals
// ---------------------------------------------------------------------------

/// Everything the scorer needs that can be derived from the resume itself.
///
/// Built from `models::CandidateProfile` (preferences) plus, when available,
/// the parsed `resume::CandidateProfile` and the flattened experience text.
/// No fixed technology list: aliases, title/location corpora and strength
/// normalisers all come from these inputs.
#[derive(Clone, Default)]
pub struct ResumeSignals {
    /// Canonical skill key -> user override weight (0..10), if any.
    pub user_weights: HashMap<String, f32>,
    /// Canonical skill key -> resume confidence 0..1 (from evidence, if known).
    pub confidence: HashMap<String, f32>,
    /// Canonical skill key -> evidence bullet count in the resume.
    pub evidence_counts: HashMap<String, usize>,
    pub max_evidence: usize,
    /// Mentions of each skill inside the experience text.
    pub mention_counts: HashMap<String, usize>,
    pub max_mentions: usize,
    /// Dynamic cap for mention saturation, derived from experience length.
    pub mention_cap: usize,
    /// Words in the experience text.
    pub experience_words: usize,
    /// Skills mentioned in the summary.
    pub summary_hits: HashSet<String>,
    /// Title corpus: preferred titles + resume headline + experience titles.
    pub title_corpus: Vec<String>,
    /// Location corpus: preferred locations + resume locations.
    pub location_corpus: Vec<String>,
    /// Preferred titles only (for the substring bonus).
    pub preferred_titles: Vec<String>,
    /// Years of experience: prefs value if > 0 else resume date estimate.
    pub years: f32,
    pub has_experience_signal: bool,
    pub has_title_signal: bool,
    pub has_location_signal: bool,
    /// Short-form -> canonical key, built only from this resume's skills.
    pub aliases: HashMap<String, String>,
    /// Preference skills plus skills extracted from the uploaded resume.
    pub skill_labels: Vec<String>,
    /// Effective component weights (skills, experience, title, location).
    pub weights: (f32, f32, f32, f32),
}

fn canonical_key(tokens: &[String]) -> String {
    tokens.join(" ")
}

fn build_alias_table(skill_names: &[String]) -> HashMap<String, String> {
    // Fully resume-derived: for each skill in THIS resume, register generic
    // short forms so "Node.js" <-> "node", multi-word skills <-> acronyms,
    // without any hard-coded technology list.
    let mut aliases = HashMap::new();
    for name in skill_names {
        let toks = tokens(name);
        if toks.is_empty() {
            continue;
        }
        let full = toks.join(" ");
        // "Node.js" -> ["node","js"]: alias the core token when the suffix
        // looks like a short runtime suffix (<=3 chars).
        if toks.len() == 2 && toks[1].len() <= 3 {
            aliases
                .entry(toks[0].clone())
                .or_insert_with(|| full.clone());
            // Also map the compact form "nodejs".
            aliases
                .entry(format!("{}{}", toks[0], toks[1]))
                .or_insert_with(|| full.clone());
        }
        // Multi-word skill -> acronym ("Google Cloud Pub Sub" -> "gcps").
        if toks.len() >= 2 {
            let acronym: String = toks.iter().filter_map(|t| t.chars().next()).collect();
            if acronym.len() >= 2 {
                aliases.entry(acronym).or_insert_with(|| full.clone());
            }
            // First-token shorthand for long dotted names is handled by the
            // prefix rule in `tokens_equal`, no fixed list needed.
        }
        // Compact form without separators ("googlebigquery").
        let compact = toks.concat();
        if compact != full.replace(' ', "") {
            aliases.entry(compact).or_insert_with(|| full.clone());
        }
    }
    aliases
}

fn resolve_alias(key: &str, aliases: &HashMap<String, String>) -> String {
    aliases.get(key).cloned().unwrap_or_else(|| key.to_string())
}

impl ResumeSignals {
    pub fn from_parts(
        prefs: &CandidateProfile,
        resume: Option<&crate::resume::CandidateProfile>,
        experience: &str,
    ) -> Self {
        let mut user_weights = HashMap::new();
        for w in &prefs.skill_weights {
            let key = canonical_key(&tokens(&w.skill));
            if !key.is_empty() {
                user_weights.insert(key, w.weight.clamp(0.0, 10.0));
            }
        }

        let mut confidence: HashMap<String, f32> = HashMap::new();
        let mut evidence_counts: HashMap<String, usize> = HashMap::new();
        let mut max_evidence = 0usize;
        if let Some(r) = resume {
            for s in &r.skills {
                let key = canonical_key(&tokens(&s.name));
                if key.is_empty() {
                    continue;
                }
                confidence.insert(key.clone(), s.confidence.clamp(0.0, 1.0));
                let n = s.evidence.len();
                evidence_counts.insert(key, n);
                max_evidence = max_evidence.max(n);
            }
        }

        let exp_tokens = tokens(experience);
        let experience_words = exp_tokens.len();
        // Dynamic saturation cap: longer resumes need more mentions to saturate.
        // ~60 words per unit of cap, clamped to 1..6. No magic "3" anymore.
        let mention_cap = ((experience_words / 60).max(1)).min(6);

        let mut mention_counts: HashMap<String, usize> = HashMap::new();
        let mut max_mentions = 0usize;
        // Count mentions against the union of pref skills (+ resume skills).
        let mut all_skills: Vec<String> = prefs.skills.clone();
        if let Some(r) = resume {
            for s in &r.skills {
                if !all_skills.iter().any(|x| x.eq_ignore_ascii_case(&s.name)) {
                    all_skills.push(s.name.clone());
                }
            }
        }
        for skill in &all_skills {
            let st = tokens(skill);
            if st.is_empty() {
                continue;
            }
            let key = canonical_key(&st);
            let c = phrase_count(&exp_tokens, &st);
            // Also count via alias core (e.g. "node" for "node js").
            let c = if c == 0 && st.len() == 2 && st[1].len() <= 3 {
                phrase_count(&exp_tokens, &st[..1])
            } else {
                c
            };
            if c > 0 {
                mention_counts.insert(key.clone(), c);
                max_mentions = max_mentions.max(c);
            }
        }

        let mut summary_hits = HashSet::new();
        for skill in &all_skills {
            let st = tokens(skill);
            if st.is_empty() {
                continue;
            }
            if phrase_count(&tokens(&prefs.summary), &st) > 0 {
                summary_hits.insert(canonical_key(&st));
            }
        }
        if let Some(r) = resume {
            if let Some(summary) = r.summary.as_deref() {
                let stoks = tokens(summary);
                for skill in &all_skills {
                    let st = tokens(skill);
                    if !st.is_empty() && phrase_count(&stoks, &st) > 0 {
                        summary_hits.insert(canonical_key(&st));
                    }
                }
            }
        }

        // Title corpus: preferred + headline + experience titles.
        let mut title_corpus: Vec<String> = prefs.preferred_titles.clone();
        if let Some(r) = resume {
            if let Some(h) = r.headline.as_deref() {
                if !h.trim().is_empty() {
                    title_corpus.push(h.to_string());
                }
            }
            for e in &r.experience {
                if !e.title.trim().is_empty() {
                    title_corpus.push(e.title.clone());
                }
            }
        }
        // Location corpus: preferred + resume locations.
        let mut location_corpus: Vec<String> = prefs.preferred_locations.clone();
        if let Some(r) = resume {
            for e in &r.experience {
                if let Some(loc) = e.location.as_deref() {
                    if !loc.trim().is_empty()
                        && !location_corpus.iter().any(|x| x.eq_ignore_ascii_case(loc))
                    {
                        location_corpus.push(loc.to_string());
                    }
                }
            }
        }

        let estimated = resume.map(estimated_years).unwrap_or(0.0);
        let years = if prefs.experience_years > 0.0 {
            prefs.experience_years
        } else {
            estimated
        };
        let has_experience_signal = years > 0.0 || estimated > 0.0 || !experience.trim().is_empty();
        let has_title_signal = !title_corpus.iter().all(|t| t.trim().is_empty());
        let has_location_signal = !location_corpus.iter().all(|l| l.trim().is_empty());

        let aliases = build_alias_table(&all_skills);
        let weights = renormalised_weights(
            !prefs.skills.is_empty() || !all_skills.is_empty(),
            has_experience_signal,
            has_title_signal,
            has_location_signal,
        );

        Self {
            user_weights,
            confidence,
            evidence_counts,
            max_evidence,
            mention_counts,
            max_mentions,
            mention_cap,
            experience_words,
            summary_hits,
            title_corpus,
            location_corpus,
            preferred_titles: prefs.preferred_titles.clone(),
            years,
            has_experience_signal,
            has_title_signal,
            has_location_signal,
            aliases,
            skill_labels: all_skills,
            weights,
        }
    }
}

fn renormalised_weights(
    has_skills: bool,
    has_exp: bool,
    has_title: bool,
    has_loc: bool,
) -> (f32, f32, f32, f32) {
    let mut w = (
        if has_skills { SKILLS_WEIGHT } else { 0.0 },
        if has_exp { EXPERIENCE_WEIGHT } else { 0.0 },
        if has_title { TITLE_WEIGHT } else { 0.0 },
        if has_loc { LOCATION_WEIGHT } else { 0.0 },
    );
    let sum = w.0 + w.1 + w.2 + w.3;
    if sum <= 0.0 {
        // No signal at all: fall back to the base policy split.
        return (
            SKILLS_WEIGHT,
            EXPERIENCE_WEIGHT,
            TITLE_WEIGHT,
            LOCATION_WEIGHT,
        );
    }
    w.0 /= sum;
    w.1 /= sum;
    w.2 /= sum;
    w.3 /= sum;
    w
}

/// Effective component weights derived from the resume/preferences.
/// Components with no signal are dropped and the rest renormalised to 1.0.
pub fn dynamic_weights(profile: &CandidateProfile) -> (f32, f32, f32, f32) {
    dynamic_weights_with_resume(profile, None)
}

pub fn dynamic_weights_with_resume(
    profile: &CandidateProfile,
    resume: Option<&crate::resume::CandidateProfile>,
) -> (f32, f32, f32, f32) {
    let exp_probe = resume.map(|r| experience_text(Some(r))).unwrap_or_default();
    ResumeSignals::from_parts(profile, resume, &exp_probe).weights
}

// ---------------------------------------------------------------------------
// Rank entry points
// ---------------------------------------------------------------------------

/// Builds an inverted index of resume-derived skill terms and ranks every job.
/// Index breadth adapts to the resume: the per-posting character budget grows
/// with resume length and shrinks with corpus size.
pub fn rank_jobs(profile: &CandidateProfile, jobs: &[&Job], experience: &str) -> Vec<Ranked> {
    rank_jobs_with_resume(profile, None, jobs, experience)
}

pub fn rank_jobs_with_resume(
    profile: &CandidateProfile,
    resume: Option<&crate::resume::CandidateProfile>,
    jobs: &[&Job],
    experience: &str,
) -> Vec<Ranked> {
    let signals = ResumeSignals::from_parts(profile, resume, experience);
    let phrases = resume_phrases_with_signals(profile, &signals, experience);
    let (k1, b) = bm25_params(jobs);
    let budget = index_budget(experience, jobs.len());

    let mut postings: HashMap<String, Vec<(usize, f32)>> = HashMap::new();
    let mut doc_len = Vec::with_capacity(jobs.len());
    let mut doc_terms: Vec<Vec<String>> = Vec::with_capacity(jobs.len());
    for (doc, job) in jobs.iter().enumerate() {
        let (title_tokens, body_tokens) = job_token_views(job, budget, &signals.aliases);
        doc_len.push((title_tokens.len() + body_tokens.len()).max(1) as f32);
        let mut terms = Vec::new();
        for phrase in &phrases {
            let title_hits =
                phrase_count_aliased(&title_tokens, &phrase.tokens, &signals.aliases) as f32;
            let body_hits =
                phrase_count_aliased(&body_tokens, &phrase.tokens, &signals.aliases) as f32;
            // Title is worth more; body saturates dynamically with the budget.
            let cap = (budget as f32 / 400.0).clamp(2.0, 8.0);
            let tf = title_hits * 3.0 + body_hits.min(cap);
            if tf <= 0.0 {
                continue;
            }
            terms.push(phrase.key.clone());
            postings
                .entry(phrase.key.clone())
                .or_default()
                .push((doc, tf));
        }
        doc_terms.push(terms);
    }
    let avgdl = {
        let sum = doc_len.iter().sum::<f32>();
        if jobs.is_empty() {
            1.0
        } else {
            (sum / jobs.len() as f32).max(1.0)
        }
    };
    let corpus = jobs.len() as f32;
    let mut raw = vec![0.0; jobs.len()];
    let mut matched: Vec<Vec<String>> = vec![Vec::new(); jobs.len()];
    for phrase in &phrases {
        if phrase.weight <= 0.0 {
            continue;
        }
        let Some(hits) = postings.get(&phrase.key) else {
            continue;
        };
        let df = hits.len() as f32;
        // Robertson-Spärck-Jones IDF with a resume-derived floor: rare resume
        // skills (low df) keep full weight, ubiquitous ones decay naturally.
        let idf = ((corpus - df + 0.5) / (df + 0.5) + 1.0).ln().max(0.0);
        let label = phrase.label.clone();
        let query_weight = phrase.weight;
        for (doc, tf) in hits {
            let length = doc_len[*doc].max(1.0);
            let normal = *tf * (k1 + 1.0) / (*tf + k1 * (1.0 - b + b * length / avgdl));
            raw[*doc] += idf * normal * (query_weight / 10.0);
            matched[*doc].push(label.clone());
        }
    }
    let best = raw.iter().copied().fold(0.0, f32::max);
    let (w_skill, w_exp, w_title, w_loc) = signals.weights;
    let mut ranked: Vec<Ranked> = jobs
        .iter()
        .enumerate()
        .map(|(position, job)| {
            let mut breakdown = listing_score_with_signals(profile, job, &phrases, &signals);
            if !breakdown.excluded {
                let skill = if best <= 0.0 {
                    0.0
                } else {
                    raw[position] / best * 100.0
                };
                breakdown.skill = skill;
                breakdown.total = skill * w_skill
                    + breakdown.experience * w_exp
                    + breakdown.title * w_title
                    + breakdown.location * w_loc;
            }
            let matched_labels: HashSet<String> =
                matched[position].iter().cloned().collect();
            // A term present in the posting is matched, never missing — this
            // keeps the two tag rows disjoint on the card.
            let missing = doc_terms[position]
                .iter()
                .filter(|term| {
                    phrases
                        .iter()
                        .find(|phrase| phrase.key == **term)
                        .map(|phrase| phrase.weight)
                        .unwrap_or(0.0)
                        < 2.0
                })
                .take(12)
                .map(|term| {
                    phrases
                        .iter()
                        .find(|phrase| phrase.key == *term)
                        .map(|phrase| phrase.label.clone())
                        .unwrap_or_else(|| display_term(term))
                })
                .filter(|label| !matched_labels.contains(label))
                .take(6)
                .collect();
            Ranked {
                position,
                rank: 0,
                breakdown,
                matched: matched[position].clone(),
                missing,
            }
        })
        .collect();
    ranked.sort_by(|left, right| {
        right
            .breakdown
            .total
            .partial_cmp(&left.breakdown.total)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (offset, item) in ranked.iter_mut().enumerate() {
        item.rank = (offset + 1) as u32;
    }
    ranked
}

struct Phrase {
    key: String,
    label: String,
    tokens: Vec<String>,
    weight: f32,
}

/// Every scored term comes from the resume: the skill list, saved weights, the summary, and experience.
fn resume_phrases(profile: &CandidateProfile, experience: &str) -> Vec<Phrase> {
    let signals = ResumeSignals::from_parts(profile, None, experience);
    resume_phrases_with_signals(profile, &signals, experience)
}

fn resume_phrases_with_signals(
    profile: &CandidateProfile,
    signals: &ResumeSignals,
    experience: &str,
) -> Vec<Phrase> {
    let strengths = skill_strengths_with_signals(profile, signals, experience);
    let mut seen = HashSet::new();
    let mut phrases = Vec::new();
    for skill in signals
        .skill_labels
        .iter()
        .chain(profile.skill_weights.iter().map(|weight| &weight.skill))
    {
        let toks = tokens(skill);
        if toks.is_empty() {
            continue;
        }
        let key = toks.join(" ");
        if !seen.insert(key.clone()) {
            continue;
        }
        phrases.push(Phrase {
            weight: strengths.get(&key).copied().unwrap_or(0.0),
            key,
            label: skill.clone(),
            tokens: toks,
        });
    }
    phrases
}

fn job_token_views(
    job: &Job,
    budget: usize,
    aliases: &HashMap<String, String>,
) -> (Vec<String>, Vec<String>) {
    let title = tokens_aliased(&job.title, aliases);
    let mut body = tokens_aliased(text_prefix(&job.description, budget), aliases);
    if !job.skills.is_empty() {
        body.extend(tokens_aliased(&job.skills.join(" "), aliases));
    }
    (title, body)
}

/// Per-posting index budget derived from resume length and corpus size.
/// Longer resumes justify deeper indexing; large corpora shrink it to stay fast.
fn index_budget(experience: &str, job_count: usize) -> usize {
    let resume_chars = experience.chars().count().max(200);
    let per_job = if job_count == 0 {
        resume_chars * 2
    } else {
        (resume_chars * 4).max(12_000 / job_count.max(1))
    };
    per_job.clamp(500, 4_000)
}

/// BM25 saturation/length-norm derived from the posting index itself.
fn bm25_params(jobs: &[&Job]) -> (f32, f32) {
    if jobs.is_empty() {
        return (1.2, 0.75);
    }
    let avg_len: f32 = jobs
        .iter()
        .map(|j| j.description.len().max(50) as f32)
        .sum::<f32>()
        / jobs.len() as f32;
    // Longer corpora saturate slower; tiny corpora normalise length less.
    let k1 = (0.9 + avg_len / 2500.0).clamp(0.8, 1.6);
    let b = if jobs.len() >= 10 {
        0.75
    } else {
        (0.45 + 0.03 * jobs.len() as f32).clamp(0.45, 0.75)
    };
    (k1, b)
}

fn phrase_count(haystack: &[String], phrase: &[String]) -> usize {
    if phrase.is_empty() || haystack.len() < phrase.len() {
        return 0;
    }
    haystack
        .windows(phrase.len())
        .filter(|window| tokens_equal(window, phrase))
        .count()
}

fn phrase_count_aliased(
    haystack: &[String],
    phrase: &[String],
    aliases: &HashMap<String, String>,
) -> usize {
    if phrase.is_empty() || haystack.len() < phrase.len() {
        return 0;
    }
    // Try the full phrase, then the resume-derived core alias
    // (e.g. ["node"] for skill ["node","js"]).
    let mut total = phrase_count(haystack, phrase);
    if total == 0 && phrase.len() == 2 && phrase[1].len() <= 3 {
        total = phrase_count(haystack, &phrase[..1]);
    }
    if total == 0 {
        let resolved: Vec<String> = phrase.iter().map(|t| resolve_alias(t, aliases)).collect();
        if resolved != phrase {
            total = phrase_count(haystack, &resolved);
        }
    }
    total
}

/// Generic token equality: exact, plus a resume-independent stemming rule.
/// A long token matches its prefix when the shared stem is >= 5 chars
/// (covers postgres/postgresql, container/containers, ...). Short tokens
/// (java/js, go, c) never fuzzy-match: they must be exact.
fn tokens_equal(window: &[String], phrase: &[String]) -> bool {
    if window.len() != phrase.len() {
        return false;
    }
    window
        .iter()
        .zip(phrase.iter())
        .all(|(a, b)| token_match(a, b))
}

fn token_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // Never fuzzy-match short tokens: "java" != "javascript", "go" stays exact.
    if a.len() < 5 || b.len() < 5 {
        return false;
    }
    let stem = a.len().min(b.len());
    if stem < 6 {
        return false;
    }
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    long.starts_with(short)
}

fn text_prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn listing_score(profile: &CandidateProfile, job: &Job, phrases: &[Phrase]) -> Breakdown {
    let signals = ResumeSignals::from_parts(profile, None, "");
    listing_score_with_signals(profile, job, phrases, &signals)
}

fn listing_score_with_signals(
    profile: &CandidateProfile,
    job: &Job,
    phrases: &[Phrase],
    signals: &ResumeSignals,
) -> Breakdown {
    let budget = 800 + signals.experience_words.min(1200);
    let excluded = profile.excluded_keywords.iter().any(|keyword| {
        let keyword = keyword.trim().to_lowercase();
        !keyword.is_empty()
            && (job.title.to_lowercase().contains(&keyword)
                || text_prefix(&job.description, budget)
                    .to_lowercase()
                    .contains(&keyword))
    });
    if excluded {
        return Breakdown {
            total: 0.0,
            skill: 0.0,
            experience: 0.0,
            title: 0.0,
            location: 0.0,
            excluded: true,
        };
    }
    let title = title_score_with_signals(profile, job, phrases, signals);
    let location = location_score_with_signals(job, signals);
    let experience = experience_score_with_signals(job, signals);
    Breakdown {
        total: 0.0,
        skill: 0.0,
        experience,
        title,
        location,
        excluded: false,
    }
}

pub fn experience_text(resume: Option<&crate::resume::CandidateProfile>) -> String {
    let Some(resume) = resume else {
        return String::new();
    };
    let mut text = resume.summary.clone().unwrap_or_default();
    text.push(' ');
    text.push_str(&resume.headline.clone().unwrap_or_default());
    for role in &resume.experience {
        text.push(' ');
        text.push_str(&role.title);
        text.push(' ');
        text.push_str(&role.company);
        if let Some(loc) = role.location.as_deref() {
            text.push(' ');
            text.push_str(loc);
        }
        for bullet in &role.bullets {
            text.push(' ');
            text.push_str(&bullet.text);
        }
    }
    for project in &resume.projects {
        text.push(' ');
        text.push_str(&project.name);
        text.push(' ');
        text.push_str(&project.description);
        for tech in &project.technologies {
            text.push(' ');
            text.push_str(tech);
        }
    }
    for edu in &resume.education {
        text.push(' ');
        text.push_str(&edu.qualification);
        text.push(' ');
        text.push_str(&edu.institution);
    }
    for skill in &resume.skills {
        // Skill evidence phrases carry the resume's own vocabulary.
        for ev in &skill.evidence {
            text.push(' ');
            text.push_str(ev);
        }
    }
    text
}

/// Estimate total years from resume date ranges like "03/2022 – 06/2025" or
/// "12/2025 – Present". Overlapping roles are summed then capped at the span
/// between earliest start and now, so double-counted parallel jobs can't
/// inflate the total beyond the calendar span.
pub fn estimated_years(resume: &crate::resume::CandidateProfile) -> f32 {
    let mut spans: Vec<(i32, i32)> = Vec::new();
    for role in &resume.experience {
        if let Some((s, e)) = parse_date_range(role.dates.as_deref().unwrap_or("")) {
            spans.push((s, e));
        }
    }
    if spans.is_empty() {
        return 0.0;
    }
    spans.sort();
    // Merge overlaps for the calendar span cap.
    let mut merged = vec![spans[0]];
    for (s, e) in spans.iter().skip(1) {
        let last = merged.last_mut().expect("non-empty");
        if *s <= last.1 {
            last.1 = last.1.max(*e);
        } else {
            merged.push((*s, *e));
        }
    }
    let covered: i32 = merged.iter().map(|(s, e)| (e - s).max(0)).sum();
    (covered as f32 / 12.0).clamp(0.0, 50.0)
}

fn parse_date_range(dates: &str) -> Option<(i32, i32)> {
    // Accepts "MM/YYYY – MM/YYYY", "MM/YYYY - Present", "YYYY – YYYY", etc.
    let norm = dates.to_lowercase().replace('–', "-").replace('—', "-");
    let parts: Vec<&str> = norm.split('-').map(str::trim).collect();
    if parts.len() != 2 {
        return None;
    }
    let start = parse_month(parts[0])?;
    let end = if parts[1].contains("present") || parts[1].contains("now") || parts[1].is_empty() {
        let now = chrono::Utc::now();
        (now.format("%Y").to_string().parse::<i32>().unwrap_or(2026) * 12)
            + (now.format("%m").to_string().parse::<i32>().unwrap_or(1) - 1)
    } else {
        parse_month(parts[1])?
    };
    if end >= start {
        Some((start, end))
    } else {
        None
    }
}

fn parse_month(part: &str) -> Option<i32> {
    let re = month_re();
    let caps = re.captures(part.trim())?;
    let year: i32 = caps.get(2)?.as_str().parse().ok()?;
    let month: i32 = caps
        .get(1)
        .map(|m| m.as_str().parse().unwrap_or(1))
        .unwrap_or(1);
    if !(1..=12).contains(&month) || !(1970..=2100).contains(&year) {
        return None;
    }
    Some(year * 12 + (month - 1))
}

fn month_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(\d{1,2})?\s*/?\s*(\d{4})").expect("month pattern"))
}

pub fn deterministic(profile: &CandidateProfile, job: &Job, experience: &str) -> Breakdown {
    deterministic_with_resume(profile, None, job, experience)
}

pub fn deterministic_with_resume(
    profile: &CandidateProfile,
    resume: Option<&crate::resume::CandidateProfile>,
    job: &Job,
    experience: &str,
) -> Breakdown {
    let text = format!("{} {} {}", job.title, job.description, job.skills.join(" "));
    let excluded = profile
        .excluded_keywords
        .iter()
        .any(|keyword| !keyword.is_empty() && has_term(&text, keyword));
    if excluded {
        return Breakdown {
            total: 0.,
            skill: 0.,
            experience: 0.,
            title: 0.,
            location: 0.,
            excluded: true,
        };
    }
    let signals = ResumeSignals::from_parts(profile, resume, experience);
    let phrases = resume_phrases_with_signals(profile, &signals, experience);
    let skills = skill_score(&phrases, job, &signals);
    let title = title_score_with_signals(profile, job, &phrases, &signals);
    let location = location_score_with_signals(job, &signals);
    let exp = experience_score_with_signals(job, &signals);
    let (w_skill, w_exp, w_title, w_loc) = signals.weights;
    Breakdown {
        total: skills * w_skill + exp * w_exp + title * w_title + location * w_loc,
        skill: skills,
        experience: exp,
        title,
        location,
        excluded: false,
    }
}

pub fn skill_overlap(
    profile: &CandidateProfile,
    job: &Job,
    experience: &str,
) -> (Vec<String>, Vec<String>) {
    skill_overlap_with_resume(profile, None, job, experience)
}

pub fn skill_overlap_with_resume(
    profile: &CandidateProfile,
    resume: Option<&crate::resume::CandidateProfile>,
    job: &Job,
    experience: &str,
) -> (Vec<String>, Vec<String>) {
    let signals = ResumeSignals::from_parts(profile, resume, experience);
    let phrases = resume_phrases_with_signals(profile, &signals, experience);
    let budget = index_budget(experience, 1);
    let (title_tokens, body_tokens) = job_token_views(job, budget, &signals.aliases);
    let mut matched: Vec<(f32, String)> = phrases
        .iter()
        .filter(|phrase| {
            phrase.weight > 0.0
                && (phrase_count_aliased(&title_tokens, &phrase.tokens, &signals.aliases) > 0
                    || phrase_count_aliased(&body_tokens, &phrase.tokens, &signals.aliases) > 0)
        })
        .map(|phrase| (phrase.weight, phrase.label.clone()))
        .collect();
    matched.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let missing = phrases
        .iter()
        .filter(|phrase| {
            phrase.weight < 2.0
                && (phrase_count_aliased(&title_tokens, &phrase.tokens, &signals.aliases) > 0
                    || phrase_count_aliased(&body_tokens, &phrase.tokens, &signals.aliases) > 0)
        })
        .map(|phrase| phrase.label.clone())
        .collect();
    (
        matched.into_iter().map(|(_, skill)| skill).collect(),
        missing,
    )
}

/// Listed skills start from a resume-derived base. Summary and experience
/// evidence scale dynamically: the per-mention value and summary bonus adapt
/// to resume length and skill diversity instead of fixed 4.0 / 3.0 constants.
/// Explicit `skill_weights` (0-10, user-edited) always override the inference.
fn skill_strengths(profile: &CandidateProfile, experience: &str) -> HashMap<String, f32> {
    let signals = ResumeSignals::from_parts(profile, None, experience);
    skill_strengths_with_signals(profile, &signals, experience)
}

fn skill_strengths_with_signals(
    profile: &CandidateProfile,
    signals: &ResumeSignals,
    experience: &str,
) -> HashMap<String, f32> {
    let mut strengths = HashMap::new();
    let exp_toks = tokens(experience);
    // Dynamic mention value: the total mention budget (~8 points) is spread
    // over the saturation cap, so short resumes saturate fast and long ones
    // need sustained evidence.
    let per_mention = 8.0 / (signals.mention_cap as f32 + 1.0);
    // Dynamic summary bonus: rarer summary mentions are worth more.
    // bonus = total_skills / max(1, skills_in_summary), clamped 1..5.
    let total = profile.skills.len().max(1) as f32;
    let in_summary = signals.summary_hits.len().max(1) as f32;
    let summary_bonus = (total / in_summary).clamp(1.0, 5.0);

    for skill in &signals.skill_labels {
        let toks = tokens(skill);
        if toks.is_empty() {
            continue;
        }
        let key = toks.join(" ");
        let mentions = phrase_count(&exp_toks, &toks).min(signals.mention_cap) as f32;
        // Confidence from the parsed resume, when this skill was extracted
        // with evidence; defaults to 0.7 for preference-only skills.
        let conf = signals.confidence.get(&key).copied().unwrap_or(0.7);
        let evidence_norm = signals
            .evidence_counts
            .get(&key)
            .map(|n| ((*n as f32 + 1.0).ln() / (signals.max_evidence as f32 + 1.0).ln().max(1.0)))
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let summary_hit = if signals.summary_hits.contains(&key) {
            summary_bonus
        } else {
            0.0
        };
        // Base 1.0 (listed) + resume-derived evidence, scaled by confidence.
        let inferred =
            (1.0 + summary_hit + mentions * per_mention + evidence_norm * 2.0) * (0.6 + 0.4 * conf);
        let chosen = user_weight_from(profile, &key).unwrap_or(inferred);
        strengths.insert(key, chosen);
    }
    for (key, w) in &signals.user_weights {
        strengths.insert(key.clone(), w.clamp(0.0, 10.0));
    }
    strengths
}

fn user_weight(profile: &CandidateProfile, key: &str) -> Option<f32> {
    user_weight_from(profile, key)
}

fn user_weight_from(profile: &CandidateProfile, key: &str) -> Option<f32> {
    profile
        .skill_weights
        .iter()
        .find(|weight| canonical_key(&tokens(&weight.skill)) == key)
        .map(|weight| weight.weight.clamp(0.0, 10.0))
}

/// Scales the summary and experience estimate onto 0–10 so you can edit it.
/// Normalisation uses this resume's own strongest signal (no fixed scale).
pub fn suggested_skill_weights(
    profile: &CandidateProfile,
    experience: &str,
) -> Vec<crate::models::SkillWeight> {
    suggested_skill_weights_with_resume(profile, None, experience)
}

pub fn suggested_skill_weights_with_resume(
    profile: &CandidateProfile,
    resume: Option<&crate::resume::CandidateProfile>,
    experience: &str,
) -> Vec<crate::models::SkillWeight> {
    let signals = ResumeSignals::from_parts(profile, resume, experience);
    let mut plain = profile.clone();
    plain.skill_weights.clear();
    let plain_signals = ResumeSignals::from_parts(&plain, resume, experience);
    let _ = signals;
    let inferred = skill_strengths_with_signals(&plain, &plain_signals, experience);
    let strongest = inferred.values().copied().fold(1e-6, f32::max);
    signals
        .skill_labels
        .iter()
        .map(|skill| {
            let raw = inferred
                .get(&canonical_key(&tokens(skill)))
                .copied()
                .unwrap_or(1.0);
            crate::models::SkillWeight {
                skill: skill.clone(),
                weight: (raw / strongest * 10.0).round().clamp(0.0, 10.0),
            }
        })
        .collect()
}

fn skill_score(phrases: &[Phrase], job: &Job, signals: &ResumeSignals) -> f32 {
    let budget = 2000;
    let (title_tokens, body_tokens) = job_token_views(job, budget, &signals.aliases);
    let strongest = phrases
        .iter()
        .map(|phrase| phrase.weight)
        .fold(1e-6, f32::max);
    let mut weighted = 0.0;
    let mut possible = 0.0;
    for phrase in phrases {
        let hits = phrase_count_aliased(&title_tokens, &phrase.tokens, &signals.aliases) as f32
            * 4.0
            + (phrase_count_aliased(&body_tokens, &phrase.tokens, &signals.aliases) as f32)
                .min(2.0);
        if hits <= 0.0 {
            continue;
        }
        weighted += hits * phrase.weight;
        possible += hits * strongest;
    }
    if possible == 0.0 {
        0.0
    } else {
        (weighted / possible * 100.0).clamp(0.0, 100.0)
    }
}

/// Continuous title relevance from the resume's own title vocabulary.
/// No fixed 0.45 cliff: coverage maps linearly onto a band that depends on
/// whether a preferred title matches as a substring.
fn title_score(profile: &CandidateProfile, job: &Job, phrases: &[Phrase]) -> f32 {
    let signals = ResumeSignals::from_parts(profile, None, "");
    title_score_with_signals(profile, job, phrases, &signals)
}

fn title_score_with_signals(
    profile: &CandidateProfile,
    job: &Job,
    phrases: &[Phrase],
    signals: &ResumeSignals,
) -> f32 {
    if !signals.has_title_signal {
        return 50.0;
    }
    let title_matches = signals.preferred_titles.iter().any(|title| {
        !title.trim().is_empty() && job.title.to_lowercase().contains(&title.to_lowercase())
    });
    // Resume headline / past titles broaden the match beyond preferences.
    let corpus_hit = if title_matches {
        true
    } else {
        signals.title_corpus.iter().any(|t| {
            let tt = tokens(t);
            let jt = tokens(&job.title);
            !tt.is_empty() && tt.iter().any(|tok| tok.len() > 3 && jt.contains(tok))
        })
    };
    if !title_matches && !corpus_hit {
        return 25.0;
    }
    let title_tokens = tokens_aliased(&job.title, &signals.aliases);
    let strongest = phrases
        .iter()
        .map(|phrase| phrase.weight)
        .fold(1e-6, f32::max);
    let mut hit = 0.0;
    let mut seen = 0.0;
    for phrase in phrases {
        if phrase_count_aliased(&title_tokens, &phrase.tokens, &signals.aliases) == 0 {
            continue;
        }
        hit += phrase.weight;
        seen += strongest;
    }
    if seen == 0.0 {
        // Title matches the role family but names none of the resume skills:
        // mildly positive, scaled by how focused the resume titles are.
        return if title_matches { 70.0 } else { 55.0 };
    }
    let coverage = (hit / seen).clamp(0.0, 1.0);
    if title_matches {
        // 40 (role fit) + up to 60 for resume-skill coverage of the title.
        40.0 + 60.0 * coverage
    } else {
        // Resume-corpus hit only: 30 + up to 40.
        30.0 + 40.0 * coverage
    }
}

/// Location score from the resume's own geography: preferred places first,
/// resume experience locations as fallback. Neutral 50 only when the resume
/// carries no location signal at all.
fn location_score_with_signals(job: &Job, signals: &ResumeSignals) -> f32 {
    if !signals.has_location_signal {
        return 50.0;
    }
    let loc = job.location.as_deref().unwrap_or("").to_lowercase();
    if loc.trim().is_empty() {
        return 50.0;
    }
    // Exact/remote handling is resume-derived: "remote" counts when the
    // resume mentions remote anywhere in its location corpus.
    let wants_remote = signals
        .location_corpus
        .iter()
        .any(|l| l.to_lowercase().contains("remote"));
    if wants_remote && loc.contains("remote") {
        return 100.0;
    }
    if signals
        .location_corpus
        .iter()
        .any(|place| !place.trim().is_empty() && loc.contains(&place.to_lowercase()))
    {
        return 100.0;
    }
    // Partial token overlap gives partial credit instead of a flat 20.
    let job_toks: HashSet<String> = tokens(&loc).into_iter().collect();
    let mut best: f32 = 0.0;
    for place in &signals.location_corpus {
        let pt: HashSet<String> = tokens(place).into_iter().collect();
        if pt.is_empty() || job_toks.is_empty() {
            continue;
        }
        let inter = pt.intersection(&job_toks).count() as f32;
        let union = pt.union(&job_toks).count() as f32;
        best = best.max(inter / union.max(1.0));
    }
    if best > 0.0 {
        20.0 + 80.0 * best
    } else {
        20.0
    }
}

/// Experience fit with resume-derived tolerance. Inside the band: 100.
/// Below the minimum, a smooth exponential penalty with tau scaled to the
/// band (tau = max(1.0, min*0.25)); above the maximum, a gentle
/// over-qualification discount. No fixed 0/65/75 cliffs.
fn experience_score_with_signals(job: &Job, signals: &ResumeSignals) -> f32 {
    let years = signals.years;
    if !signals.has_experience_signal {
        return 70.0;
    }
    match (job.min_experience, job.max_experience) {
        (Some(min), Some(max)) if years >= min && years <= max => 100.0,
        (Some(min), _) if years < min => {
            let tau = (min * 0.25).max(1.0);
            100.0 * (-(min - years) / tau).exp()
        }
        (Some(_), Some(max)) => {
            // Over-qualified: mild, resume-scaled discount.
            let tau = ((max * 0.3).max(1.5)).max(1.0);
            (95.0 - 10.0 * (years - max) / tau).clamp(50.0, 95.0)
        }
        (Some(min), None) => {
            if years >= min {
                let tau = (min * 0.25).max(1.0);
                80.0 + 20.0 * (-(years - min) / tau).exp()
            } else {
                let tau = (min * 0.25).max(1.0);
                100.0 * (-(min - years) / tau).exp()
            }
        }
        _ => 70.0,
    }
}

fn canon(term: &str) -> String {
    tokens(term).join(" ")
}

fn display_term(term: &str) -> String {
    // No fixed pretty-name table: fall back to title-casing the key.
    let mut chars = term.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn has_term(text: &str, term: &str) -> bool {
    let wanted = canon(term);
    !wanted.is_empty() && tokens(text).iter().any(|token| token == &wanted)
}

fn count_term(text: &str, term: &str) -> usize {
    let wanted = canon(term);
    if wanted.is_empty() {
        0
    } else {
        tokens(text)
            .iter()
            .filter(|token| *token == &wanted)
            .count()
    }
}

fn token_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-z0-9#+]+").expect("token pattern"))
}

/// Generic tokenizer. No technology-specific hard codes: lowercase, keep
/// `+`/`#` (C++/C#), split on everything else (dots, slashes, dashes).
/// Plural stemming is resume-aware (handled by `token_match`), not a list.
fn tokens(text: &str) -> Vec<String> {
    let folded = text.to_lowercase();
    token_pattern()
        .find_iter(&folded)
        .map(|item| item.as_str().to_string())
        .collect()
}

fn tokens_aliased(text: &str, aliases: &HashMap<String, String>) -> Vec<String> {
    let base = tokens(text);
    if aliases.is_empty() {
        return base;
    }
    base.into_iter()
        .map(|t| resolve_alias(&t, aliases))
        // Resolved aliases may be multi-word ("node js"); split them back.
        .flat_map(|s| {
            if s.contains(' ') {
                s.split_whitespace().map(str::to_string).collect::<Vec<_>>()
            } else {
                vec![s]
            }
        })
        .collect()
}

pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        None
    } else {
        Some(
            a.iter()
                .zip(b)
                .map(|(x, y)| x * y)
                .sum::<f32>()
                .clamp(-1., 1.),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    fn profile() -> CandidateProfile {
        CandidateProfile {
            name: "Test".into(),
            summary: "Backend".into(),
            skills: vec!["Rust".into(), "Go".into()],
            experience_years: 4.0,
            preferred_titles: vec!["Backend Engineer".into()],
            role_keywords: vec![],
            preferred_locations: vec!["Bengaluru".into()],
            excluded_keywords: vec!["unpaid".into()],
            minimum_match_score: 65.0,
            projects: vec![],
            preferred_work_modes: vec![],
            auto_apply: false,
            email: String::new(),
            phone: String::new(),
            skill_weights: vec![],
        }
    }
    fn job(desc: &str) -> Job {
        Job {
            external_id: "1".into(),
            source: "test".into(),
            title: "Senior Backend Engineer".into(),
            company: "Acme".into(),
            location: Some("Bengaluru".into()),
            description: desc.into(),
            skills: vec!["Rust".into(), "Go".into()],
            min_experience: Some(3.0),
            max_experience: Some(6.0),
            url: "https://example.test".into(),
            discovered_at: Utc::now(),
            published_at: None,
            work_mode: None,
        }
    }
    #[test]
    fn strong_match_is_high() {
        assert!(
            deterministic(&profile(), &job("Build Rust and Go backend services."), "").total > 90.0
        )
    }
    #[test]
    fn excluded_keyword_rejects() {
        let b = deterministic(&profile(), &job("Unpaid role"), "");
        assert!(b.excluded);
        assert_eq!(b.total, 0.0)
    }
    #[test]
    fn weights_sum_to_one() {
        assert!(
            (SKILLS_WEIGHT + EXPERIENCE_WEIGHT + TITLE_WEIGHT + LOCATION_WEIGHT - 1.0).abs()
                < f32::EPSILON
        );
    }
    #[test]
    fn score_uses_requested_weighting() {
        let b = deterministic(&profile(), &job("Build Rust and Go backend services."), "");
        let expected = b.skill * 0.35 + b.experience * 0.20 + b.title * 0.25 + b.location * 0.20;
        assert!((b.total - expected).abs() < 0.0001);
    }

    #[test]
    fn node_title_loses_to_summary_and_experience() {
        let mut candidate = profile();
        candidate.summary = "Backend engineer. Strongest in Rust and Go.".into();
        candidate
            .skills
            .extend(["Java".into(), "TypeScript".into(), "Docker".into()]);
        candidate.preferred_titles = vec!["Senior Software Engineer".into()];
        candidate.experience_years = 4.5;
        let node = Job {
            title: "Senior Software Engineer (React / Node.js / Java)".into(),
            description: "Build Node.js and React services. JavaScript experience required.".into(),
            skills: vec!["Node.js".into(), "React".into()],
            ..job("")
        };
        let rust = Job {
            title: "Senior Software Engineer, Rust".into(),
            description: "Build Rust services.".into(),
            skills: vec!["Rust".into()],
            ..job("")
        };
        let experience = "Built Rust services and Go APIs in production.";
        let node_score = deterministic(&candidate, &node, experience);
        let rust_score = deterministic(&candidate, &rust, experience);
        assert!(node_score.total < candidate.minimum_match_score);
        assert!(rust_score.total > node_score.total);
        assert!(rust_score.total >= candidate.minimum_match_score);
    }

    #[test]
    fn saved_weightage_overrides_a_passing_mention() {
        let mut candidate = profile();
        candidate.summary = "Worked with Rust, Go, and Node.js.".into();
        candidate.skills.push("Node.js".into());
        candidate.skill_weights = vec![
            crate::models::SkillWeight {
                skill: "Rust".into(),
                weight: 10.0,
            },
            crate::models::SkillWeight {
                skill: "Go".into(),
                weight: 8.0,
            },
            crate::models::SkillWeight {
                skill: "Node.js".into(),
                weight: 0.0,
            },
        ];
        candidate.preferred_titles = vec!["Senior Software Engineer".into()];
        let node = Job {
            title: "Senior Software Engineer, Node.js".into(),
            description: "Node.js services.".into(),
            skills: vec!["Node.js".into()],
            ..job("")
        };
        let score = deterministic(&candidate, &node, "Built Node.js services for years.");
        assert!(score.total < candidate.minimum_match_score);
    }

    #[test]
    fn index_ranks_the_stronger_skill_first() {
        let mut candidate = profile();
        candidate.summary = "Backend engineer. Strongest in Rust and Go.".into();
        candidate.skills.extend(["Java".into(), "Node.js".into()]);
        candidate.skill_weights = vec![
            crate::models::SkillWeight {
                skill: "Rust".into(),
                weight: 10.0,
            },
            crate::models::SkillWeight {
                skill: "Go".into(),
                weight: 8.0,
            },
            crate::models::SkillWeight {
                skill: "Node.js".into(),
                weight: 0.0,
            },
        ];
        candidate.preferred_titles = vec!["Senior Software Engineer".into()];
        let jobs = vec![
            Job {
                title: "Senior Software Engineer, Node.js".into(),
                description: "Node.js and React services in Bengaluru.".into(),
                skills: vec!["Node.js".into()],
                ..job("")
            },
            Job {
                title: "Senior Software Engineer, Rust".into(),
                description: "Build Rust services in Bengaluru.".into(),
                skills: vec!["Rust".into()],
                ..job("")
            },
        ];
        let refs: Vec<&Job> = jobs.iter().collect();
        let ranked = rank_jobs(&candidate, &refs, "Built Rust services and Go APIs.");
        assert_eq!(ranked[0].rank, 1);
        assert_eq!(ranked[0].position, 1);
        assert!(ranked[0].breakdown.total > ranked[1].breakdown.total);
        assert!(ranked[1].breakdown.skill < 1.0);
    }

    #[test]
    fn a_resume_skill_is_ranked_without_a_fixed_technology_list() {
        let mut candidate = profile();
        candidate.summary = "Data engineer working with ClickHouse.".into();
        candidate.skills = vec!["ClickHouse".into(), "DuckDB".into()];
        candidate.preferred_titles = vec!["Senior Software Engineer".into()];
        let jobs = vec![
            Job {
                title: "Senior Software Engineer, Node.js".into(),
                description: "Node.js services.".into(),
                skills: vec!["Node.js".into()],
                ..job("")
            },
            Job {
                title: "Senior Software Engineer, ClickHouse".into(),
                description: "Build ClickHouse pipelines.".into(),
                skills: vec!["ClickHouse".into()],
                ..job("")
            },
        ];
        let refs: Vec<&Job> = jobs.iter().collect();
        let ranked = rank_jobs(&candidate, &refs, "Maintained ClickHouse tables.");
        assert_eq!(ranked[0].position, 1);
        assert!(ranked[0].matched.iter().any(|skill| skill == "ClickHouse"));
    }

    #[test]
    fn ranking_uses_skills_extracted_from_the_resume_file() {
        let mut candidate = profile();
        candidate.skills = vec!["Rust".into()];
        let resume = crate::resume::CandidateProfile {
            name: Some("Gowtham".into()),
            headline: Some("Backend engineer".into()),
            summary: Some("Built ClickHouse pipelines.".into()),
            skills: vec![crate::resume::SkillEvidence {
                name: "ClickHouse".into(),
                evidence: vec!["Built ClickHouse pipelines.".into()],
                confidence: 0.9,
            }],
            experience: vec![],
            projects: vec![],
            education: vec![],
            open_source: vec![], layout: vec![], custom_fields: vec![],
            source_file: None,
            extraction_warnings: vec![],
        };
        let jobs = vec![
            Job {
                title: "Senior Software Engineer, Node.js".into(),
                description: "Node.js services.".into(),
                skills: vec!["Node.js".into()],
                ..job("")
            },
            Job {
                title: "Senior Software Engineer, ClickHouse".into(),
                description: "Operate ClickHouse.".into(),
                skills: vec!["ClickHouse".into()],
                ..job("")
            },
        ];
        let refs: Vec<&Job> = jobs.iter().collect();
        let ranked = rank_jobs_with_resume(
            &candidate,
            Some(&resume),
            &refs,
            "Built ClickHouse pipelines.",
        );
        assert_eq!(ranked[0].position, 1);
        assert!(ranked[0].matched.iter().any(|skill| skill == "ClickHouse"));
    }

    #[test]
    fn weights_adapt_when_a_component_has_no_signal() {
        let mut candidate = profile();
        candidate.preferred_locations.clear();
        candidate.preferred_titles.clear();
        let (ws, we, wt, wl) = dynamic_weights(&candidate);
        assert!((ws + we + wt + wl - 1.0).abs() < 0.0001);
        assert_eq!(wt, 0.0);
        assert_eq!(wl, 0.0);
        assert!(ws > SKILLS_WEIGHT);
    }

    #[test]
    fn resume_alias_links_short_and_long_forms() {
        let mut candidate = profile();
        candidate.skills = vec!["Node.js".into(), "PostgreSQL".into()];
        candidate.summary = "Backend with Node.js and PostgreSQL.".into();
        let signals = ResumeSignals::from_parts(&candidate, None, "Node and Postgres work.");
        // Core alias derived from the skill string itself, not a fixed list.
        assert!(signals.aliases.get("node").is_some());
        // Prefix rule links postgres/postgresql without a hard-coded map.
        assert!(token_match("postgresql", "postgres"));
        assert!(!token_match("java", "javascript"));
        assert!(!token_match("go", "golang"));
    }
}
