mod apply;
mod discovery;
mod email;
mod inference;
mod knowledge;
mod llm;
mod matching;
mod models;
mod prep;
mod providers;
mod resume;
mod resume_style;
mod secrets;
mod store;
mod study;
mod tailored;
mod web;
use discovery::provider::SearchJobProvider;
use models::CandidateProfile;
use providers::{
    ArbeitnowProvider, GreenhouseProvider, JobProvider, JsonImportProvider, PublicJobPageProvider,
    RemotiveProvider,
};
use sqlx::SqlitePool;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tower_http::{
    services::ServeDir,
    trace::TraceLayer,
};
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub profile: Arc<RwLock<Option<resume::CandidateProfile>>>,
    pub preferences: Arc<RwLock<Option<CandidateProfile>>>,
    pub upload_dir: PathBuf,
    pub data_dir: PathBuf,
    pub providers: Arc<Vec<Arc<dyn JobProvider>>>,
    #[cfg(feature = "onnx")]
    pub embedder: Option<Arc<inference::onnx::OnnxEmbedder>>,
}

pub async fn discovery_once(
    state: &AppState,
    fix: Option<discovery::location::GeoFix>,
) -> anyhow::Result<usize> {
    let run_id = store::run_started(&state.db).await?;
    let mut total = 0usize;
    let result: anyhow::Result<()> = async {
        let prefs = state.preferences.read().await.clone();
        let Some(profile) = prefs else { anyhow::bail!("Configure matching preferences before discovery") };
        let saved = store::load_search_config(&state.db).await?;
        let config = match discovery::location::refresh(&saved, fix).await {
            Ok(detected) => store::save_search_config(&state.db, &detected).await?,
            Err(err) => {
                tracing::warn!(error = %err, "location detection failed; using the saved location");
                saved
            }
        };
        tracing::info!(
            location = %config.location,
            nearby = %config.nearby.join(", "),
            source = %config.location_source,
            "search starts near the saved location"
        );
        let mut provider_handles = Vec::new();
        for provider in state.providers.iter().cloned() {
            provider_handles.push(tokio::spawn(async move { provider.discover().await }));
        }
        let mut discovered = Vec::new();
        for handle in provider_handles {
            match handle.await {
                Ok(Ok(jobs)) => discovered.extend(jobs),
                Ok(Err(err)) => tracing::warn!(error = %err, "discovery provider failed"),
                Err(err) => tracing::warn!(error = %err, "discovery provider task failed"),
            }
        }
        discovered.retain(|job| {
            models::is_applicable_posting(
                job.published_at,
                &format!(
                    "{} {} {}",
                    job.title,
                    job.location.as_deref().unwrap_or(""),
                    job.description
                ),
                &job.url,
                &config,
            )
        });
        let experience = matching::experience_text(state.profile.read().await.as_ref());
        let resume_guard = state.profile.read().await;
        let refs: Vec<&models::Job> = discovered.iter().collect();
        let ranked = matching::rank_jobs_with_resume(&profile, resume_guard.as_ref(), &refs, &experience);
        let mut by_position = vec![None; discovered.len()];
        for item in ranked {
            let position = item.position;
            by_position[position] = Some(item);
        }
        let mut scored = Vec::with_capacity(discovered.len());
        for (position, job) in discovered.into_iter().enumerate() {
                let item = by_position[position].take().expect("every job is ranked");
                let base = item.breakdown;
                let matched = item.matched.clone();
                let missing = item.missing.clone();
                let mut score = base.total;
                let mut explanation = format!("Rule score — skills {:.0}, experience {:.0}, title {:.0}, location {:.0}.",base.skill,base.experience,base.title,base.location);
                let mut semantic_score: Option<f32> = None;
                let mut llm_score: Option<f32> = None;
                #[cfg(feature = "onnx")]
                if let Some(embedder) = &state.embedder {
                    if let (Ok(a),Ok(b))=(embedder.embed(&profile.semantic_document()),embedder.embed(&job.semantic_document())) {
                        if let Some(sim)=matching::cosine(&a,&b) { let sem=((sim+1.0)*50.0).clamp(0.,100.); semantic_score=Some(sem); score=score*0.65+sem*0.35; explanation.push_str(&format!(" Embedding similarity {:.0}/100.",sem)); }
                    }
                }
                if let (Ok(base_url),Ok(model))=(std::env::var("LLM_BASE_URL"),std::env::var("LLM_MODEL")) {
                    if !base.excluded && score >= std::env::var("LLM_RERANK_THRESHOLD").ok().and_then(|v|v.parse().ok()).unwrap_or(55.0) {
                        let client=reqwest::Client::new();
                        let prompt=format!("Evaluate candidate/job fit conservatively. Return JSON {{\"score\":0-100,\"reason\":\"...\"}}. Do not invent qualifications. Candidate: {}. Job: {}",profile.semantic_document(),job.semantic_document());
                        let body=serde_json::json!({"model":model,"temperature":0,"response_format":{"type":"json_object"},"messages":[{"role":"user","content":prompt}]});
                        let mut req=client.post(format!("{}/v1/chat/completions",base_url.trim_end_matches('/'))).json(&body);
                        if let Ok(k)=std::env::var("LLM_API_KEY"){req=req.bearer_auth(k);}
                        if let Ok(resp) = req.send().await {
                            if let Ok(resp) = resp.error_for_status() {
                                if let Ok(value) = resp.json::<serde_json::Value>().await {
                                    if let Some(content) = value["choices"][0]["message"]["content"].as_str() {
                                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) {
                                            if let Some(raw_score) = parsed["score"].as_f64() {
                                                let rerank = raw_score.clamp(0.0, 100.0) as f32;
                                                llm_score = Some(rerank);
                                                score = score * 0.6 + rerank * 0.4;
                                                explanation.push_str(&format!(" LLM rerank {:.0}/100: {}", rerank, parsed["reason"].as_str().unwrap_or("")));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if base.excluded { score=0.; }
                let status=if score>=profile.minimum_match_score{"shortlisted"}else{"discovered"}.to_string();
                let rank = item.rank;
                explanation = format!("Rank {rank}. {explanation}");
                scored.push(models::JobResult{job,score,deterministic_score:base.total,semantic_score,llm_score,matched_skills:matched,missing_skills:missing,explanation,status,rank});
        }
        scored.sort_by_key(|result| result.rank);
        total += scored.len();
        store::upsert_jobs(&state.db, &scored).await?;
        // Drop empty/low-quality postings (thin bodies, unknown company,
        // missing location, zero skill overlap, excluded stacks) — including
        // ones saved by earlier runs, so the dashboard clears up by itself.
        let gates = discovery::provider::quality_gates();
        match store::purge_low_quality_jobs(
            &state.db,
            gates.min_body_chars,
            gates.require_company,
            gates.require_location,
            gates.require_skills,
        )
        .await
        {
            Ok(removed) if removed > 0 => {
                tracing::info!(removed, "low-quality jobs purged")
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(error = %error, "low-quality job purge failed"),
        }
        match store::jobs(&state.db).await {
            Ok(all) => {
                let stale: Vec<String> = all
                    .iter()
                    .filter(|result| {
                        crate::discovery::provider::looks_like_business_listing(
                            &result.job.title,
                            &result.job.description,
                        )
                    })
                    .map(|result| result.job.external_id.clone())
                    .collect();
                if !stale.is_empty() {
                    match store::soft_delete_jobs(&state.db, &stale).await {
                        Ok(hidden) => tracing::info!(hidden, "business listings hidden"),
                        Err(error) => {
                            tracing::warn!(error = %error, "business-listing purge failed")
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, "stored jobs could not be scanned for listings")
            }
        }
        if profile.auto_apply {
            let report = apply::auto_apply(state, &profile).await?;
            tracing::info!(
                drafted = report.drafted,
                submitted = report.submitted,
                needs_manual = report.needs_manual,
                skipped = report.skipped,
                "auto-apply finished"
            );
        }
        Ok(())
    }.await;
    store::run_finished(
        &state.db,
        run_id,
        total,
        result.as_ref().err().map(|e| e.to_string()),
    )
    .await?;
    result?;
    Ok(total)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    tokio::fs::create_dir_all("uploads").await?;
    tokio::fs::create_dir_all("data").await?;
    tokio::fs::create_dir_all("models").await?;
    let db = store::connect("sqlite://data/job_agent.db?mode=rwc").await?;
    store::migrate(&db).await?;
    // Resume profile and preferences live in SQLite as JSON; legacy loose
    // files are imported once and then ignored.
    store::import_legacy_json_files(&db).await?;
    let profile = store::load_candidate_profile(&db).await?;
    let preferences = store::load_preferences(&db).await?;
    let providers: Vec<Arc<dyn JobProvider>> = vec![
        Arc::new(JsonImportProvider::new("data/jobs.json")),
        Arc::new(PublicJobPageProvider::new("data/job_sources.json")),
        Arc::new(GreenhouseProvider::new(
            "data/greenhouse_boards.json",
            "data/job_sources.json",
            db.clone(),
        )),
        Arc::new(SearchJobProvider::from_env(
            "data/preferences.json",
            db.clone(),
        )),
        // Keyless and always on: official free JSON APIs, no signup needed.
        Arc::new(RemotiveProvider::new(db.clone())),
        Arc::new(ArbeitnowProvider::new(db.clone())),
    ];
    #[cfg(feature = "onnx")]
    let embedder = match (
        std::env::var("EMBEDDING_MODEL"),
        std::env::var("TOKENIZER_JSON"),
    ) {
        (Ok(m), Ok(t)) => inference::onnx::OnnxEmbedder::load(m, t, 256)
            .ok()
            .map(Arc::new),
        _ => None,
    };
    let state = AppState {
        db,
        profile: Arc::new(RwLock::new(profile)),
        preferences: Arc::new(RwLock::new(preferences)),
        upload_dir: PathBuf::from("uploads"),
        data_dir: PathBuf::from("data"),
        providers: Arc::new(providers),
        #[cfg(feature = "onnx")]
        embedder,
    };
    let cleanup_db = state.db.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(7 * 24 * 60 * 60));
        loop {
            ticker.tick().await;
            match store::purge_expired_search_pages(&cleanup_db).await {
                Ok(removed) => tracing::info!(removed, "weekly search-page cleanup finished"),
                Err(err) => tracing::warn!(error = %err, "weekly search-page cleanup failed"),
            }
        }
    });
    let scheduler_state = state.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(4 * 60 * 60));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match discovery_once(&scheduler_state, None).await {
                Ok(n) => tracing::info!(discovered = n, "scheduled discovery completed"),
                Err(e) => tracing::warn!(error=%e,"scheduled discovery failed"),
            }
        }
    });
    let app = web::router(state)
        .nest_service("/static", ServeDir::new("static"))
        .fallback(web::spa_fallback)
        .layer(TraceLayer::new_for_http());
    let addr = "127.0.0.1:8787";
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Dashboard: http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
