use super::cache::{self, CachedSearchHit};
use super::enrich;
use super::title::{company_from_host, role_words, split_role_company};
use super::{env_secret, SearchProvider, SearchResult};
use crate::models::{CandidateProfile, Job};
use crate::providers::{greenhouse, JobProvider};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug, Clone, Serialize)]
pub struct SearchKeyStatus {
    pub serpapi_configured: bool,
    pub jsearch_configured: bool,
    pub tavily_configured: bool,
    pub brave_configured: bool,
    /// Masked hints (`••••••••ab12`) — the full key is never exposed.
    #[serde(default)]
    pub serpapi_hint: Option<String>,
    #[serde(default)]
    pub jsearch_hint: Option<String>,
    #[serde(default)]
    pub tavily_hint: Option<String>,
    #[serde(default)]
    pub brave_hint: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredSearchKeys {
    #[serde(default)]
    serpapi_api_key: String,
    #[serde(default)]
    jsearch_api_key: String,
    #[serde(default)]
    tavily_api_key: String,
    #[serde(default)]
    brave_api_key: String,
}

/// Runs SerpApi Google Jobs, Tavily, and/or Brave during discovery. Queries
/// carry the candidate's title and tech stack. Descriptions are enriched to
/// full page text (cached) before jobs are built, so matching scores and
/// skill extraction see the whole posting, not a fragment.
/// Greenhouse job URLs are loaded through the official board API. This provider
/// does not log into job sites or submit applications.
/// Snippets are enriched to full page text (cached) before jobs are built, so
/// matching scores and skill extraction see the whole posting, not a fragment.
/// Greenhouse job URLs are loaded through the official board API. This provider
/// does not log into job sites or submit applications.
pub struct SearchJobProvider {
    engines: Mutex<Vec<SearchProvider>>,
    keys_path: PathBuf,
    limit: usize,
    db: SqlitePool,
}

impl SearchJobProvider {
    pub fn from_env(preferences_path: impl Into<PathBuf>, db: SqlitePool) -> Self {
        // `preferences_path` is kept as the anchor for the legacy keys file;
        // preferences themselves now live in the database.
        let keys_path = preferences_path
            .into()
            .parent()
            .unwrap_or_else(|| Path::new("data"))
            .join("search.json");
        let limit = std::env::var("SEARCH_RESULT_LIMIT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(15)
            .clamp(1, 20);
        Self {
            // Populated on the first discovery run (needs the DB, so async).
            engines: Mutex::new(Vec::new()),
            keys_path,
            limit,
            db,
        }
    }
}

/// One provider's key from the winning source: env, encrypted DB, or legacy file.
struct KeySource {
    key: String,
    hint: String,
}

async fn key_source(
    db: &SqlitePool,
    env_name: &str,
    file_key: &str,
    db_name: &str,
) -> Option<KeySource> {
    if let Some(key) = env_secret(env_name) {
        return Some(KeySource {
            hint: crate::secrets::hint_of(&key),
            key,
        });
    }
    match crate::store::load_secret(db, db_name).await {
        Ok(Some((nonce, ciphertext, last4))) => match crate::secrets::open(&nonce, &ciphertext) {
            Ok(key) => {
                return Some(KeySource {
                    hint: crate::secrets::mask(&last4),
                    key,
                });
            }
            Err(error) => {
                tracing::warn!(provider = db_name, %error, "stored search key cannot be decrypted; ignoring it");
            }
        },
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(provider = db_name, %error, "stored search key lookup failed");
        }
    }
    clean_secret(Some(file_key)).map(|key| KeySource {
        hint: crate::secrets::hint_of(&key),
        key,
    })
}

pub async fn search_key_status(db: &SqlitePool, path: &Path) -> SearchKeyStatus {
    let stored = read_stored_keys(path);
    let serpapi = key_source(db, "SERPAPI_API_KEY", &stored.serpapi_api_key, "serpapi").await;
    let jsearch = key_source(db, "JSEARCH_API_KEY", &stored.jsearch_api_key, "jsearch").await;
    let tavily = key_source(db, "TAVILY_API_KEY", &stored.tavily_api_key, "tavily").await;
    let brave = key_source(db, "BRAVE_API_KEY", &stored.brave_api_key, "brave").await;
    SearchKeyStatus {
        serpapi_configured: serpapi.is_some(),
        jsearch_configured: jsearch.is_some(),
        tavily_configured: tavily.is_some(),
        brave_configured: brave.is_some(),
        serpapi_hint: serpapi.map(|k| k.hint),
        jsearch_hint: jsearch.map(|k| k.hint),
        tavily_hint: tavily.map(|k| k.hint),
        brave_hint: brave.map(|k| k.hint),
    }
}

/// Seals a dashboard-supplied key into the encrypted DB store. Blank values keep
/// whatever is already stored. After a successful save, any legacy plaintext
/// keys are wiped from `search.json` so only ciphertext remains on disk.
pub async fn save_search_keys(
    db: &SqlitePool,
    path: &Path,
    serpapi_api_key: Option<&str>,
    jsearch_api_key: Option<&str>,
    tavily_api_key: Option<&str>,
    brave_api_key: Option<&str>,
) -> anyhow::Result<SearchKeyStatus> {
    for (name, value) in [
        ("serpapi", serpapi_api_key),
        ("jsearch", jsearch_api_key),
        ("tavily", tavily_api_key),
        ("brave", brave_api_key),
    ] {
        if let Some(key) = clean_secret(value) {
            let sealed = crate::secrets::seal(&key)?;
            crate::store::save_secret(
                db,
                name,
                &sealed.nonce_b64,
                &sealed.ciphertext_b64,
                &sealed.last4,
            )
            .await?;
        }
    }
    // Migration: seal any legacy plaintext file keys into the DB, then wipe
    // them from the file so only ciphertext remains on disk.
    migrate_legacy_keys(db, path).await?;
    Ok(search_key_status(db, path).await)
}

/// Seals legacy `search.json` plaintext keys into the encrypted DB store and
/// wipes them from the file. Runs on every discovery and every save, but only
/// does work once — afterwards the file holds no keys.
async fn migrate_legacy_keys(db: &SqlitePool, path: &Path) -> anyhow::Result<()> {
    let mut stored = read_stored_keys(path);
    let mut changed = false;
    for (name, slot) in [
        ("serpapi", &mut stored.serpapi_api_key),
        ("jsearch", &mut stored.jsearch_api_key),
        ("tavily", &mut stored.tavily_api_key),
        ("brave", &mut stored.brave_api_key),
    ] {
        let has_db = crate::store::load_secret(db, name).await?.is_some();
        if !slot.trim().is_empty() && !has_db {
            let sealed = crate::secrets::seal(slot)?;
            crate::store::save_secret(
                db,
                name,
                &sealed.nonce_b64,
                &sealed.ciphertext_b64,
                &sealed.last4,
            )
            .await?;
        }
        if !slot.trim().is_empty() && crate::store::load_secret(db, name).await?.is_some() {
            slot.clear();
            changed = true;
        }
    }
    if changed {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, serde_json::to_vec_pretty(&stored)?)?;
    }
    Ok(())
}

