use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateProfile {
    pub name: String,
    pub summary: String,
    pub skills: Vec<String>,
    pub experience_years: f32,
    pub preferred_titles: Vec<String>,
    /// Extra words that mark a search-result title as a job role, such as "fellow" or "sre".
    /// Built-in role words and preferred titles are always included as well.
    #[serde(default)]
    pub role_keywords: Vec<String>,
    pub preferred_locations: Vec<String>,
    pub excluded_keywords: Vec<String>,
    pub minimum_match_score: f32,
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub preferred_work_modes: Vec<String>,
    /// When set, discovery drafts an application for every job at or above the minimum score.
    /// Greenhouse jobs are submitted through the official job board API.
    #[serde(default)]
    pub auto_apply: bool,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub phone: String,
    /// Your own skill weights, from 0 to 10. These override the summary and experience estimate.
    #[serde(default)]
    pub skill_weights: Vec<SkillWeight>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillWeight {
    pub skill: String,
    pub weight: f32,
}
impl CandidateProfile {
    pub fn semantic_document(&self) -> String {
        format!("{} {} Skills: {}. Experience: {} years. Projects: {}. Target roles: {}. Locations: {}.", self.name, self.summary, self.skills.join(", "), self.experience_years, self.projects.join("; "), self.preferred_titles.join(", "), self.preferred_locations.join(", "))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub external_id: String,
    pub source: String,
    pub title: String,
    pub company: String,
    pub location: Option<String>,
    pub description: String,
    #[serde(default)]
    pub skills: Vec<String>,
    pub min_experience: Option<f32>,
    pub max_experience: Option<f32>,
    pub url: String,
    pub discovered_at: DateTime<Utc>,
    /// When the employer posted the job. Missing dates are treated as not recent.
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub work_mode: Option<String>,
}

const CLOSED_POSTING: &[&str] = &[
    "no longer accepting",
    "no longer available",
    "job has expired",
    "posting has expired",
    "this position has been filled",
    "position has been filled",
    "applications are closed",
    "job is closed",
    "posting is closed",
];

/// Upper bound for the configurable search age window. The discovery APIs
/// support it natively: SerpApi takes `location` + parses Google's posted
/// dates, Tavily maps
/// to day/week/month/year, Brave to pd/pw/pm/py — the local
/// `posted_within` check then enforces the exact day count.
pub const MAX_AGE_DAYS: u32 = 90;

fn default_country() -> String {
    "India".into()
}

fn default_max_age() -> u32 {
    7
}

fn default_follow_location() -> bool {
    true
}

fn default_true() -> bool {
    true
}

/// User search scope. Stored in SQLite and editable from the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default = "default_country")]
    pub target_country: String,
    #[serde(default = "default_max_age")]
    pub max_age_days: u32,
    /// City the search starts from. Editable, and replaced on each search when follow_location is set.
    #[serde(default)]
    pub location: String,
    /// Extra cities searched alongside the primary one (up to 5 after cleanup).
    #[serde(default)]
    pub extra_locations: Vec<String>,
    /// Include remote postings: adds remote query variants per search run.
    #[serde(default = "default_true")]
    pub include_remote: bool,
    /// Include hybrid postings: adds hybrid query variants per search run.
    #[serde(default = "default_true")]
    pub include_hybrid: bool,
    #[serde(default = "default_follow_location")]
    pub follow_location: bool,
    #[serde(default)]
    pub detected_city: String,
    #[serde(default)]
    pub detected_region: String,
    #[serde(default)]
    pub detected_country: String,
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    #[serde(default)]
    pub nearby: Vec<String>,
    /// "browser" when the dashboard sent coordinates, "ip" for the scheduled fallback.
    #[serde(default)]
    pub location_source: String,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            target_country: default_country(),
            max_age_days: default_max_age(),
            location: String::new(),
            extra_locations: Vec::new(),
            include_remote: true,
            include_hybrid: true,
            follow_location: true,
            detected_city: String::new(),
            detected_region: String::new(),
            detected_country: String::new(),
            latitude: None,
            longitude: None,
            nearby: Vec::new(),
            location_source: String::new(),
        }
    }
}

