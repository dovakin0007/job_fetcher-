use crate::models::CandidateProfile;

const DEFAULT_ROLE_WORDS: &[&str] = &[
    "engineer",
    "developer",
    "scientist",
    "researcher",
    "manager",
    "designer",
    "analyst",
    "architect",
    "intern",
    "lead",
    "specialist",
    "consultant",
    "director",
    "officer",
];

const TITLE_NOISE: &[&str] = &[
    "senior",
    "junior",
    "staff",
    "principal",
    "associate",
    "internal",
    "software",
    "with",
    "from",
    "and",
    "the",
    "for",
    "level",
];

pub(crate) fn role_words(profile: &CandidateProfile) -> Vec<String> {
    let mut words = DEFAULT_ROLE_WORDS
        .iter()
        .map(|word| (*word).to_string())
        .collect::<Vec<_>>();
    for keyword in &profile.role_keywords {
        push_phrase(&mut words, keyword, 2);
    }
    for title in &profile.preferred_titles {
        push_phrase(&mut words, title, 4);
    }
    words
}

pub(crate) fn split_role_company(title: &str, role_words: &[String]) -> (String, Option<String>) {
    let title = title.trim();
    if let Some((role, company)) = split_once_ci(title, " at ") {
        let company = clean_company(company);
        if looks_like_role(role, role_words) && !company.is_empty() && company.len() < 80 {
            return (role.trim().to_string(), Some(company));
        }
    }
    for sep in [" | ", " - ", " — ", " – "] {
        if let Some((left, right)) = title.split_once(sep) {
            let left = left.trim();
            let right = clean_company(right.trim());
            if looks_like_role(left, role_words) && !right.is_empty() && right.len() < 80 {
                return (left.to_string(), Some(right));
            }
            if looks_like_role(&right, role_words) && !left.is_empty() && left.len() < 80 {
                return (right, Some(clean_company(left)));
            }
        }
    }
    (title.to_string(), None)
}

pub(crate) fn company_from_host(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?.trim_start_matches("www.");
    let label = host.split('.').next()?.trim();
    if label.is_empty()
        || label.eq_ignore_ascii_case("jobs")
        || label.eq_ignore_ascii_case("careers")
    {
        return None;
    }
    Some(prettify_slug(label))
}

fn looks_like_role(value: &str, role_words: &[String]) -> bool {
    let lower = value.to_lowercase();
    role_words.iter().any(|word| lower.contains(word.as_str()))
}

fn push_phrase(words: &mut Vec<String>, raw: &str, token_min: usize) {
    let phrase = raw.trim().to_lowercase();
    if phrase.chars().count() >= token_min {
        push_unique(words, phrase.clone());
    }
    for token in phrase.split(|ch: char| !ch.is_ascii_alphanumeric()) {
        if token.chars().count() >= token_min && !TITLE_NOISE.contains(&token) {
            push_unique(words, token.to_string());
        }
    }
}

fn push_unique(words: &mut Vec<String>, word: String) {
    if !word.is_empty() && !words.iter().any(|existing| existing == &word) {
        words.push(word);
    }
}

fn clean_company(value: &str) -> String {
    value
        .split(['|', '·'])
        .next()
        .unwrap_or(value)
        .trim()
        .trim_end_matches('.')
        .to_string()
}

fn prettify_slug(value: &str) -> String {
    value
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn split_once_ci<'a>(value: &'a str, needle: &str) -> Option<(&'a str, &'a str)> {
    let lower = value.to_lowercase();
    let index = lower.find(&needle.to_lowercase())?;
    Some((&value[..index], &value[index + needle.len()..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(role_keywords: Vec<String>, preferred_titles: Vec<String>) -> CandidateProfile {
        CandidateProfile {
            name: "Candidate".into(),
            summary: String::new(),
            skills: vec![],
            experience_years: 4.0,
            preferred_titles,
            role_keywords,
            preferred_locations: vec![],
            excluded_keywords: vec![],
            minimum_match_score: 60.0,
            projects: vec![],
            preferred_work_modes: vec![],
            auto_apply: false,
            email: String::new(),
            phone: String::new(),
            skill_weights: vec![],
        }
    }

    #[test]
    fn splits_a_builtin_role() {
        let words = role_words(&profile(vec![], vec![]));
        let (role, company) = split_role_company("Machine Learning Engineer at Example AI", &words);
        assert_eq!(role, "Machine Learning Engineer");
        assert_eq!(company.as_deref(), Some("Example AI"));
    }

    #[test]
    fn user_keyword_marks_a_custom_role() {
        let words = role_words(&profile(vec!["fellow".into()], vec![]));
        let (role, company) = split_role_company("Research Fellow - DeepMind", &words);
        assert_eq!(role, "Research Fellow");
        assert_eq!(company.as_deref(), Some("DeepMind"));
    }

    #[test]
    fn preferred_title_adds_its_words() {
        let words = role_words(&profile(vec![], vec!["Research Fellow".into()]));
        assert!(words.iter().any(|word| word == "fellow"));
        let (role, company) = split_role_company("Research Fellow | DeepMind", &words);
        assert_eq!(role, "Research Fellow");
        assert_eq!(company.as_deref(), Some("DeepMind"));
    }
}