/// Forgets dashboard-saved keys. Environment keys are unaffected.
pub async fn remove_search_keys(
    db: &SqlitePool,
    serpapi: bool,
    jsearch: bool,
    tavily: bool,
    brave: bool,
) -> anyhow::Result<()> {
    for (name, remove) in [
        ("serpapi", serpapi),
        ("jsearch", jsearch),
        ("tavily", tavily),
        ("brave", brave),
    ] {
        if remove {
            crate::store::delete_secret(db, name).await?;
        }
    }
    Ok(())
}

async fn configured_providers(db: &SqlitePool, path: &Path) -> Vec<SearchProvider> {
    // Pick up keys added to .env after the process started. Existing variables stay as they are.
    let _ = dotenvy::dotenv();
    // Best-effort legacy migration; discovery must not fail because of it —
    // unreadable plaintext simply stays readable until the next save.
    if let Err(error) = migrate_legacy_keys(db, path).await {
        tracing::warn!(%error, "legacy search-key migration failed");
    }
    let stored = read_stored_keys(path);
    let mut engines = Vec::new();
    if let Some(source) = key_source(db, "SERPAPI_API_KEY", &stored.serpapi_api_key, "serpapi").await {
        engines.push(SearchProvider::SerpApi(super::SerpApiProvider::new(
            source.key.clone(),
        )));
        engines.push(SearchProvider::SerpApiGlassdoor(
            super::SerpApiGlassdoorProvider::new(source.key),
        ));
    }
    if let Some(source) = key_source(db, "JSEARCH_API_KEY", &stored.jsearch_api_key, "jsearch").await {
        engines.push(SearchProvider::JSearch(super::JSearchProvider::new(
            source.key,
        )));
    }
    if let Some(source) = key_source(db, "TAVILY_API_KEY", &stored.tavily_api_key, "tavily").await {
        engines.push(SearchProvider::Tavily(super::TavilyProvider::new(
            source.key,
        )));
    }
    if let Some(source) = key_source(db, "BRAVE_API_KEY", &stored.brave_api_key, "brave").await {
        engines.push(SearchProvider::Brave(super::BraveProvider::new(source.key)));
    }
    engines
}