impl SearchConfig {
    pub fn normalized(mut self) -> Self {
        self.target_country = self.target_country.trim().to_string();
        self.max_age_days = self.max_age_days.clamp(1, MAX_AGE_DAYS);
        self.location = self.location.trim().to_string();
        self.detected_city = self.detected_city.trim().to_string();
        self.detected_region = self.detected_region.trim().to_string();
        self.detected_country = self.detected_country.trim().to_string();
        self.location_source = self.location_source.trim().to_string();
        self.nearby = self
            .nearby
            .into_iter()
            .map(|place| place.trim().to_string())
            .filter(|place| !place.is_empty())
            .collect();
        self.extra_locations = self
            .extra_locations
            .into_iter()
            .map(|place| place.trim().to_string())
            .filter(|place| !place.is_empty())
            .fold(Vec::new(), |mut seen: Vec<String>, place| {
                if !seen.iter().any(|existing| existing.eq_ignore_ascii_case(&place)) {
                    seen.push(place);
                }
                seen
            });
        self.extra_locations.truncate(5);
        self
    }

    /// Search starts at the saved city, then the extra cities, then the
    /// nearest detected places (capped at 4 distinct places).
    pub fn search_places(&self) -> Vec<String> {
        let mut places = Vec::new();
        let mut push = |place: &str| {
            let place = place.trim();
            if place.is_empty()
                || places.len() >= 4
                || places.iter().any(|existing: &String| existing.eq_ignore_ascii_case(place))
            {
                return;
            }
            places.push(place.to_string());
        };
        push(&self.location.clone());
        for place in self.extra_locations.iter().chain(self.nearby.iter()) {
            push(place);
        }
        places
    }

    pub fn country_code(&self) -> Option<&'static str> {
        match self.target_country.trim().to_ascii_lowercase().as_str() {
            "india" | "in" => Some("IN"),
            "united states" | "usa" | "us" => Some("US"),
            "united kingdom" | "uk" | "gb" => Some("GB"),
            "germany" | "de" => Some("DE"),
            "canada" | "ca" => Some("CA"),
            "singapore" | "sg" => Some("SG"),
            "australia" | "au" => Some("AU"),
            _ => None,
        }
    }

    pub fn targets_india(&self) -> bool {
        matches!(
            self.target_country.trim().to_ascii_lowercase().as_str(),
            "india" | "in"
        )
    }
}

/// A posting is kept when it is still open and was published in the last 7 days.
pub fn is_recent_open_posting(published_at: Option<DateTime<Utc>>, text: &str) -> bool {
    if posting_is_closed(text) {
        return false;
    }
    let Some(published) = published_at else {
        return false;
    };
    posted_within(published, 7)
}

/// Keeps an open posting in the configured country and age window.
/// Listings with no date are kept only when a country is set and the text matches it.
pub fn is_applicable_posting(
    published_at: Option<DateTime<Utc>>,
    text: &str,
    url: &str,
    config: &SearchConfig,
) -> bool {
    if posting_is_closed(text) {
        return false;
    }
    let country = config.target_country.trim();
    if !country.is_empty() && !matches_country(country, text, url) {
        return false;
    }
    match published_at {
        Some(published) => posted_within(published, config.max_age_days),
        None => !country.is_empty(),
    }
}

fn matches_country(country: &str, text: &str, url: &str) -> bool {
    let country = country.to_ascii_lowercase();
    if country == "india" || country == "in" {
        return india_applicable(text, url);
    }
    let tokens = words(text);
    let parts: Vec<&str> = country.split_whitespace().collect();
    !parts.is_empty()
        && parts
            .iter()
            .all(|part| tokens.iter().any(|word| word == part))
        || url.to_ascii_lowercase().contains(&country.replace(' ', ""))
}

pub fn india_place(text: &str) -> Option<&'static str> {
    const CITIES: &[(&str, &str)] = &[
        ("bengaluru", "Bengaluru"),
        ("bangalore", "Bengaluru"),
        ("chennai", "Chennai"),
        ("hyderabad", "Hyderabad"),
        ("mumbai", "Mumbai"),
        ("pune", "Pune"),
        ("delhi", "Delhi"),
        ("gurgaon", "Gurugram"),
        ("gurugram", "Gurugram"),
        ("noida", "Noida"),
        ("kolkata", "Kolkata"),
        ("ahmedabad", "Ahmedabad"),
        ("kochi", "Kochi"),
        ("coimbatore", "Coimbatore"),
        ("jaipur", "Jaipur"),
        ("chandigarh", "Chandigarh"),
        ("indore", "Indore"),
        ("lucknow", "Lucknow"),
        ("trivandrum", "Thiruvananthapuram"),
        ("thiruvananthapuram", "Thiruvananthapuram"),
    ];
    let words = words(text);
    for (needle, label) in CITIES {
        if words.iter().any(|word| word == needle) {
            return Some(*label);
        }
    }
    let india = words
        .iter()
        .any(|word| *word == "india" || *word == "indian");
    let remote = words.iter().any(|word| *word == "remote");
    if remote && india {
        return Some("Remote, India");
    }
    if india {
        return Some("India");
    }
    None
}

