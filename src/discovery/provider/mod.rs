mod brave;
mod cache;
mod enrich;
mod jobs;
mod jsearch;
mod serpapi;
mod tavily;
mod title;
mod verify;

pub use brave::BraveProvider;
pub use cache::clear_search_cache;
pub use jobs::{
    remove_search_keys, save_search_keys, search_key_status, SearchJobProvider,
};
pub use jsearch::JSearchProvider;
pub use serpapi::{SerpApiGlassdoorProvider, SerpApiProvider};
pub(crate) use jobs::{looks_like_business_listing, quality_gates};
pub use tavily::TavilyProvider;

use reqwest::Client;
use std::time::Duration;

pub(crate) const EXCLUDED_DOMAINS: &[&str] = &["linkedin.com", "naukri.com"];

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
    pub published_at: Option<String>,
    pub source: &'static str,
}

#[derive(Clone)]
pub enum SearchProvider {
    SerpApi(SerpApiProvider),
    SerpApiGlassdoor(SerpApiGlassdoorProvider),
    JSearch(JSearchProvider),
    Tavily(TavilyProvider),
    Brave(BraveProvider),
}

impl SearchProvider {
    pub fn name(&self) -> &'static str {
        match self {
            SearchProvider::SerpApi(_) => "serpapi",
            SearchProvider::SerpApiGlassdoor(_) => "serpapi-glassdoor",
            SearchProvider::JSearch(_) => "jsearch",
            SearchProvider::Tavily(_) => "tavily",
            SearchProvider::Brave(_) => "brave",
        }
    }

    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        country_code: Option<&str>,
        max_age_days: u32,
        location: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        match self {
            SearchProvider::SerpApi(provider) => {
                provider
                    .search(query, limit, country_code, max_age_days, location)
                    .await
            }
            SearchProvider::SerpApiGlassdoor(provider) => {
                provider
                    .search(query, limit, country_code, max_age_days, location)
                    .await
            }
            SearchProvider::JSearch(provider) => {
                provider
                    .search(query, limit, country_code, max_age_days, location)
                    .await
            }
            SearchProvider::Tavily(provider) => provider.search(query, limit, max_age_days).await,
            SearchProvider::Brave(provider) => {
                provider
                    .search(query, limit, country_code, max_age_days)
                    .await
            }
        }
    }
}

pub(crate) fn search_client() -> Client {
    Client::builder()
        .user_agent("RustJobAgent/0.6 (local job search)")
        .timeout(Duration::from_secs(30))
        .build()
        .expect("valid reqwest client")
}

pub(crate) fn env_secret(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value != "replace-me")
}

pub(crate) fn trim_err(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    clip(&compact, 280)
}

pub(crate) fn clip(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed.chars().take(max).collect::<String>() + "…"
}