fn read_stored_keys(path: &Path) -> StoredSearchKeys {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn clean_secret(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value == "replace-me" {
        None
    } else {
        Some(value.to_string())
    }
}

#[async_trait]
impl JobProvider for SearchJobProvider {
    fn name(&self) -> &'static str {
        "job-search"
    }

    async fn discover(&self) -> anyhow::Result<Vec<Job>> {
        let engines = configured_providers(&self.db, &self.keys_path).await;
        *self.engines.lock().expect("search providers") = engines.clone();
        if engines.is_empty() {
            tracing::info!(
                "SERPAPI_API_KEY, TAVILY_API_KEY and BRAVE_API_KEY are unset; web job search skipped"
            );
            return Ok(Vec::new());
        }
        let Some(profile) = crate::store::load_preferences(&self.db).await? else {
            tracing::warn!(
                "matching preferences missing; save them in the dashboard before discovery"
            );
            return Ok(Vec::new());
        };
        let config = crate::store::load_search_config(&self.db).await?;
        let queries = job_queries(&profile, &config);
        if queries.is_empty() {
            tracing::info!("No preferred titles or skills; web job search skipped");
            return Ok(Vec::new());
        }

        let client = super::search_client();
        let today = cache::today_stamp();
        if let Err(err) = cache::soft_delete_other_days(&self.db, &today).await {
            tracing::warn!(error = %err, "could not retire search pages from earlier days");
        }
        let country_code = config.country_code().map(str::to_string);
        // Paginated fetching: only one small HTTP batch per run; everything
        // else is served from today's SQLite pages. The offset is persisted
        // so the next run (next page load / scheduled tick) continues with
        // the following queries. Coverage grows as each slice is cached.
        let window = crate::discovery::rate_limit::queries_per_run();
        let fetch_budget = if window == 0 { usize::MAX } else { window.max(1) };
        let offset = load_search_offset(&self.db).await;
        let (active_queries, next_offset) =
            crate::discovery::rate_limit::select_query_window(&queries, offset, window);
        let active_set: HashSet<&str> =
            active_queries.iter().map(String::as_str).collect();
        if let Err(err) = save_search_offset(&self.db, next_offset).await {
            tracing::warn!(error = %err, "search query offset was not saved");
        }
        tracing::info!(
            total_queries = queries.len(),
            window = active_queries.len(),
            fetch_budget = fetch_budget,
            offset = offset,
            next_offset = next_offset,
            "search query window for this run"
        );
        let mut jobs = Vec::new();
        let mut seen = HashSet::new();
        let mut errors = Vec::new();
        let mut boards: HashMap<String, Vec<u64>> = HashMap::new();
        let mut pending: Vec<PendingHit> = Vec::new();
        // Phase 1: serve everything already cached today — no HTTP, no rate cost.
        // Query-outer order keeps each query's engines together, so truncating
        // to `fetch_budget` below fetches whole queries first.
        let mut to_fetch: Vec<(SearchProvider, String)> = Vec::new();
        for query in &queries {
            for engine in &engines {
                let cache_key =
                    cache_key_for(query, country_code.as_deref(), config.max_age_days);
                match cache::load_pages(&self.db, &today, engine.name(), &cache_key).await {
                    Ok(Some(pages)) => {
                        tracing::info!(
                            provider = engine.name(),
                            pages = pages.len(),
                            "using today's cached search pages"
                        );
                        for page in pages_from_cache(engine.name(), pages) {
                            for result in page {
                                collect_search_result(
                                    &config,
                                    result,
                                    &mut seen,
                                    &mut pending,
                                    &mut boards,
                                );
                            }
                        }
                    }
                    Ok(None) => {
                        if active_set.contains(query.as_str()) {
                            to_fetch.push((engine.clone(), query.clone()));
                        } else {
                            tracing::info!(
                                provider = engine.name(),
                                query = query.as_str(),
                                "query deferred to a later run (outside window)"
                            );
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "search page cache lookup failed");
                        if active_set.contains(query.as_str()) {
                            to_fetch.push((engine.clone(), query.clone()));
                        }
                    }
                }
            }
        }
        // Paginate the HTTP batch: fetch at most `fetch_budget` pairs now,
        // store their pages in SQLite, defer the rest to later runs.
        if to_fetch.len() > fetch_budget {
            let deferred = to_fetch.len() - fetch_budget;
            tracing::info!(
                deferred = deferred,
                fetch_now = fetch_budget,
                "deferring search fetches to later runs (paginated)"
            );
            to_fetch.truncate(fetch_budget);
        }
        // Phase 2: fetch one paginated batch — one tokio task per (engine,
        // query) pair, up to `fetch_budget` (default 2). Each call holds a
        // rate_limit::acquire() permit across send + enforces
        // SEARCH_MIN_GAP_MS between starts, so parallel tasks still start
        // ~300ms apart per provider. Pages are
        // stored in SQLite; 429s are retried with backoff inside providers.
        let mut fetch_handles = Vec::new();
        for (engine, query) in to_fetch {
            let engine_name = engine.name().to_string();
            let max_results = 20;
            let db = self.db.clone();
            let today = today.clone();
            let country_code = country_code.clone();
            let page_size = self.limit;
            let max_age_days = config.max_age_days;
            let place: Option<String> = (!config.location.trim().is_empty())
                .then(|| config.location.clone());
            fetch_handles.push((
                engine_name,
                query.clone(),
                tokio::spawn(async move {
                    cached_search(
                        &db,
                        &today,
                        &engine,
                        &query,
                        page_size,
                        max_results,
                        country_code.as_deref(),
                        max_age_days,
                        place.as_deref(),
                    )
                    .await
                }),
            ));
        }
        for (engine_name, query, handle) in fetch_handles {
            match handle.await {
                Ok(Ok(pages)) => {
                    for page in pages {
                        for result in page {
                            collect_search_result(
                                &config,
                                result,
                                &mut seen,
                                &mut pending,
                                &mut boards,
                            );
                        }
                    }
                }
                Ok(Err(err)) => {
                    tracing::warn!(engine = %engine_name, query = %query, error = %err, "job search query failed");
                    errors.push(err.to_string());
                }
                Err(err) => {
                    tracing::warn!(engine = %engine_name, query = %query, error = %err, "job search task failed");
                    errors.push(err.to_string());
                }
            }
        }
        // Enrich thin snippets to full page text (cached in SQLite), then
        // build jobs from the fullest text available per posting.
        let thin_urls: Vec<String> = pending
            .iter()
            .filter(|hit| enrich::needs_enrichment(hit.result.snippet.as_deref()))
            .map(|hit| hit.url.clone())
            .collect();
        let enriched = enrich::enrich(&thin_urls, &self.db).await;
        for hit in pending {
            let full = enriched.get(&hit.url).map(String::as_str);
            if let Some(job) = job_from_snippet(&profile, &config, hit.result, &hit.url, full) {
                if seen.insert(job.external_id.clone()) {
                    jobs.push(job);
                }
            }
        }
        // Legitimacy check: confirm each posting URL still serves a live,
        // open job page before it enters the backend. Dead pages are dropped
        // here (and logged); fetch failures fail open so a flaky network
        // never wipes good jobs. Greenhouse board jobs come from the live
        // board API itself, so they skip this step.
        let verify_urls: Vec<String> = jobs.iter().map(|job| job.url.clone()).collect();
        let live = super::verify::verify_postings(&verify_urls, &self.db).await;
        let before = jobs.len();
        jobs.retain(|job| live.get(&job.url).copied().unwrap_or(true));
        tracing::info!(
            checked = verify_urls.len(),
            removed = before - jobs.len(),
            "posting legitimacy check finished"
        );
        let mut board_handles = Vec::new();
        for (token, ids) in boards {
            let client = client.clone();
            board_handles.push(tokio::spawn(async move {
                greenhouse::fetch_jobs_by_id(&client, &token, &ids).await
            }));
        }
        for handle in board_handles {
            match handle.await {
                Ok(Ok(found)) => {
                    for job in found {
                        if !crate::models::is_applicable_posting(
                            job.published_at,
                            &format!(
                                "{} {} {}",
                                job.title,
                                job.location.as_deref().unwrap_or(""),
                                job.description
                            ),
                            &job.url,
                            &config,
                        ) {
                            continue;
                        }
                        if seen.insert(job.external_id.clone()) {
                            jobs.push(job);
                        }
                    }
                }
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, "Greenhouse board could not be loaded")
                }
                Err(err) => tracing::warn!(error = %err, "Greenhouse board task failed"),
            }
        }
        if jobs.is_empty() && !errors.is_empty() {
            anyhow::bail!(errors.join("; "));
        }
        tracing::info!(count = jobs.len(), "web job search finished");
        Ok(jobs)
    }
}