fn india_applicable(text: &str, url: &str) -> bool {
    if india_place(text).is_some() {
        return true;
    }
    let url = url.to_ascii_lowercase();
    url.contains(".co.in") || url.contains(".in/") || url.ends_with(".in")
}

fn posted_within(published: DateTime<Utc>, max_age_days: u32) -> bool {
    let age = Utc::now().signed_duration_since(published);
    age >= chrono::Duration::zero()
        && age <= chrono::Duration::days(max_age_days.clamp(1, MAX_AGE_DAYS) as i64)
}

pub(crate) fn posting_is_closed(text: &str) -> bool {
    let lower = text.to_lowercase();
    CLOSED_POSTING.iter().any(|phrase| lower.contains(phrase))
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn parse_posted_at(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    let date = value.get(..10).unwrap_or(value);
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc())
}
impl Job {
    pub fn semantic_document(&self) -> String {
        format!(
            "{} {} {} {} {} {}",
            self.title,
            self.company,
            self.location.as_deref().unwrap_or(""),
            self.work_mode.as_deref().unwrap_or(""),
            self.skills.join(" "),
            self.description
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_an_open_posting_from_this_week() {
        let posted = Utc::now() - chrono::Duration::days(2);
        assert!(is_recent_open_posting(Some(posted), "Backend Engineer"));
        assert_eq!(
            parse_posted_at("2026-11-13T00:00:00Z").map(|value| value.to_rfc3339()),
            Some("2026-11-13T00:00:00+00:00".into())
        );
    }

    #[test]
    fn keeps_open_india_roles_and_drops_other_countries() {
        assert_eq!(
            india_place("Backend Engineer, Bangalore"),
            Some("Bengaluru")
        );
        let india = SearchConfig::default();
        assert!(is_applicable_posting(
            None,
            "Senior Engineer in Chennai",
            "https://example.com/jobs/1",
            &india
        ));
        assert!(!is_applicable_posting(
            None,
            "Remote Staff Engineer, United States",
            "https://example.com/jobs/2",
            &india
        ));
        assert!(!is_applicable_posting(
            Some(Utc::now() - chrono::Duration::days(12)),
            "Engineer in Hyderabad",
            "https://example.com/jobs/3",
            &india
        ));
        let germany = SearchConfig {
            target_country: "Germany".into(),
            ..SearchConfig::default()
        };
        assert!(is_applicable_posting(
            None,
            "Engineer in Germany",
            "https://example.com/jobs/4",
            &germany
        ));
        assert!(!is_applicable_posting(
            None,
            "Engineer in Chennai",
            "https://example.com/jobs/5",
            &germany
        ));
    }

    #[test]
    fn drops_closed_and_old_postings() {
        let posted = Utc::now() - chrono::Duration::days(2);
        assert!(!is_recent_open_posting(
            Some(posted),
            "This position has been filled"
        ));
        assert!(!is_recent_open_posting(
            Some(Utc::now() - chrono::Duration::days(10)),
            "Backend Engineer"
        ));
        assert!(!is_recent_open_posting(None, "Backend Engineer"));
    }

    #[test]
    fn search_covers_extra_cities_then_nearby() {
        let config = SearchConfig {
            location: "Chennai".into(),
            extra_locations: vec![
                "Pune".into(),
                "chennai ".into(),
                "PUNE".into(),
                String::new(),
            ],
            nearby: vec!["Bengaluru".into(), "Chennai".into()],
            ..SearchConfig::default()
        }
        .normalized();
        assert_eq!(config.extra_locations, vec!["Pune".to_string(), "chennai".to_string()]);
        assert_eq!(
            config.search_places(),
            vec!["Chennai".to_string(), "Pune".to_string(), "Bengaluru".to_string()]
        );
    }

    #[test]
    fn extra_cities_are_capped() {
        let config = SearchConfig {
            extra_locations: ["A", "B", "C", "D", "E", "F", "G"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ..SearchConfig::default()
        }
        .normalized();
        assert_eq!(config.extra_locations.len(), 5);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    pub job: Job,
    pub score: f32,
    pub deterministic_score: f32,
    pub semantic_score: Option<f32>,
    pub llm_score: Option<f32>,
    pub matched_skills: Vec<String>,
    pub missing_skills: Vec<String>,
    pub explanation: String,
    pub status: String,
    /// 1-based position after the posting index is ranked against the weightage resume.
    #[serde(default)]
    pub rank: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Application {
    pub id: String,
    pub external_id: String,
    pub job_title: String,
    pub company: String,
    pub job_url: String,
    pub status: String,
    pub tailored_summary: String,
    pub selected_bullets: Vec<String>,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
