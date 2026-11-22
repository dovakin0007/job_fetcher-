//! Inverted-index keyword search over stored postings.
//!
//! The dashboard search box and per-card "Similar" buttons run on this
//! instead of ad-hoc regex/substring scans: each posting is tokenised once
//! (title x3, skills x2, description x1), a document-frequency table is
//! built, and queries score with BM25. Rebuilt per request from SQLite so it
//! is never stale; 3-4k postings tokenise in milliseconds.

use std::collections::{HashMap, HashSet};

const K1: f32 = 1.2;
const B: f32 = 0.75;
const TITLE_BOOST: f32 = 3.0;
const SKILL_BOOST: f32 = 2.0;

/// Split on anything that is not a letter, digit, `+` or `#`
/// (keeps `c++`, `c#`, `node.js` -> `node` + `js`). Drops English
/// stopwords so `and`/`with`/`for` never outrank real stack terms.
pub fn lex(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() || ch == '+' || ch == '#' {
            current.push(ch);
        } else if !current.is_empty() {
            push_token(&mut out, &mut current);
        }
    }
    if !current.is_empty() {
        push_token(&mut out, &mut current);
    }
    out
}

fn push_token(out: &mut Vec<String>, current: &mut String) {
    let token = std::mem::take(current);
    if !is_stopword(&token) {
        out.push(token);
    }
}

fn is_stopword(token: &str) -> bool {
    matches!(
        token,
        "a" | "an" | "the"
            | "and" | "or" | "with" | "for" | "of" | "in" | "on" | "at" | "to"
            | "from" | "into" | "as" | "is" | "are" | "be" | "by" | "we" | "you"
            | "your" | "our" | "their" | "this" | "that" | "these" | "will"
            | "all" | "per" | "via" | "s" | "t" | "etc"
    )
}

struct Doc {
    external_id: String,
    len: f32,
    tf: HashMap<String, f32>,
}

pub struct PostingIndex {
    docs: Vec<Doc>,
    df: HashMap<String, usize>,
    avgdl: f32,
}

impl PostingIndex {
    pub fn build(jobs: &[crate::models::Job]) -> Self {
        let mut docs = Vec::with_capacity(jobs.len());
        let mut df: HashMap<String, usize> = HashMap::new();
        let mut total_len = 0.0f32;
        for job in jobs {
            let mut tf: HashMap<String, f32> = HashMap::new();
            for token in lex(&job.title) {
                *tf.entry(token).or_insert(0.0) += TITLE_BOOST;
            }
            for token in job.skills.iter().flat_map(|skill| lex(skill)) {
                *tf.entry(token).or_insert(0.0) += SKILL_BOOST;
            }
            for token in lex(&job.description) {
                *tf.entry(token).or_insert(0.0) += 1.0;
            }
            for token in lex(&job.company) {
                *tf.entry(token).or_insert(0.0) += 1.0;
            }
            let len: f32 = tf.values().sum();
            total_len += len;
            for token in tf.keys() {
                *df.entry(token.clone()).or_insert(0) += 1;
            }
            docs.push(Doc {
                external_id: job.external_id.clone(),
                len,
                tf,
            });
        }
        let avgdl = if docs.is_empty() {
            1.0
        } else {
            (total_len / docs.len() as f32).max(1.0)
        };
        Self { docs, df, avgdl }
    }

    fn idf(&self, term: &str) -> Option<f32> {
        let df = *self.df.get(term)? as f32;
        let corpus = self.docs.len() as f32;
        Some((((corpus - df + 0.5) / (df + 0.5)) + 1.0).ln())
    }

    fn score_terms(&self, terms: &[String], limit: usize) -> Vec<(usize, f32)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for term in terms {
            *counts.entry(term.as_str()).or_insert(0) += 1;
        }
        let mut scored: Vec<(usize, f32)> = Vec::new();
        for (doc_no, doc) in self.docs.iter().enumerate() {
            let mut total = 0.0f32;
            for (term, query_tf) in &counts {
                let Some(tf) = doc.tf.get(*term) else {
                    continue;
                };
                let Some(idf) = self.idf(term) else {
                    continue;
                };
                let normal = tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * doc.len / self.avgdl));
                total += idf * normal * (*query_tf as f32);
            }
            if total > 0.0 {
                scored.push((doc_no, total));
            }
        }
        scored.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit);
        scored
    }

    /// Keyword search. Returns `(doc_no, score)` best first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<(usize, f32)> {
        let terms: Vec<String> = lex(query)
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }
        self.score_terms(&terms, limit)
    }

    /// Jobs like `doc_no`: the doc's most distinctive terms become the query.
    pub fn similar(&self, doc_no: usize, limit: usize) -> Vec<(usize, f32)> {
        let Some(doc) = self.docs.get(doc_no) else {
            return Vec::new();
        };
        let mut weighted: Vec<(&String, f32)> = doc
            .tf
            .iter()
            .filter_map(|(term, tf)| self.idf(term).map(|idf| (term, tf * idf)))
            .collect();
        weighted.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let terms: Vec<String> = weighted
            .into_iter()
            .take(12)
            .map(|(term, _)| term.clone())
            .collect();
        self.score_terms(&terms, limit + 1)
            .into_iter()
            .filter(|(doc, _)| *doc != doc_no)
            .take(limit)
            .collect()
    }

    pub fn doc_id(&self, doc_no: usize) -> Option<&str> {
        self.docs.get(doc_no).map(|doc| doc.external_id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, title: &str, skills: &[&str], description: &str) -> crate::models::Job {
        crate::models::Job {
            external_id: id.to_string(),
            source: "test".to_string(),
            title: title.to_string(),
            company: "Acme".to_string(),
            location: Some("Bengaluru".to_string()),
            description: description.to_string(),
            skills: skills.iter().map(|skill| skill.to_string()).collect(),
            min_experience: None,
            max_experience: None,
            url: format!("https://example.com/{id}"),
            discovered_at: chrono::Utc::now(),
            published_at: None,
            work_mode: None,
        }
    }

    #[test]
    fn keyword_search_ranks_title_matches_first() {
        let jobs = vec![
            job("a", "Java Developer", &["Java"], "Java and Spring backend role."),
            job("b", "Rust Engineer", &["Rust"], "Rust systems role in Bengaluru."),
        ];
        let index = PostingIndex::build(&jobs);
        let hits = index.search("rust", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(index.doc_id(hits[0].0), Some("b"));
    }

    #[test]
    fn similar_finds_same_stack_postings() {
        let jobs = vec![
            job("a", "Rust Engineer", &["Rust"], "Rust and Kubernetes backend."),
            job("b", "Rust Developer", &["Rust"], "Rust services with Postgres."),
            job("c", "Java Developer", &["Java"], "Java and Spring backend role."),
        ];
        let index = PostingIndex::build(&jobs);
        let hits = index.similar(0, 10);
        assert!(!hits.is_empty());
        assert_eq!(index.doc_id(hits[0].0), Some("b"));
        assert!(hits.iter().all(|(doc, _)| index.doc_id(*doc) != Some("a")));
    }

    #[test]
    fn lex_keeps_plus_and_hash() {
        assert!(lex("C++ and C#").contains(&"c++".to_string()));
        assert!(lex("C++ and C#").contains(&"c#".to_string()));
        assert!(lex("Node.js role").contains(&"node".to_string()));
    }
}