async fn cached_search(
    db: &SqlitePool,
    today: &str,
    engine: &SearchProvider,
    query: &str,
    page_size: usize,
    max_results: usize,
    country_code: Option<&str>,
    max_age_days: u32,
    location: Option<&str>,
) -> anyhow::Result<Vec<Vec<SearchResult>>> {
    let cache_key = cache_key_for(query, country_code, max_age_days);
    if let Some(pages) = cache::load_pages(db, today, engine.name(), &cache_key).await? {
        tracing::info!(
            provider = engine.name(),
            pages = pages.len(),
            "using today's cached search pages"
        );
        return Ok(pages_from_cache(engine.name(), pages));
    }
    let fetched = engine
        .search(query, max_results, country_code, max_age_days, location)
        .await?;
    let hits = fetched
        .iter()
        .map(|result| CachedSearchHit {
            title: result.title.clone(),
            url: result.url.clone(),
            snippet: result.snippet.clone(),
            published_at: result.published_at.clone(),
        })
        .collect();
    let pages = cache::chunk_hits(hits, page_size);
    if let Err(err) = cache::store_pages(db, today, engine.name(), &cache_key, &pages).await {
        tracing::warn!(error = %err, "search page cache was not written");
    } else {
        tracing::info!(
            provider = engine.name(),
            pages = pages.len(),
            "cached search pages for today"
        );
    }
    Ok(pages_from_cache(engine.name(), pages))
}

fn cache_key_for(query: &str, country_code: Option<&str>, max_age_days: u32) -> String {
    format!("{query}|{}|{max_age_days}", country_code.unwrap_or(""))
}

/// Persisted rotation offset for query windowing (kv_store key
/// `search_query_offset`). The next discovery run continues after this run's
/// window, so repeated page loads / scheduled ticks sweep the whole query
/// list instead of re-hammering the first queries.
const SEARCH_OFFSET_KEY: &str = "search_query_offset";

async fn load_search_offset(db: &SqlitePool) -> usize {
    crate::store::kv_get(db, SEARCH_OFFSET_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0)
}

async fn save_search_offset(db: &SqlitePool, offset: usize) -> anyhow::Result<()> {
    crate::store::kv_set(db, SEARCH_OFFSET_KEY, &offset.to_string()).await
}

fn pages_from_cache(provider: &str, pages: Vec<Vec<CachedSearchHit>>) -> Vec<Vec<SearchResult>> {
    let source = match provider {
        "tavily" => "tavily",
        "brave" => "brave",
        "serpapi" => "serpapi",
        "serpapi-glassdoor" => "serpapi-glassdoor",
        "jsearch" => "jsearch",
        _ => "web",
    };
    pages
        .into_iter()
        .map(|page| {
            page.into_iter()
                .map(|hit| SearchResult {
                    title: hit.title,
                    url: hit.url,
                    snippet: hit.snippet,
                    published_at: hit.published_at,
                    source,
                })
                .collect()
        })
        .collect()
}

fn job_queries(profile: &CandidateProfile, config: &crate::models::SearchConfig) -> Vec<String> {
    let country = config.target_country.as_str();
    let detected = config.search_places();
    let locations: Vec<&str> = if detected.is_empty() {
        profile
            .preferred_locations
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .take(4)
            .collect()
    } else {
        detected.iter().map(String::as_str).take(4).collect()
    };
    let titles: Vec<&str> = profile
        .preferred_titles
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .take(6)
        .collect();
    let skills: Vec<&str> = profile
        .skills
        .iter()
        .map(|value| value.trim())
        .filter(|value| value.len() >= 2)
        .take(3)
        .collect();
    let places: Vec<&str> = if locations.is_empty() {
        vec![""]
    } else {
        locations
    };

    let mut queries = Vec::new();
    // API-specific targeting: every title query carries the candidate's tech
    // stack in the text, so the API returns that stack ("Senior Software
    // Engineer Rust Go…") instead of whatever is popular (Node/Java…).
    // Stack-specific skill queries follow for depth.
    let stack = skills.join(" ");
    for title in &titles {
        for place in &places {
            let base = if stack.is_empty() {
                format!("hiring {title} job")
            } else {
                format!("hiring {title} {stack} job")
            };
            queries.push(with_place(&base, place, country));
        }
    }
    if config.include_remote {
        for title in titles.iter().take(2) {
            queries.push(with_place(&format!("hiring {title} remote job"), "", country));
        }
    }
    if config.include_hybrid {
        if let Some(place) = places.first() {
            for title in titles.iter().take(2) {
                queries.push(with_place(&format!("hiring hybrid {title} job"), place, country));
            }
        }
    }
    let skill_places = if places.len() > 2 {
        &places[..2]
    } else {
        places.as_slice()
    };
    for skill in &skills {
        for place in skill_places {
            queries.push(with_place(
                &format!("{skill} engineer job posting"),
                place,
                country,
            ));
        }
    }
    if queries.is_empty() {
        for skill in &skills {
            queries.push(with_place(
                &format!("hiring {skill} job posting"),
                "",
                country,
            ));
        }
    }
    let mut seen = HashSet::new();
    queries
        .into_iter()
        .filter(|query| seen.insert(query.to_ascii_lowercase()))
        .take(18)
        .collect()
}

fn with_place(query: &str, place: &str, country: &str) -> String {
    let country = country.trim();
    let india = country.eq_ignore_ascii_case("india") || country.eq_ignore_ascii_case("in");
    let place = if india {
        match place.trim().to_ascii_lowercase().as_str() {
            "" => "India".to_string(),
            "remote" => "India remote".to_string(),
            "bengaluru" | "bangalore" => "Bengaluru Bangalore India".to_string(),
            "chennai" => "Chennai India".to_string(),
            _ if place.to_ascii_lowercase().contains("india") => place.trim().to_string(),
            _ => format!("{} India", place.trim()),
        }
    } else if country.is_empty() {
        place.trim().to_string()
    } else if place.trim().is_empty() {
        country.to_string()
    } else if place
        .to_ascii_lowercase()
        .contains(&country.to_ascii_lowercase())
    {
        place.trim().to_string()
    } else if place.eq_ignore_ascii_case("remote") {
        format!("{country} remote")
    } else {
        format!("{} {country}", place.trim())
    };
    if place.is_empty() {
        query.to_string()
    } else {
        format!("{query} in {place}")
    }
}

struct PendingHit {
    result: SearchResult,
    url: String,
}

fn collect_search_result(
    config: &crate::models::SearchConfig,
    result: SearchResult,
    seen: &mut HashSet<String>,
    pending: &mut Vec<PendingHit>,
    boards: &mut HashMap<String, Vec<u64>>,
) {
    let url = result.url.trim().to_string();
    if !allowed_result_url(&url) || !seen.insert(normalize_url(&url)) {
        return;
    }
    if let Some(link) = greenhouse::parse_board_url(&url) {
        if let Some(job_id) = link.job_id {
            boards.entry(link.token).or_default().push(job_id);
        }
        return;
    }
    // Cheap pre-filter on the snippet so enrichment is only fetched for
    // plausibly applicable postings; the full text is re-checked at build time.
    let title = result.title.trim();
    if title.is_empty() {
        return;
    }
    let snippet = result.snippet.clone().unwrap_or_default();
    if !crate::models::is_applicable_posting(
        result
            .published_at
            .as_deref()
            .and_then(crate::models::parse_posted_at),
        &format!("{title} {snippet}"),
        &url,
        config,
    ) {
        return;
    }
    pending.push(PendingHit { result, url });
}

fn job_from_snippet(
    profile: &CandidateProfile,
    config: &crate::models::SearchConfig,
    result: SearchResult,
    url: &str,
    full_text: Option<&str>,
) -> Option<Job> {
    let title = result.title.trim();
    if title.is_empty() {
        return None;
    }
    let snippet = result.snippet.unwrap_or_default();
    let body = full_text.unwrap_or(&snippet);
    // Quality gate 1: thin/empty postings never become jobs.
    let gates = quality_gates();
    if body.trim().chars().count() < gates.min_body_chars {
        return None;
    }
    let text = format!("{title} {body}");
    // Quality gate 0: business-directory listings are not jobs.
    if looks_like_business_listing(title, body) {
        return None;
    }
    let (role, company) = split_role_company(title, &role_words(profile));
    let company = company
        .unwrap_or_else(|| company_from_host(url).unwrap_or_else(|| "Unknown company".into()));
    // Quality gate 2: unidentifiable postings never become jobs.
    if gates.require_company && company == "Unknown company" {
        return None;
    }
    let published_at = result
        .published_at
        .as_deref()
        .and_then(crate::models::parse_posted_at);
    if !crate::models::is_applicable_posting(published_at, &text, url, config) {
        return None;
    }
    let (min_experience, max_experience) = experience_bounds(&text);
    let location = if config.targets_india() {
        crate::models::india_place(&text).map(str::to_string)
    } else {
        let country = config.target_country.trim();
        (!country.is_empty()).then(|| country.to_string())
    };
    // Quality gate 3: postings without a usable location never become jobs.
    if gates.require_location && location.is_none() {
        return None;
    }
    let skills = extract_skills(&text, profile);
    // Quality gate 4: postings naming none of the candidate's skills are
    // kept out — they are what "empty" cards mostly are.
    if gates.require_skills && skills.is_empty() {
        return None;
    }
    let work_mode = location
        .as_deref()
        .and_then(work_mode_from_text)
        .or_else(|| work_mode_from_text(&text));
    Some(Job {
        external_id: stable_id(url),
        source: result.source.to_string(),
        title: role,
        company,
        location,
        description: super::clip(body, 8_000),
        skills,
        min_experience,
        max_experience,
        url: url.to_string(),
        discovered_at: Utc::now(),
        published_at,
        work_mode,
    })
}

/// Posting quality gates: thin bodies, missing company/location, and
/// zero skill overlap are what "empty" job cards mostly are.
/// Tune with `SEARCH_MIN_BODY_CHARS` (default 120 — Glassdoor/Google Jobs
/// snippets are short but trustworthy, business listings are caught by the
/// phone/trade-word gate instead),
/// `SEARCH_REQUIRE_COMPANY` / `SEARCH_REQUIRE_LOCATION` /
/// `SEARCH_REQUIRE_SKILLS` (default true; `0`/`false`/`no` disables).
#[derive(Debug, Clone, Copy)]
pub(crate) struct QualityGates {
    pub min_body_chars: usize,
    pub require_company: bool,
    pub require_location: bool,
    pub require_skills: bool,
}

pub(crate) fn quality_gates() -> QualityGates {
    fn flag(name: &str, fallback: bool) -> bool {
        match std::env::var(name)
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
        {
            Some(value) if ["1", "true", "yes", "on"].contains(&value.as_str()) => true,
            Some(value) if ["0", "false", "no", "off"].contains(&value.as_str()) => false,
            _ => fallback,
        }
    }
    QualityGates {
        min_body_chars: std::env::var("SEARCH_MIN_BODY_CHARS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(120),
        require_company: flag("SEARCH_REQUIRE_COMPANY", true),
        require_location: flag("SEARCH_REQUIRE_LOCATION", true),
        require_skills: flag("SEARCH_REQUIRE_SKILLS", true),
    }
}

/// Skills named anywhere in the full posting text, matched against the
/// candidate's preference skills. Previously this field was always empty.
fn extract_skills(text: &str, profile: &CandidateProfile) -> Vec<String> {
    crate::providers::skills_in_text(text, &profile.skills)
}

fn allowed_result_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.trim_start_matches("www.").to_ascii_lowercase();
    !super::EXCLUDED_DOMAINS
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn work_mode_from_text(value: &str) -> Option<String> {
    let lower = value.to_lowercase();
    if lower.contains("remote") {
        Some("Remote".into())
    } else if lower.contains("hybrid") {
        Some("Hybrid".into())
    } else {
        None
    }
}

pub(crate) fn looks_like_business_listing(title: &str, body: &str) -> bool {
    // Business-directory pages (exporters, manufacturers, local shops) slip
    // in via web search: several phone numbers, trade words, and no hiring
    // signal at all. Real postings name a role and usually state experience.
    let text = format!("{title} {body}");
    let phones = BUSINESS_PHONE
        .get_or_init(|| {
            regex::Regex::new(r"\+?\d[\d\s\-()]{7,}\d").expect("phone pattern")
        })
        .find_iter(&text)
        .count();
    if phones < 2 {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let trade_word = [
        "manufacturer",
        "manufacturers",
        "exporter",
        "exporters",
        "exports",
        "importer",
        "supplier",
        "wholesaler",
        "distributor",
        "trader",
        "seafood",
        "call us",
        "contact us",
        "visit us",
    ]
    .iter()
    .any(|word| lower.contains(word));
    trade_word && experience_bounds(&text) == (None, None)
}

static BUSINESS_PHONE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

fn experience_bounds(text: &str) -> (Option<f32>, Option<f32>) {
    let range = regex::Regex::new(
        r"(?i)(\d+(?:\.\d+)?)\s*(?:-|to)\s*(\d+(?:\.\d+)?)\s*(?:\+\s*)?(?:years?|yrs?)",
    )
    .ok();
    if let Some(captures) = range.and_then(|re| re.captures(text)) {
        return (
            captures.get(1).and_then(|m| m.as_str().parse().ok()),
            captures.get(2).and_then(|m| m.as_str().parse().ok()),
        );
    }
    let one = regex::Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*\+?\s*(?:years?|yrs?)").ok();
    (
        one.and_then(|re| re.captures(text))
            .and_then(|captures| captures.get(1)?.as_str().parse().ok()),
        None,
    )
}

fn stable_id(url: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in normalize_url(url).bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("search-{hash:x}")
}

/// Canonical posting URL: scheme, `www.`, fragments, and tracking query
/// params (`utm_*`, `fbclid`, …) are dropped so the same posting found via
/// several queries or engines maps to one `external_id` instead of showing
/// up as duplicate cards. Other query params are kept — some boards encode
/// the job id in them.
fn normalize_url(url: &str) -> String {
    let trimmed = url.trim();
    let Ok(mut parsed) = reqwest::Url::parse(trimmed) else {
        return trimmed.trim_end_matches('/').to_ascii_lowercase();
    };
    parsed.set_fragment(None);
    let host = parsed
        .host_str()
        .unwrap_or_default()
        .trim_start_matches("www.")
        .to_ascii_lowercase();
    if host.is_empty() {
        return trimmed.trim_end_matches('/').to_ascii_lowercase();
    }
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.to_ascii_lowercase();
            !(key == "fbclid"
                || key == "gclid"
                || key == "gbraid"
                || key == "wbraid"
                || key == "msclkid"
                || key == "dclid"
                || key == "yclid"
                || key == "srsltid"
                || key == "ref"
                || key == "ref_"
                || key == "source"
                || key == "_ga"
                || key == "_gl"
                || key == "mc_cid"
                || key == "mc_eid"
                || key.starts_with("utm_"))
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let mut out = format!(
        "{}{}",
        host,
        parsed.path().trim_end_matches('/').to_ascii_lowercase()
    );
    if !pairs.is_empty() {
        let query = pairs
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("&");
        out.push('?');
        out.push_str(&query.to_ascii_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> CandidateProfile {
        CandidateProfile {
            name: "Candidate".into(),
            summary: String::new(),
            skills: vec!["Rust".into(), "Python".into()],
            experience_years: 4.0,
            preferred_titles: vec!["Machine Learning Engineer".into()],
            role_keywords: vec!["fellow".into()],
            preferred_locations: vec!["Remote".into(), "Bengaluru".into()],
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
    fn builds_a_job_query_from_preferences() {
        let queries = job_queries(&profile(), &crate::models::SearchConfig::default());
        assert!(queries.iter().any(|query| {
            query.contains("Machine Learning Engineer") && query.contains("Bengaluru")
        }));
        assert!(queries.iter().any(|query| {
            query.contains("Machine Learning Engineer")
                && query.to_ascii_lowercase().contains("india remote")
        }));
        assert!(queries.iter().any(|query| query.contains("Rust engineer")));
        assert!(queries
            .iter()
            .all(|query| query.to_ascii_lowercase().contains("india")));
        assert!(queries.len() > 1);
    }

    #[test]
    fn search_starts_at_the_saved_city_then_nearby() {
        let config = crate::models::SearchConfig {
            location: "Chennai".into(),
            nearby: vec!["Bengaluru".into(), "Coimbatore".into()],
            ..crate::models::SearchConfig::default()
        };
        let queries = job_queries(&profile(), &config);
        assert!(queries[0].contains("Chennai"));
        assert!(queries.iter().any(|query| query.contains("Bengaluru")));
    }

    #[test]
    fn extra_cities_remote_and_hybrid_expand_queries() {
        let config = crate::models::SearchConfig {
            location: "Chennai".into(),
            extra_locations: vec!["Pune".into()],
            ..crate::models::SearchConfig::default()
        };
        let queries = job_queries(&profile(), &config);
        assert!(queries.iter().any(|query| query.contains("Pune")));
        assert!(queries
            .iter()
            .any(|query| query.to_ascii_lowercase().contains("remote")));
        assert!(queries
            .iter()
            .any(|query| query.to_ascii_lowercase().contains("hybrid")));
    }

    #[test]
    fn remote_and_hybrid_can_be_turned_off() {
        let config = crate::models::SearchConfig {
            location: "Chennai".into(),
            include_remote: false,
            include_hybrid: false,
            ..crate::models::SearchConfig::default()
        };
        let queries = job_queries(&profile(), &config);
        assert!(!queries.is_empty());
        assert!(queries
            .iter()
            .all(|query| !query.to_ascii_lowercase().contains("remote")));
        assert!(queries
            .iter()
            .all(|query| !query.to_ascii_lowercase().contains("hybrid")));
    }

    #[test]
    fn rejects_linkedin_and_keeps_public_https() {
        assert!(!allowed_result_url("https://www.linkedin.com/jobs/view/1"));
        assert!(!allowed_result_url("https://www.naukri.com/job-listings-1"));
        assert!(allowed_result_url(
            "https://boards.greenhouse.io/example/jobs/123"
        ));
        assert!(!allowed_result_url("javascript:alert(1)"));
    }

    #[test]
    fn reads_experience_range() {
        assert_eq!(
            experience_bounds("3-5 years of experience"),
            (Some(3.0), Some(5.0))
        );
    }

    #[test]
    fn extracts_skills_from_full_text() {
        let skills = extract_skills(
            "Senior Rust Engineer. Must know Rust, Kubernetes, and Go. 3-5 years of experience in Bengaluru.",
            &profile(),
        );
        assert!(skills.contains(&"Rust".to_string()));
        assert!(!skills.contains(&"Python".to_string()));
    }

    #[test]
    fn job_prefers_enriched_text_over_snippet() {
        let published = (chrono::Utc::now() - chrono::Duration::days(2)).to_rfc3339();
        let result = SearchResult {
            title: "Senior Rust Engineer".into(),
            url: "https://example.com/jobs/1".into(),
            snippet: Some("Hiring in Bengaluru.".into()),
            published_at: Some(published),
            source: "exa",
        };
        let config = crate::models::SearchConfig::default();
        let job = job_from_snippet(
            &profile(),
            &config,
            result,
            "https://example.com/jobs/1",
            Some("Build Rust and Kubernetes services in Bengaluru for a high-scale payments platform. You will own backend services end to end, mentor engineers, and drive reliability. 3-5 years of experience with Rust, Kubernetes, and Postgres. Hybrid role with two office days per week."),
        )
        .expect("applicable posting");
        assert!(job.description.contains("Kubernetes"));
        assert!(job.skills.contains(&"Rust".to_string()));
        assert_eq!(job.min_experience, Some(3.0));
        assert_eq!(job.max_experience, Some(5.0));
        assert_eq!(job.work_mode.as_deref(), Some("Hybrid"));
    }

    #[test]
    fn canonical_url_dedupes_tracking_and_scheme() {
        assert_eq!(
            normalize_url("https://Example.com/Jobs/1?utm_source=x#frag"),
            normalize_url("http://example.com/jobs/1")
        );
        assert_eq!(
            normalize_url("https://www.example.com/jobs/1?fbclid=abc"),
            "example.com/jobs/1"
        );
        assert_ne!(
            normalize_url("https://example.com/j?jobId=1"),
            normalize_url("https://example.com/j?jobId=2")
        );
    }

    #[test]
    fn business_listings_are_not_jobs() {
        assert!(looks_like_business_listing(
            "K V Marine Exports in Chennai",
            "Call +919884718389, +919884718390. The best choice for quality seafood in Chennai. Contact us at our Chennai address.",
        ));
        assert!(!looks_like_business_listing(
            "Senior Rust Engineer",
            "Build Rust services in Bengaluru. 3-5 years of experience. Apply with your resume. Call +919884718389 for queries.",
        ));
    }

    #[test]
    fn title_queries_carry_the_tech_stack() {        let queries = job_queries(&profile(), &crate::models::SearchConfig::default());
        // Stack-aware: every title query names the candidate's skills, so the
        // API returns that stack instead of whatever is popular.
        assert!(queries.iter().any(|query| {
            query.contains("Machine Learning Engineer")
                && query.contains("Rust")
                && query.contains("Bengaluru")
        }));
    }

    #[tokio::test]
    async fn saved_search_key_is_encrypted_and_masked() {
        crate::secrets::set_test_master_key([7u8; 32]);
        let dir = std::env::temp_dir().join(format!(
            "rust-job-agent-keys-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("search.json");
        // Legacy plaintext file: picked up until the DB holds a sealed copy.
        std::fs::write(
            &path,
            r#"{"serpapi_api_key":"serp-legacy-1","tavily_api_key":"","brave_api_key":""}"#,
        )
        .unwrap();
        let db_path = dir.join("keys.db");
        let url = format!(
            "sqlite://{}?mode=rwc",
            db_path.to_string_lossy().replace('\\', "/")
        );
        let db = crate::store::connect(&url).await.unwrap();
        crate::store::migrate(&db).await.unwrap();

        let status = search_key_status(&db, &path).await;
        assert!(status.serpapi_configured);
        assert_eq!(status.serpapi_hint.as_deref(), Some("••••••••cy-1"));
        assert!(!status.tavily_configured);

        // Saving seals new keys and wipes the legacy plaintext.
        save_search_keys(&db, &path, None, None, Some("tavily-test-key"), None)
            .await
            .unwrap();
        let status = search_key_status(&db, &path).await;
        assert!(status.serpapi_configured && status.tavily_configured && !status.brave_configured);
        assert_eq!(status.tavily_hint.as_deref(), Some("••••••••-key"));
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(
            !stored.contains("serp-legacy-1"),
            "plaintext must be wiped: {stored}"
        );
        assert!(!stored.contains("tavily-test-key"));

        // Blank saves keep what is stored; removal forgets.
        save_search_keys(&db, &path, None, None, None, None)
            .await
            .unwrap();
        assert!(search_key_status(&db, &path).await.tavily_configured);
        remove_search_keys(&db, false, false, true, false).await.unwrap();
        let status = search_key_status(&db, &path).await;
        assert!(!status.tavily_configured);
        assert!(status.tavily_hint.is_none());
        assert!(status.serpapi_configured);

        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
