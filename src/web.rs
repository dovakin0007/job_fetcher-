use crate::{
    discovery_once, llm, matching, prep, resume, resume_style, store, study, tailored, AppState,
};
use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderValue, Method, StatusCode, Uri},
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::Deserialize;
use std::path::PathBuf;
use tower_http::cors::{AllowOrigin, CorsLayer};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/extension/download", get(extension_download))
        .route("/api/resume", get(get_resume).put(save_resume))
        .route("/api/resume/upload", post(upload_resume))
        .route("/api/resume/file", get(resume_file))
        .route("/api/resume/assist", post(assist_resume))
        .route("/api/resume/suggestions", post(resume_suggestions))
        .route("/api/resume/export", get(export_resume))
        .route("/api/resume/styles", get(resume_styles))
        .route("/api/resume/render", post(render_resume))
        .route("/api/resume/versions", get(resume_versions))
        .route(
            "/api/resume/versions/:version",
            get(resume_version).post(restore_resume_version),
        )
        .route("/api/resume/import", post(import_resume_markdown))
        .route(
            "/api/rope/:id",
            get(load_rope).put(save_rope).merge(delete(forget_rope)),
        )
        .route("/api/jobs", get(get_jobs))
        .route("/api/jobs/search", get(search_jobs))
        .route("/api/discovery/run", post(run_discovery))
        .route(
            "/api/discovery/search-config",
            get(get_search_config).put(save_search_config),
        )
        .route("/api/discovery/status", get(discovery_status))
        .route("/api/discovery/search", post(save_search))
        .route("/api/discovery/cache/clear", post(clear_search_cache))
        .route("/api/discovery/greenhouse", post(add_greenhouse_board))
        .route("/api/discovery/sources", get(get_sources).post(add_source))
        .route(
            "/api/preferences",
            get(get_preferences).put(save_preferences),
        )
        .route(
            "/api/preferences/suggested-weights",
            post(suggested_weights),
        )
        .route("/api/jobs/:id/tailor", post(tailor_job))
        .route("/api/jobs/:id/tailored-resume", post(tailored_resume_job))
        .route("/api/jobs/:id/tailored-resume/pdf", post(tailored_resume_pdf))
        .route("/api/jobs/:id/interview-prep", post(interview_prep_job))
        .route("/api/companies", get(list_companies))
        .route(
            "/api/companies/:name/guide",
            get(company_guide).post(rebuild_company_guide),
        )
        .route("/api/companies/:name/notes", put(save_company_notes))
        .route(
            "/api/applications",
            get(get_applications).post(create_application),
        )
        .route("/api/applications/auto", post(auto_apply_now))
        .route("/api/applications/sync-email", post(sync_email_outcomes))
        .route("/api/applications/report-applied", post(report_applied))
        .route("/api/email/status", get(email_status))
        .route("/api/email/oauth/start", get(email_oauth_start))
        .route("/api/email/oauth/callback", get(email_oauth_callback))
        .route("/api/email/disconnect", post(email_disconnect))
        .route("/api/autofill", get(autofill))
        .route(
            "/api/applications/:id/status",
            post(update_application_status),
        )
        .layer(extension_cors())
        .with_state(state)
}

/// SPA fallback: known files under `web/dist` are served with a content
/// type; every other non-API path serves `index.html` with 200 so the
/// client-side router (`/jobs?page=2`, `/applications`, …) boots on
/// direct navigation and reload instead of leaking a 404 status.
pub async fn spa_fallback(uri: Uri) -> impl IntoResponse {
    fn mime_for(name: &str) -> &'static str {
        match name.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
            "js" => "text/javascript",
            "css" => "text/css",
            "html" => "text/html",
            "json" | "map" => "application/json",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "ico" => "image/x-icon",
            "woff2" => "font/woff2",
            _ => "application/octet-stream",
        }
    }
    let path = uri.path().trim_start_matches('/').trim_end_matches('/');
    if !path.is_empty() && !path.contains("..") {
        let file = std::path::PathBuf::from("web/dist").join(path);
        if file.is_file() {
            let mime = mime_for(path);
            if let Ok(bytes) = tokio::fs::read(&file).await {
                return ([(header::CONTENT_TYPE, mime)], bytes).into_response();
            }
        }
    }
    match tokio::fs::read("web/dist/index.html").await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "text/html")], bytes).into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            "Dashboard build not found. Run `npm run build` in web/ once.",
        )
            .into_response(),
    }
}

fn extension_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PUT])
        .allow_headers([header::CONTENT_TYPE])
        .allow_origin(AllowOrigin::predicate(|origin: &HeaderValue, _| {
            let Ok(origin) = origin.to_str() else {
                return false;
            };
            origin == "http://127.0.0.1:5173"
                || origin == "http://127.0.0.1:8787"
                || origin.starts_with("chrome-extension://")
        }))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok":true,"service":"rust-job-agent"}))
}
async fn upload_resume(
    State(s): State<AppState>,
    mut mp: Multipart,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut upload: Option<(String, Vec<u8>)> = None;
    while let Some(field) = mp.next_field().await.map_err(bad)? {
        if field.name() == Some("resume") {
            let filename = field.file_name().unwrap_or("resume.pdf").to_string();
            let bytes = field.bytes().await.map_err(bad)?.to_vec();
            upload = Some((filename, bytes));
            break;
        }
    }
    let (filename, bytes) =
        upload.ok_or((StatusCode::BAD_REQUEST, "Missing resume field".into()))?;
    if bytes.len() > 10 * 1024 * 1024 {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "Resume must be under 10 MB".into(),
        ));
    }
    let ext = std::path::Path::new(&filename)
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !["pdf", "docx"].contains(&ext.as_str()) {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Only PDF and DOCX resumes are supported. Scanned PDF OCR is not enabled yet.".into(),
        ));
    }
    let stored_name = format!("resume.{ext}");
    let path = s.upload_dir.join(&stored_name);
    tokio::fs::write(&path, &bytes).await.map_err(internal)?;
    let extraction_path = path.clone();
    let text = tokio::task::spawn_blocking(move || resume::extract_text(&extraction_path))
        .await
        .map_err(internal)?
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let clean = resume::preprocess(&text);
    let mut profile = match resume::extract_profile(&clean).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(%e,"resume LLM extraction failed; using conservative fallback");
            let mut p = resume::fallback_profile(&clean);
            p.extraction_warnings
                .push(format!("LLM extraction unavailable: {e}"));
            p
        }
    };
    resume::validate_profile(&mut profile, &clean);
    profile.source_file = Some(format!("uploads/{stored_name}"));
    store::save_candidate_profile(&s.db, &profile)
        .await
        .map_err(internal)?;
    *s.profile.write().await = Some(profile.clone());
    Ok(Json(
        serde_json::json!({"ok":true,"source_file":profile.source_file,"format":ext,"name":profile.name,"skills":profile.skills.len(),"experience":profile.experience.len(),"projects":profile.projects.len(),"education":profile.education.len(),"open_source":profile.open_source.len(),"warnings":profile.extraction_warnings,"llm_configured":profile.extraction_warnings.iter().all(|w|!w.starts_with("LLM extraction unavailable:"))}),
    ))
}
async fn get_resume(State(s): State<AppState>) -> Json<serde_json::Value> {
    match s.profile.read().await.as_ref() {
        Some(p) => {
            let mut value = serde_json::to_value(p).unwrap_or_default();
            if let Some(object) = value.as_object_mut() {
                // The frontend uses this flag to distinguish an empty state from
                // a valid profile. Previously it was omitted for existing profiles.
                object.insert("uploaded".to_owned(), serde_json::Value::Bool(true));
            }
            Json(value)
        }
        None => Json(serde_json::json!({"uploaded":false})),
    }
}

/// Raw resume bytes for the browser extension so it can attach the file to
/// application forms (`GET /api/resume/file`). Same loopback trust as the
/// autofill payload — no separate auth.
async fn resume_file(
    State(s): State<AppState>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let guard = s.profile.read().await;
    let profile = guard.as_ref().ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Upload a resume first".to_string(),
    ))?;
    let stored = profile.source_file.as_deref().ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Upload a resume first".to_string(),
    ))?;
    let name = stored
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(stored)
        .to_string();
    if name.trim().is_empty() || name.contains("..") {
        return Err((StatusCode::BAD_REQUEST, "Invalid resume path".into()));
    }
    let bytes = tokio::fs::read(s.upload_dir.join(&name))
        .await
        .map_err(internal)?;
    let mime = if name.to_ascii_lowercase().ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    };
    axum::http::Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        )
        .body(axum::body::Body::from(bytes))
        .map_err(internal)
}

/// Resume builder save: normalises the edited profile, persists it to
/// `candidate_profile.json`, and refreshes matching state. The builder is the
/// source of truth here, so no source-text validation runs.
async fn save_resume(
    State(s): State<AppState>,
    Json(profile): Json<crate::resume::CandidateProfile>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let clean = resume::sanitize_builder_profile(profile);
    if clean
        .name
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
        && clean.skills.is_empty()
        && clean.experience.is_empty()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Resume needs a name, a skill, or an experience entry".into(),
        ));
    }
    store::save_candidate_profile(&s.db, &clean)
        .await
        .map_err(internal)?;
    *s.profile.write().await = Some(clean.clone());
    Ok(Json(serde_json::json!({"ok": true, "profile": clean})))
}

#[derive(Deserialize)]
struct AssistRequest {
    task: String,
    #[serde(default)]
    bullet: String,
    #[serde(default)]
    skill: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    count: usize,
    /// Free-form instruction for the AI editor.
    #[serde(default)]
    instruction: String,
    /// Unsaved builder draft. When present, edits are grounded against this
    /// copy instead of the last saved profile.
    #[serde(default)]
    profile: Option<crate::resume::CandidateProfile>,
}

/// AI writing help for the builder. `improve_bullet` rewrites one bullet with
/// the same facts (groundedness-checked); `generate_summary` drafts a summary
/// from the resume; `suggest_bullets` drafts STAR-style bullets for a skill,
/// clearly marked generated — the user must review before saving.
async fn assist_resume(
    State(s): State<AppState>,
    Json(req): Json<AssistRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let guard = s.profile.read().await;
    let resume = guard.as_ref().ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Upload a resume or start a builder draft first".into(),
    ))?;
    let task = req.task.trim().to_ascii_lowercase();
    match task.as_str() {
        "improve_bullet" => {
            let bullet = req.bullet.trim();
            if bullet.is_empty() {
                return Err(bad("Provide the bullet to improve"));
            }
            let system = "Rewrite ONE resume bullet to be stronger and more specific. Hard rules:\n\
                1. Keep every fact, number, and technology from the original. Never add new claims.\n\
                2. Start with a strong action verb. Keep it to one or two sentences.\n\
                3. Return valid JSON only: {\"bullet\": \"...\"}.";
            let user = format!("Rewrite this resume bullet:\n\n{bullet}");
            match llm::chat_json(&system, &user).await {
                Ok((provider, value)) => {
                    let rewritten = value
                        .get("bullet")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim();
                    if rewritten.len() < 8
                        || rewritten.len() > 800
                        || !resume::rewrite_is_grounded(rewritten, bullet)
                    {
                        return Ok(Json(serde_json::json!({
                            "ok": true, "bullet": bullet, "llm_used": false,
                            "note": "The rewrite drifted from your facts, so the original is kept.",
                        })));
                    }
                    Ok(Json(serde_json::json!({
                        "ok": true, "bullet": rewritten, "llm_used": true,
                        "provider": provider.name(),
                    })))
                }
                Err(error) => Ok(Json(serde_json::json!({
                    "ok": true, "bullet": bullet, "llm_used": false,
                    "note": format!("No LLM available ({error}); original kept."),
                }))),
            }
        }
        "generate_summary" => {
            let context = matching::experience_text(Some(resume));
            let top_skills: Vec<&str> = resume
                .skills
                .iter()
                .take(10)
                .map(|sk| sk.name.as_str())
                .collect();
            let current = resume
                .experience
                .first()
                .map(|r| format!("{} at {}", r.title, r.company));
            let deterministic = {
                let who = current.clone().unwrap_or_else(|| {
                    resume
                        .headline
                        .clone()
                        .unwrap_or_else(|| "Software engineer".into())
                });
                if top_skills.is_empty() {
                    format!("{who}.")
                } else {
                    format!("{who}. Skilled in {}.", top_skills.join(", "))
                }
            };
            let system = "Write a 2-3 sentence resume summary. Hard rules:\n\
                1. Use ONLY the supplied resume facts. Never invent experience, metrics, or skills.\n\
                2. Return valid JSON only: {\"summary\": \"...\"}.";
            let user = format!(
                "Resume headline: {}\nTop skills: {}\nCurrent role: {}\nExperience text:\n{}",
                resume.headline.clone().unwrap_or_default(),
                top_skills.join(", "),
                current.unwrap_or_default(),
                context.chars().take(6_000).collect::<String>(),
            );
            match llm::chat_json(&system, &user).await {
                Ok((provider, value)) => {
                    let summary = value
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim();
                    if summary.len() < 20 || summary.len() > 1_200 {
                        return Ok(Json(serde_json::json!({
                            "ok": true, "summary": deterministic, "llm_used": false,
                            "note": "The draft was unusable, so a factual fallback is returned.",
                        })));
                    }
                    Ok(Json(serde_json::json!({
                        "ok": true, "summary": summary, "llm_used": true,
                        "provider": provider.name(),
                    })))
                }
                Err(error) => Ok(Json(serde_json::json!({
                    "ok": true, "summary": deterministic, "llm_used": false,
                    "note": format!("No LLM available ({error}); factual fallback returned."),
                }))),
            }
        }
        "suggest_bullets" => {
            let skill = req.skill.trim();
            if skill.is_empty() {
                return Err(bad("Provide the skill to draft bullets for"));
            }
            let count = req.count.clamp(1, 5);
            let system = "Draft STAR-style resume bullets for ONE skill, grounded in the candidate's own resume. Hard rules:\n\
                1. Every bullet must be plausibly backed by the supplied resume; do not invent employers, metrics, or projects.\n\
                2. Mark anything uncertain with [verify].\n\
                3. Return valid JSON only: {\"bullets\": [\"...\"]}.";
            let user = format!(
                "Skill: {skill}\nRole context: {role}\nResume skills: {skills}\nResume bullets:\n{bullets}\n\nDraft {count} bullets. These are marked generated and must be reviewed.",
                skill = skill,
                role = req.role.trim(),
                skills = resume.skills.iter().take(20).map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "),
                bullets = resume.experience.iter().flat_map(|r| r.bullets.iter().map(|b| b.text.as_str())).take(30).collect::<Vec<_>>().join("\n- "),
                count = count,
            );
            match llm::chat_json(&system, &user).await {
                Ok((provider, value)) => {
                    let items: Vec<String> = value
                        .get("bullets")
                        .and_then(|v| v.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|v| v.as_str())
                                .map(str::trim)
                                .filter(|s| s.len() >= 10 && s.len() <= 600)
                                .take(count)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    Ok(Json(serde_json::json!({
                        "ok": true, "bullets": items, "llm_used": true,
                        "provider": provider.name(),
                        "note": "Generated drafts — verify every claim before saving.",
                    })))
                }
                Err(error) => Ok(Json(serde_json::json!({
                    "ok": true, "bullets": Vec::<String>::new(), "llm_used": false,
                    "note": format!("No LLM available ({error}); no drafts generated."),
                }))),
            }
        }
        "edit" => {
            let source = req.profile.as_ref().unwrap_or(resume);
            let instruction = req.instruction.trim();
            if instruction.is_empty() {
                return Err(bad("Tell the editor what to change"));
            }
            let snapshot = editor_snapshot(source);
            let system = "You are a resume editor. Rewrite ONLY the headline, summary, and existing bullets. Hard rules:\n\
                1. Keep every employer, title, date, metric, and technology that is already written. Never add a new claim.\n\
                2. Do not add or remove roles. Refer to bullets by their role and index.\n\
                3. Follow the user's instruction (tone, length, verbs) without inventing facts.\n\
                4. Return valid JSON only: {\"headline\":\"...\",\"summary\":\"...\",\"bullets\":[{\"role\":0,\"index\":0,\"text\":\"...\"}]}.";
            let user = format!("Instruction:\n{instruction}\n\nCurrent resume:\n{snapshot}");
            match llm::chat_json(&system, &user).await {
                Ok((provider, value)) => {
                    let edit = resume::filter_editor_edit(source, &value);
                    let note = if edit.dropped > 0 {
                        format!("{} suggestion(s) were dropped because they added facts that are not on your resume.", edit.dropped)
                    } else if edit.headline.is_none() && edit.summary.is_none() && edit.bullets.is_empty() {
                        "The model returned nothing usable. Your resume is unchanged.".into()
                    } else {
                        "Review each change, then apply the ones you want.".into()
                    };
                    Ok(Json(serde_json::json!({
                        "ok": true,
                        "llm_used": true,
                        "provider": provider.name(),
                        "headline": edit.headline,
                        "summary": edit.summary,
                        "bullets": edit.bullets.iter().map(|b| serde_json::json!({
                            "role": b.role, "index": b.index, "text": b.text,
                        })).collect::<Vec<_>>(),
                        "dropped": edit.dropped,
                        "note": note,
                    })))
                }
                Err(error) => Ok(Json(serde_json::json!({
                    "ok": true,
                    "llm_used": false,
                    "headline": serde_json::Value::Null,
                    "summary": serde_json::Value::Null,
                    "bullets": Vec::<serde_json::Value>::new(),
                    "dropped": 0,
                    "note": format!("No LLM is configured ({error}). Set a provider in Settings, then try again."),
                }))),
            }
        }
        _ => Err(bad(
            "Unknown assist task. Use improve_bullet, generate_summary, suggest_bullets, or edit",
        )),
    }
}

fn editor_snapshot(profile: &crate::resume::CandidateProfile) -> String {
    let roles: Vec<serde_json::Value> = profile
        .experience
        .iter()
        .enumerate()
        .map(|(role, item)| {
            serde_json::json!({
                "role": role,
                "title": item.title,
                "company": item.company,
                "bullets": item.bullets.iter().enumerate().map(|(index, bullet)| serde_json::json!({
                    "index": index,
                    "text": bullet.text,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "headline": profile.headline,
        "summary": profile.summary,
        "skills": profile.skills.iter().map(|s| &s.name).collect::<Vec<_>>(),
        "experience": roles,
    })
    .to_string()
}

#[derive(Deserialize)]
struct ExportQuery {
    #[serde(default)]
    format: String,
    #[serde(default)]
    style: String,
    #[serde(default)]
    font: String,
    #[serde(default)]
    body_size: f32,
    #[serde(default)]
    heading_size: f32,
    #[serde(default)]
    font_weight: u16,
    #[serde(flatten)]
    titles: TitleFields,
}

fn typeface_from(font: &str, body: f32, heading: f32, weight: u16) -> resume_style::Typeface {
    let body = if body <= 0.0 { 10.5 } else { body };
    let heading = if heading <= 0.0 { 26.0 } else { heading };
    resume_style::Typeface::parse(font, body, heading, weight)
}

fn titles_from(req: &TitleFields) -> resume_style::SectionTitles {
    resume_style::SectionTitles::parse(
        &req.summary_title,
        &req.skills_title,
        &req.experience_title,
        &req.projects_title,
        &req.education_title,
        req.subtitle_size,
        req.section_size,
    )
}

#[derive(Default, Deserialize)]
struct TitleFields {
    #[serde(default)]
    summary_title: String,
    #[serde(default)]
    skills_title: String,
    #[serde(default)]
    experience_title: String,
    #[serde(default)]
    projects_title: String,
    #[serde(default)]
    education_title: String,
    #[serde(default)]
    subtitle_size: f32,
    #[serde(default)]
    section_size: f32,
}

async fn resume_styles() -> Json<serde_json::Value> {
    Json(resume_style::catalog())
}

/// Resume export. Default is PDF. `format=html` returns a print preview; `md` stays available for the source editor.
async fn export_resume(
    State(s): State<AppState>,
    Query(query): Query<ExportQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let guard = s.profile.read().await;
    let resume = guard.as_ref().ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Upload a resume or start a builder draft first".into(),
    ))?;
    let prefs = s.preferences.read().await.clone();
    let style = resume_style::ResumeStyle::parse(&query.style);
    let face = typeface_from(&query.font, query.body_size, query.heading_size, query.font_weight);
    let doc = resume_style::ResumeDoc::from_profile(resume, prefs.as_ref());
    match query.format.trim().to_ascii_lowercase().as_str() {
        "html" => Ok((
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            resume_style::render_html_with(&doc, style, &face, &titles_from(&query.titles)),
        )
            .into_response()),
        "md" | "markdown" => Ok((
            [
                (header::CONTENT_TYPE, "text/markdown; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"resume.md\"",
                ),
            ],
            resume::render_markdown(resume),
        )
            .into_response()),
        _ => {
            let font_bytes = resume_style::download_google_font(&face.family).await;
            let bytes = resume_style::render_pdf_with(&doc, style, &face, &titles_from(&query.titles), font_bytes.as_deref())
                .map_err(internal)?;
            Ok(resume_style::pdf_response(bytes, "resume.pdf").into_response())
        }
    }
}

#[derive(Deserialize)]
struct RenderResumeRequest {
    #[serde(default)]
    style: String,
    #[serde(default)]
    format: String,
    #[serde(default)]
    profile: Option<crate::resume::CandidateProfile>,
    #[serde(default)]
    tailored: Option<crate::tailored::TailoredResume>,
    #[serde(default)]
    font: String,
    #[serde(default)]
    body_size: f32,
    #[serde(default)]
    heading_size: f32,
    #[serde(default)]
    font_weight: u16,
    #[serde(flatten)]
    titles: TitleFields,
}

/// Preview or PDF from the editor's current form, or a job-tailored selection
/// of the same resume. Does not require saving first.
async fn render_resume(
    State(s): State<AppState>,
    Json(req): Json<RenderResumeRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let prefs = s.preferences.read().await.clone();
    let stored = s.profile.read().await.clone();
    let profile = req.profile.clone().or(stored).ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Upload a resume or start a builder draft first".into(),
    ))?;
    let style = resume_style::ResumeStyle::parse(&req.style);
    let face = typeface_from(&req.font, req.body_size, req.heading_size, req.font_weight);
    let doc = match req.tailored.as_ref() {
        Some(tailored) => resume_style::ResumeDoc::from_tailored(&profile, prefs.as_ref(), tailored),
        None => resume_style::ResumeDoc::from_profile(&profile, prefs.as_ref()),
    };
    if req.format.trim().eq_ignore_ascii_case("pdf") {
        let font_bytes = resume_style::download_google_font(&face.family).await;
        let bytes = resume_style::render_pdf_with(&doc, style, &face, &titles_from(&req.titles), font_bytes.as_deref())
            .map_err(internal)?;
        return Ok(resume_style::pdf_response(bytes, "resume.pdf").into_response());
    }
    Ok(Json(serde_json::json!({
        "style": style.id(),
        "font": face.family,
        "html": resume_style::render_html_with(&doc, style, &face, &titles_from(&req.titles)),
    }))
    .into_response())
}

#[derive(Deserialize)]
struct SuggestionRequest {
    #[serde(default)]
    section: String,
    #[serde(default)]
    profile: Option<crate::resume::CandidateProfile>,
}

/// Section suggestions for the editor. Uses the configured LLM when one is
/// available, and drops any line that is not grounded in the resume.
async fn resume_suggestions(
    State(s): State<AppState>,
    Json(req): Json<SuggestionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let stored = s.profile.read().await.clone();
    let profile = req.profile.or(stored).ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Upload a resume or start a builder draft first".into(),
    ))?;
    let section = req.section.trim().to_ascii_lowercase();
    let basis = suggestion_basis(&profile);
    let snapshot = editor_snapshot(&profile);
    let system = "You suggest alternate resume wording for ONE section. Hard rules:\n\
        1. Use only facts already in the resume. Never add employers, dates, metrics, or skills.\n\
        2. Return exactly 3 short alternatives.\n\
        3. Return valid JSON only: {\"suggestions\":[\"...\",\"...\",\"...\"]}.";
    let user = format!("Section: {section}\nResume:\n{snapshot}");
    match llm::chat_json(&system, &user).await {
        Ok((provider, value)) => {
            let suggestions = value
                .get("suggestions")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(str::trim)
                        .filter(|text| {
                            (8..400).contains(&text.len())
                                && resume::rewrite_is_grounded(text, &basis)
                        })
                        .take(3)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(Json(serde_json::json!({
                "ok": true,
                "section": section,
                "suggestions": suggestions,
                "llm_used": true,
                "provider": provider.name(),
                "note": if suggestions.is_empty() {
                    "Nothing usable came back. Your resume is unchanged."
                } else {
                    "Pick a suggestion to apply it. Each one stays inside your existing facts."
                },
            })))
        }
        Err(error) => Ok(Json(serde_json::json!({
            "ok": true,
            "section": section,
            "suggestions": Vec::<String>::new(),
            "llm_used": false,
            "note": format!("No suggestion model is configured ({error})."),
        }))),
    }
}

fn suggestion_basis(profile: &crate::resume::CandidateProfile) -> String {
    let mut text = String::new();
    if let Some(name) = &profile.name {
        text.push_str(name);
        text.push(' ');
    }
    if let Some(headline) = &profile.headline {
        text.push_str(headline);
        text.push(' ');
    }
    if let Some(summary) = &profile.summary {
        text.push_str(summary);
        text.push(' ');
    }
    for skill in &profile.skills {
        text.push_str(&skill.name);
        text.push(' ');
    }
    for role in &profile.experience {
        text.push_str(&role.title);
        text.push(' ');
        text.push_str(&role.company);
        text.push(' ');
        for bullet in &role.bullets {
            text.push_str(&bullet.text);
            text.push(' ');
        }
    }
    text
}

/// Version history for the resume profile: every save archives the outgoing
/// JSON, capped at the newest 20. Restores are themselves versioned.
async fn resume_versions(
    State(s): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let history = store::kv_history(&s.db, store::CANDIDATE_PROFILE_KEY)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({
        "versions": history
            .into_iter()
            .map(|(version, created_at)| serde_json::json!({"version": version, "created_at": created_at}))
            .collect::<Vec<_>>(),
    })))
}

async fn resume_version(
    State(s): State<AppState>,
    Path(version): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    match store::kv_version(&s.db, store::CANDIDATE_PROFILE_KEY, version)
        .await
        .map_err(internal)?
    {
        Some(payload) => Ok(Json(serde_json::json!({
            "version": version,
            "profile": serde_json::from_str::<serde_json::Value>(&payload).unwrap_or_default(),
        }))),
        None => Err((StatusCode::NOT_FOUND, "No such resume version".into())),
    }
}

async fn restore_resume_version(
    State(s): State<AppState>,
    Path(version): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let payload = store::kv_restore(&s.db, store::CANDIDATE_PROFILE_KEY, version)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    let profile: crate::resume::CandidateProfile = serde_json::from_str(&payload).map_err(bad)?;
    *s.profile.write().await = Some(profile.clone());
    Ok(Json(serde_json::json!({"ok": true, "profile": profile})))
}

#[derive(Deserialize)]
struct ImportMarkdown {
    #[serde(default)]
    markdown: String,
}

/// Parses builder-produced Markdown back into a structured profile for review.
/// The payload is returned unsaved; saving still goes through PUT /api/resume.
async fn import_resume_markdown(
    State(_s): State<AppState>,
    Json(req): Json<ImportMarkdown>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.markdown.trim().is_empty() {
        return Err(bad("Provide Markdown to import"));
    }
    let profile = resume::parse_markdown(&req.markdown);
    let clean = resume::sanitize_builder_profile(profile);
    Ok(Json(serde_json::json!({"ok": true, "profile": clean})))
}

/// Persisted rope editing sessions: full snapshot plus undo/redo op stacks as
/// JSON, so closing the app never loses in-progress source edits.
#[derive(Deserialize)]
struct RopeDocBody {
    #[serde(default)]
    snapshot: String,
    #[serde(default)]
    undo: Vec<serde_json::Value>,
    #[serde(default)]
    redo: Vec<serde_json::Value>,
}

async fn load_rope(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let key = rope_key(&id)?;
    match store::load_rope_doc(&s.db, &key).await.map_err(internal)? {
        Some(doc) => Ok(Json(serde_json::json!({
            "snapshot": doc.snapshot,
            "undo": serde_json::from_str::<Vec<serde_json::Value>>(&doc.undo_stack).unwrap_or_default(),
            "redo": serde_json::from_str::<Vec<serde_json::Value>>(&doc.redo_stack).unwrap_or_default(),
        }))),
        None => Err((StatusCode::NOT_FOUND, "No saved editing session".into())),
    }
}

async fn save_rope(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RopeDocBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let key = rope_key(&id)?;
    store::save_rope_doc(
        &s.db,
        &key,
        &req.snapshot,
        &serde_json::to_string(&req.undo).map_err(internal)?,
        &serde_json::to_string(&req.redo).map_err(internal)?,
    )
    .await
    .map_err(internal)?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn forget_rope(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let key = rope_key(&id)?;
    store::delete_rope_doc(&s.db, &key)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({"ok": true})))
}

fn rope_key(id: &str) -> Result<String, (StatusCode, String)> {
    let id = id.trim().to_ascii_lowercase();
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(bad("Invalid rope document id"));
    }
    Ok(format!("rope:{id}"))
}
async fn get_preferences(State(s): State<AppState>) -> Json<serde_json::Value> {
    match s.preferences.read().await.as_ref() {
        Some(p) => Json(serde_json::to_value(p).unwrap_or_default()),
        None => Json(
            serde_json::json!({"configured":false,"name":"","summary":"","skills":[],"experience_years":0,"preferred_titles":["Backend Engineer","Software Engineer"],"role_keywords":[],"preferred_locations":["Bengaluru","Remote"],"excluded_keywords":[],"minimum_match_score":65,"projects":[],"preferred_work_modes":["Remote","Hybrid"],"auto_apply":false,"email":"","phone":""}),
        ),
    }
}
#[derive(Deserialize)]
struct SuggestWeightsRequest {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    skills: Vec<String>,
}

async fn suggested_weights(
    State(s): State<AppState>,
    Json(req): Json<SuggestWeightsRequest>,
) -> Result<Json<Vec<crate::models::SkillWeight>>, (StatusCode, String)> {
    let mut profile = s.preferences.read().await.clone().ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Save matching preferences before building a weightage resume".into(),
    ))?;
    if !req.summary.trim().is_empty() {
        profile.summary = req.summary;
    }
    if !req.skills.is_empty() {
        profile.skills = req.skills;
    }
    profile.skill_weights.clear();
    let guard = s.profile.read().await;
    let experience = matching::experience_text(guard.as_ref());
    let resume_opt = guard.as_ref();
    Ok(Json(matching::suggested_skill_weights_with_resume(
        &profile,
        resume_opt,
        &experience,
    )))
}

async fn save_preferences(
    State(s): State<AppState>,
    Json(p): Json<crate::models::CandidateProfile>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    store::save_preferences(&s.db, &p).await.map_err(internal)?;
    *s.preferences.write().await = Some(p);
    Ok(Json(serde_json::json!({"ok":true})))
}
async fn get_jobs(
    State(s): State<AppState>,
    Query(paging): Query<Paging>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let rows = load_ranked_jobs(&s).await?;
    Ok(Json(page_envelope(&rows, paging.page, paging.per_page)))
}

/// Keyword search over stored postings via an inverted index (BM25), not
/// regex scans. `q` is free text; `similar_to` is a job id whose most
/// distinctive terms become the query. Same envelope as `/api/jobs`.
async fn search_jobs(
    State(s): State<AppState>,
    Query(query): Query<JobSearchQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut rows = load_ranked_jobs(&s).await?;
    let jobs: Vec<crate::models::Job> = rows.iter().map(|row| row.job.clone()).collect();
    let index = crate::matching::index::PostingIndex::build(&jobs);
    let hits = if !query.similar_to.trim().is_empty() {
        let wanted = query.similar_to.trim();
        let Some(doc_no) = jobs
            .iter()
            .position(|job| job.external_id == wanted)
        else {
            return Err((StatusCode::NOT_FOUND, "Job not found".into()));
        };
        index.similar(doc_no, 200)
    } else {
        index.search(&query.q, 200)
    };
    if hits.is_empty() {
        return Ok(Json(page_envelope(&[], query.page, query.per_page)));
    }
    let order: std::collections::HashMap<usize, f32> = hits.into_iter().collect();
    rows.retain(|row| {
        jobs.iter()
            .position(|job| job.external_id == row.job.external_id)
            .is_some_and(|doc_no| order.contains_key(&doc_no))
    });
    rows.sort_by(|left, right| {
        let left_no = jobs
            .iter()
            .position(|job| job.external_id == left.job.external_id);
        let right_no = jobs
            .iter()
            .position(|job| job.external_id == right.job.external_id);
        match (left_no, right_no) {
            (Some(left_no), Some(right_no)) => order[&right_no]
                .partial_cmp(&order[&left_no])
                .unwrap_or(std::cmp::Ordering::Equal),
            _ => std::cmp::Ordering::Equal,
        }
    });
    for row in &mut rows {
        row.explanation = format!("Keyword match. {}", row.explanation);
    }
    Ok(Json(page_envelope(&rows, query.page, query.per_page)))
}

#[derive(Deserialize)]
struct Paging {
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_per_page")]
    per_page: usize,
}

#[derive(Deserialize)]
struct JobSearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    similar_to: String,
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_per_page")]
    per_page: usize,
}

fn default_page() -> usize {
    1
}

fn default_per_page() -> usize {
    15
}

/// Slice ranked rows into a `{total, page, per_page, jobs}` envelope.
/// 15 per page keeps cards readable; the UI pager navigates the rest.
fn page_envelope(
    rows: &[crate::models::JobResult],
    page: usize,
    per_page: usize,
) -> serde_json::Value {
    let per_page = per_page.clamp(1, 50);
    let total = rows.len();
    let pages = ((total + per_page - 1) / per_page).max(1);
    let page = page.clamp(1, pages);
    let start = (page - 1) * per_page;
    let jobs: Vec<serde_json::Value> = rows
        .iter()
        .skip(start)
        .take(per_page)
        .map(|row| serde_json::to_value(row).unwrap_or_default())
        .collect();
    serde_json::json!({ "total": total, "page": page, "per_page": per_page, "jobs": jobs })
}

/// Ranked, applicable postings shared by the list and search endpoints.
async fn load_ranked_jobs(
    s: &AppState,
) -> Result<Vec<crate::models::JobResult>, (StatusCode, String)> {
    let prefs = s.preferences.read().await.clone();
    let config = store::load_search_config(&s.db).await.map_err(internal)?;
    let mut rows = store::jobs(&s.db).await.map_err(internal)?;
    rows.retain(|result| {
        crate::models::is_applicable_posting(
            result.job.published_at,
            &format!(
                "{} {} {}",
                result.job.title,
                result.job.location.as_deref().unwrap_or(""),
                result.job.description
            ),
            &result.job.url,
            &config,
        )
    });
    if let Some(p) = prefs {
        let guard = s.profile.read().await;
        let experience = matching::experience_text(guard.as_ref());
        let refs: Vec<_> = rows.iter().map(|row| &row.job).collect();
        let ranked = matching::rank_jobs_with_resume(&p, guard.as_ref(), &refs, &experience);
        for item in ranked {
            let row = &mut rows[item.position];
            row.score = item.breakdown.total;
            row.deterministic_score = item.breakdown.total;
            row.rank = item.rank;
            row.matched_skills = item.matched;
            row.missing_skills = item.missing;
            row.status = if item.breakdown.excluded || item.breakdown.total < p.minimum_match_score
            {
                "discovered".into()
            } else {
                "shortlisted".into()
            };
            row.explanation = format!(
                "Rank {}. Index {:.0}/100, experience {:.0}, title {:.0}, location {:.0}. Ranked from the posting index against your skill weights.",
                item.rank, item.breakdown.skill, item.breakdown.experience, item.breakdown.title, item.breakdown.location
            );
        }
        rows.sort_by(|left, right| {
            left.rank.cmp(&right.rank).then_with(|| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        for row in &mut rows {
            row.job.description = preview_text(&row.job.description, 700);
        }
    }
    Ok(rows)
}
#[derive(Deserialize, Default)]
struct RunDiscoveryRequest {
    #[serde(default)]
    latitude: Option<f64>,
    #[serde(default)]
    longitude: Option<f64>,
}

async fn run_discovery(
    State(s): State<AppState>,
    Json(req): Json<RunDiscoveryRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let fix = match (req.latitude, req.longitude) {
        (Some(latitude), Some(longitude)) => Some(crate::discovery::location::GeoFix {
            latitude,
            longitude,
        }),
        _ => None,
    };
    let n = discovery_once(&s, fix)
        .await
        .map_err(|e| (StatusCode::PRECONDITION_FAILED, e.to_string()))?;
    let config = store::load_search_config(&s.db).await.map_err(internal)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "discovered": n,
        "location": config.location,
        "nearby": config.nearby,
        "location_source": config.location_source,
    })))
}

async fn get_search_config(
    State(s): State<AppState>,
) -> Result<Json<crate::models::SearchConfig>, (StatusCode, String)> {
    Ok(Json(
        store::load_search_config(&s.db).await.map_err(internal)?,
    ))
}

async fn save_search_config(
    State(s): State<AppState>,
    Json(config): Json<crate::models::SearchConfig>,
) -> Result<Json<crate::models::SearchConfig>, (StatusCode, String)> {
    let config = crate::discovery::location::retarget(config);
    Ok(Json(
        store::save_search_config(&s.db, &config)
            .await
            .map_err(internal)?,
    ))
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct JobSourceEntry {
    url: String,
    name: Option<String>,
}
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct JobSourcesConfig {
    sources: Vec<JobSourceEntry>,
}
async fn get_sources(
    State(s): State<AppState>,
) -> Result<Json<JobSourcesConfig>, (StatusCode, String)> {
    let path = s.data_dir.join("job_sources.json");
    if !path.exists() {
        return Ok(Json(JobSourcesConfig::default()));
    }
    let bytes = tokio::fs::read(path).await.map_err(internal)?;
    Ok(Json(serde_json::from_slice(&bytes).map_err(internal)?))
}
#[derive(Deserialize)]
struct AddSourceRequest {
    url: String,
    name: Option<String>,
}
async fn add_source(
    State(s): State<AppState>,
    Json(req): Json<AddSourceRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let url = reqwest::Url::parse(&req.url).map_err(bad)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(bad("Only public HTTP/HTTPS URLs are supported"));
    }
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    if host.is_empty()
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host == "0.0.0.0"
        || host == "127.0.0.1"
        || host == "::1"
    {
        return Err(bad("Local or private hostnames are not accepted"));
    }
    if let Ok(std::net::IpAddr::V4(ip)) = host.parse::<std::net::IpAddr>() {
        if ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() {
            return Err(bad("Private IP addresses are not accepted"));
        }
    }
    let path = s.data_dir.join("job_sources.json");
    let mut cfg = if path.exists() {
        serde_json::from_slice::<JobSourcesConfig>(&tokio::fs::read(&path).await.map_err(internal)?)
            .map_err(internal)?
    } else {
        JobSourcesConfig::default()
    };
    if cfg.sources.iter().any(|x| x.url == url.as_str()) {
        return Ok(Json(
            serde_json::json!({"ok":true,"duplicate":true,"sources":cfg.sources.len()}),
        ));
    }
    cfg.sources.push(JobSourceEntry {
        url: url.to_string(),
        name: req
            .name
            .filter(|n| !n.trim().is_empty())
            .or_else(|| Some(host)),
    });
    tokio::fs::write(path, serde_json::to_vec_pretty(&cfg).map_err(internal)?)
        .await
        .map_err(internal)?;
    let note = if crate::providers::greenhouse::parse_board_url(url.as_str()).is_some() {
        "Greenhouse board saved. Discovery loads it through the official job board API."
    } else {
        "robots.txt will be checked before fetching this page"
    };
    Ok(Json(
        serde_json::json!({"ok":true,"sources":cfg.sources.len(),"note":note}),
    ))
}

fn search_keys_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("search.json")
}

fn greenhouse_boards_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("greenhouse_boards.json")
}

async fn discovery_snapshot(s: &AppState) -> Result<serde_json::Value, (StatusCode, String)> {
    let keys =
        crate::discovery::provider::search_key_status(&s.db, &search_keys_path(&s.data_dir)).await;
    let boards = crate::providers::greenhouse::saved_boards(&greenhouse_boards_path(&s.data_dir))
        .await
        .map_err(internal)?;
    Ok(serde_json::json!({
        "interval_hours": 4,
        "last_run": store::latest_run(&s.db).await.map_err(internal)?,
        "provider_count": s.providers.len(),
        "onnx_enabled": cfg!(feature = "onnx"),
        "serpapi_configured": keys.serpapi_configured,
        "jsearch_configured": keys.jsearch_configured,
        "tavily_configured": keys.tavily_configured,
        "brave_configured": keys.brave_configured,
        "greenhouse_boards": boards,
    }))
}

async fn clear_search_cache(
    State(s): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    crate::discovery::provider::clear_search_cache(&s.db)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn discovery_status(
    State(s): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    Ok(Json(discovery_snapshot(&s).await?))
}

#[derive(Deserialize)]
struct SaveSearchRequest {
    #[serde(default)]
    serpapi_api_key: Option<String>,
    #[serde(default)]
    jsearch_api_key: Option<String>,
    #[serde(default)]
    tavily_api_key: Option<String>,
    #[serde(default)]
    brave_api_key: Option<String>,
    #[serde(default)]
    remove_serpapi: bool,
    #[serde(default)]
    remove_jsearch: bool,
    #[serde(default)]
    remove_tavily: bool,
    #[serde(default)]
    remove_brave: bool,
}

async fn save_search(
    State(s): State<AppState>,
    Json(req): Json<SaveSearchRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.remove_serpapi || req.remove_jsearch || req.remove_tavily || req.remove_brave {
        crate::discovery::provider::remove_search_keys(
            &s.db,
            req.remove_serpapi,
            req.remove_jsearch,
            req.remove_tavily,
            req.remove_brave,
        )
        .await
        .map_err(internal)?;
    }
    let serpapi = req
        .serpapi_api_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let jsearch = req
        .jsearch_api_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let tavily = req
        .tavily_api_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let brave = req
        .brave_api_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if serpapi.is_none()
        && jsearch.is_none()
        && tavily.is_none()
        && brave.is_none()
        && !(req.remove_serpapi || req.remove_jsearch || req.remove_tavily || req.remove_brave)
    {
        return Err(bad("Enter a SerpApi, JSearch, Tavily, or Brave API key"));
    }
    crate::discovery::provider::save_search_keys(
        &s.db,
        &search_keys_path(&s.data_dir),
        serpapi,
        jsearch,
        tavily,
        brave,
    )
    .await
    .map_err(internal)?;
    let mut body = discovery_snapshot(&s).await?;
    body["ok"] = serde_json::json!(true);
    body["note"] = serde_json::json!("Search keys are sealed in SQLite and only masked hints are shown. Env vars still override them.");
    Ok(Json(body))
}

#[derive(Deserialize)]
struct AddGreenhouseBoard {
    token: String,
    #[serde(default)]
    name: Option<String>,
}

async fn add_greenhouse_board(
    State(s): State<AppState>,
    Json(req): Json<AddGreenhouseBoard>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (duplicate, _) = crate::providers::greenhouse::add_saved_board(
        &greenhouse_boards_path(&s.data_dir),
        &req.token,
        req.name,
    )
    .await
    .map_err(bad)?;
    let mut body = discovery_snapshot(&s).await?;
    body["ok"] = serde_json::json!(true);
    body["duplicate"] = serde_json::json!(duplicate);
    Ok(Json(body))
}
#[derive(Deserialize)]
struct TailorRequest {
    job: crate::models::Job,
    #[serde(default)]
    style: String,
}
#[derive(Deserialize)]
struct PrepRequest {
    job: crate::models::Job,
    /// Regenerate even when a cached sheet exists.
    #[serde(default)]
    refresh: bool,
}
async fn tailor_job(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TailorRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if id != req.job.external_id {
        return Err((StatusCode::BAD_REQUEST, "Path ID and job ID differ".into()));
    }
    let p = s.profile.read().await;
    let p = p.as_ref().ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Upload a resume first".into(),
    ))?;
    let experience = matching::experience_text(Some(p));
    let (matched, missing) = if let Some(prefs) = s.preferences.read().await.as_ref() {
        matching::skill_overlap_with_resume(prefs, Some(p), &req.job, &experience)
    } else {
        (
            p.skills
                .iter()
                .filter(|x| {
                    req.job
                        .description
                        .to_lowercase()
                        .contains(&x.name.to_lowercase())
                })
                .map(|x| x.name.clone())
                .collect(),
            req.job.skills.clone(),
        )
    };
    let bullets = crate::apply::supporting_bullets(p, &req.job, &matched);
    Ok(Json(
        serde_json::json!({"job_title":req.job.title,"company":req.job.company,"matched_skills":matched,"missing_requirements":missing,"relevant_experience_bullets":bullets,"integrity_note":"Review each claim. Never add qualifications that are not supported by your resume."}),
    ))
}

/// Per-job tailored resume. The deterministic base is built from the parsed
/// (LLM-extracted, evidence-backed) resume; when a tailor LLM is configured
/// (Ollama / OpenAI-compatible / Anthropic / Gemini via TAILOR_LLM_PROVIDER),
/// it rewrites the headline/summary and re-picks bullets, and every line is
/// validated back against the resume before it is returned.
async fn tailored_resume_job(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TailorRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (resume, prefs, tailored) = build_job_resume(&s, &id, &req).await?;
    let style = resume_style::ResumeStyle::parse(&req.style);
    let doc = resume_style::ResumeDoc::from_tailored(&resume, prefs.as_ref(), &tailored);
    let mut value = serde_json::to_value(&tailored).map_err(internal)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("style".into(), serde_json::json!(style.id()));
        obj.insert(
            "html".into(),
            serde_json::Value::String(resume_style::render_html(&doc, style)),
        );
    }
    Ok(Json(value))
}

async fn tailored_resume_pdf(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TailorRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (resume, prefs, tailored) = build_job_resume(&s, &id, &req).await?;
    let style = resume_style::ResumeStyle::parse(&req.style);
    let doc = resume_style::ResumeDoc::from_tailored(&resume, prefs.as_ref(), &tailored);
    let bytes = resume_style::render_pdf(&doc, style).map_err(internal)?;
    let filename = resume_style::safe_filename(&format!(
        "resume-{}-{}",
        req.job.company,
        style.id()
    ));
    Ok(resume_style::pdf_response(bytes, &filename).into_response())
}

async fn build_job_resume(
    s: &AppState,
    id: &str,
    req: &TailorRequest,
) -> Result<
    (
        crate::resume::CandidateProfile,
        Option<crate::models::CandidateProfile>,
        tailored::TailoredResume,
    ),
    (StatusCode, String),
> {
    if id != req.job.external_id {
        return Err((StatusCode::BAD_REQUEST, "Path ID and job ID differ".into()));
    }
    let guard = s.profile.read().await;
    let resume = guard.as_ref().ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Upload a resume first".into(),
    ))?;
    let prefs = s.preferences.read().await.clone();
    let mut tailored = tailored::build_tailored_resume(resume, prefs.as_ref(), &req.job);
    let (system, user) = tailored::tailor_prompts(resume, prefs.as_ref(), &req.job, &tailored);
    match llm::chat_json(&system, &user).await {
        Ok((provider, value)) => {
            tailored::merge_llm_rewrite(&mut tailored, resume, prefs.as_ref(), &req.job, &value);
            tailored.llm_used = true;
            tailored.llm_provider = Some(provider.name().to_string());
        }
        Err(error) => {
            tracing::warn!(%error, "tailor LLM unavailable; returning deterministic resume");
        }
    }
    Ok((resume.clone(), prefs, tailored))
}

/// Per-job interview prep sheet. Deterministic base (technical questions from
/// matched skills, behavioral questions paired with your own resume bullets)
/// plus an LLM rewrite when configured; quoted evidence is validated back
/// against the resume. Cached in SQLite per job; pass refresh to rebuild.
async fn interview_prep_job(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PrepRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if id != req.job.external_id {
        return Err((StatusCode::BAD_REQUEST, "Path ID and job ID differ".into()));
    }
    if !req.refresh {
        if let Some(cached) = store::load_prep_sheet(&s.db, &id).await.map_err(internal)? {
            if let Ok(sheet) = serde_json::from_str::<serde_json::Value>(&cached) {
                return Ok(Json(serde_json::json!({"sheet": sheet, "cached": true})));
            }
        }
    }
    let guard = s.profile.read().await;
    let resume = guard.as_ref().ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Upload a resume first".into(),
    ))?;
    let prefs = s.preferences.read().await.clone();
    let experience = matching::experience_text(Some(resume));
    let (matched, _) = match prefs.as_ref() {
        Some(prefs) => {
            matching::skill_overlap_with_resume(prefs, Some(resume), &req.job, &experience)
        }
        None => (
            resume
                .skills
                .iter()
                .map(|skill| skill.name.clone())
                .collect(),
            Vec::new(),
        ),
    };
    let mut sheet = prep::build_deterministic(resume, &req.job, &matched);
    let mut generated_by: Option<String> = None;
    let (system, user) = prep::prep_prompts(resume, &req.job, &sheet);
    match llm::chat_json(&system, &user).await {
        Ok((provider, value)) => {
            prep::merge_llm_prep(&mut sheet, resume, &value);
            generated_by = Some(provider.name().to_string());
        }
        Err(error) => {
            tracing::warn!(%error, "prep LLM unavailable; returning deterministic sheet");
        }
    }
    sheet.generated_by = generated_by;
    let payload = serde_json::to_value(&sheet).map_err(internal)?;
    store::save_prep_sheet(&s.db, &id, &payload.to_string())
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({"sheet": payload, "cached": false})))
}

#[derive(serde::Serialize)]
struct CompanyEntry {
    key: String,
    name: String,
    jobs: usize,
    applications: usize,
    top_role: String,
    top_score: f32,
    statuses: Vec<String>,
    has_guide: bool,
    notes_preview: String,
}

/// Company knowledge base: every saved posting and application grouped by
/// company, merged with stored notes and cached guides.
async fn list_companies(
    State(s): State<AppState>,
) -> Result<Json<Vec<CompanyEntry>>, (StatusCode, String)> {
    let jobs = store::jobs(&s.db).await.map_err(internal)?;
    let applications = store::applications(&s.db).await.map_err(internal)?;
    let mut order: Vec<String> = Vec::new();
    let mut names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut job_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut best: std::collections::HashMap<String, (String, f32)> =
        std::collections::HashMap::new();
    for result in &jobs {
        let key = study::company_key(&result.job.company);
        if key.is_empty() {
            continue;
        }
        names
            .entry(key.clone())
            .or_insert_with(|| result.job.company.clone());
        *job_counts.entry(key.clone()).or_insert(0) += 1;
        let entry = best
            .entry(key.clone())
            .or_insert((result.job.title.clone(), 0.0));
        if result.score > entry.1 {
            *entry = (result.job.title.clone(), result.score);
        }
        if !order.contains(&key) {
            order.push(key);
        }
    }
    let mut app_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut statuses: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for application in &applications {
        let key = study::company_key(&application.company);
        if key.is_empty() {
            continue;
        }
        names
            .entry(key.clone())
            .or_insert_with(|| application.company.clone());
        *app_counts.entry(key.clone()).or_insert(0) += 1;
        let list = statuses.entry(key.clone()).or_default();
        if !list.contains(&application.status) {
            list.push(application.status.clone());
        }
        if !order.contains(&key) {
            order.push(key);
        }
    }
    let mut entries = Vec::new();
    for key in order {
        let profile = store::load_company_profile(&s.db, &key)
            .await
            .map_err(internal)?;
        let (notes, guide) = profile
            .map(|(_, notes, guide)| (notes, guide))
            .unwrap_or_default();
        let (top_role, top_score) = best.remove(&key).unwrap_or_default();
        entries.push(CompanyEntry {
            name: names.remove(&key).unwrap_or_else(|| key.clone()),
            jobs: job_counts.remove(&key).unwrap_or(0),
            applications: app_counts.remove(&key).unwrap_or(0),
            top_role,
            top_score,
            statuses: statuses.remove(&key).unwrap_or_default(),
            has_guide: !guide.is_empty(),
            notes_preview: notes.chars().take(120).collect(),
            key,
        });
    }
    entries.sort_by(|a, b| {
        b.applications.cmp(&a.applications).then_with(|| {
            b.top_score
                .partial_cmp(&a.top_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    Ok(Json(entries))
}

#[derive(Deserialize)]
struct GuideRefresh {
    #[serde(default)]
    refresh: bool,
}

async fn company_guide(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    guide_for_company(&s, &name, false).await
}

async fn rebuild_company_guide(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<GuideRefresh>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    guide_for_company(&s, &name, req.refresh).await
}

#[allow(clippy::too_many_lines)]
async fn guide_for_company(
    s: &AppState,
    name: &str,
    refresh: bool,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let key = study::company_key(name);
    if key.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Unknown company".into()));
    }
    let stored = store::load_company_profile(&s.db, &key)
        .await
        .map_err(internal)?;
    let (display_name, notes, cached) = match stored {
        Some((display_name, notes, guide)) => (display_name, notes, guide),
        None => (name.trim().to_string(), String::new(), String::new()),
    };
    if !refresh && !cached.is_empty() {
        if let Ok(sheet) = serde_json::from_str::<serde_json::Value>(&cached) {
            return Ok(Json(
                serde_json::json!({"company": display_name, "notes": notes, "guide": sheet, "cached": true}),
            ));
        }
    }
    let jobs = store::jobs(&s.db).await.map_err(internal)?;
    let mut relevant: Vec<&crate::models::JobResult> = jobs
        .iter()
        .filter(|r| study::company_key(&r.job.company) == key)
        .collect();
    relevant.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    relevant.truncate(5);
    if relevant.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            "No saved postings for this company".into(),
        ));
    }
    let applications = store::applications(&s.db).await.map_err(internal)?;
    let app_count = applications
        .iter()
        .filter(|a| study::company_key(&a.company) == key)
        .count();
    let guard = s.profile.read().await;
    let resume = guard.as_ref().ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Upload a resume first".into(),
    ))?;
    let refs: Vec<&crate::models::Job> = relevant.iter().map(|r| &r.job).collect();
    let mut bundle = study::build_bundle(&display_name, &refs, app_count, resume);
    let context = relevant
        .iter()
        .map(|r| {
            format!(
                "## {}\n{}",
                r.job.title,
                r.job.description.chars().take(2_000).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let (system, user) = study::study_prompts(&bundle, &context);
    let mut generated_by: Option<String> = None;
    match llm::chat_json(&system, &user).await {
        Ok((provider, value)) => {
            study::merge_study(&mut bundle, &value);
            generated_by = Some(provider.name().to_string());
        }
        Err(error) => {
            tracing::warn!(%error, company = %display_name, "study LLM unavailable; returning deterministic guide");
        }
    }
    bundle.generated_by = generated_by;
    // Community Q&A is additive and license-safe (verbatim quotes with
    // attribution), so it sits outside the LLM merge. When no repo questions
    // match, Q&A is generated from your own resume instead. Primer links are
    // already baked into the bundle by the builder.
    let dsa_names: Vec<String> = bundle.dsa_topics.iter().map(|t| t.topic.clone()).collect();
    bundle.community_qa = crate::knowledge::community_qa_with_fallback(
        &s.db,
        resume,
        &bundle.top_skills,
        &dsa_names,
        8,
    )
    .await;
    let payload = serde_json::to_value(&bundle).map_err(internal)?;
    store::save_company_profile(&s.db, &key, &display_name, &notes, &payload.to_string())
        .await
        .map_err(internal)?;
    Ok(Json(
        serde_json::json!({"company": display_name, "notes": notes, "guide": payload, "cached": false}),
    ))
}

#[derive(Deserialize)]
struct CompanyNotes {
    #[serde(default)]
    notes: String,
}

async fn save_company_notes(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<CompanyNotes>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let key = study::company_key(&name);
    if key.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Unknown company".into()));
    }
    let notes = req.notes.chars().take(4_000).collect::<String>();
    let stored = store::load_company_profile(&s.db, &key)
        .await
        .map_err(internal)?;
    let (display_name, _, guide) =
        stored.unwrap_or((name.trim().to_string(), String::new(), String::new()));
    store::save_company_profile(&s.db, &key, &display_name, &notes, &guide)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({"ok": true})))
}
async fn auto_apply_now(
    State(s): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let prefs = s.preferences.read().await.clone().ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Save matching preferences before applying".into(),
    ))?;
    let report = crate::apply::auto_apply(&s, &prefs)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "drafted": report.drafted,
        "submitted": report.submitted,
        "needs_manual": report.needs_manual,
        "skipped": report.skipped,
    })))
}
#[derive(Deserialize)]
struct AutofillQuery {
    #[serde(default)]
    url: String,
}

#[derive(serde::Serialize)]
struct AutofillPayload {
    first_name: String,
    last_name: String,
    full_name: String,
    email: String,
    phone: String,
    location: String,
    country: String,
    headline: String,
    summary: String,
    cover_letter: String,
    skills: Vec<String>,
    current_title: String,
    current_company: String,
    companies: Vec<String>,
    schools: Vec<String>,
    education: String,
    matched_job: Option<String>,
    resume_file: Option<String>,
}

async fn autofill(
    State(s): State<AppState>,
    Query(query): Query<AutofillQuery>,
) -> Result<Json<AutofillPayload>, (StatusCode, String)> {
    let prefs = s.preferences.read().await.clone();
    let profile = s.profile.read().await.clone();
    let prefs = prefs.ok_or((
        StatusCode::PRECONDITION_FAILED,
        "Save matching preferences before using the extension".into(),
    ))?;
    let full_name = prefs.name.trim().to_string();
    let (first_name, last_name) = if full_name.is_empty() {
        (String::new(), String::new())
    } else {
        crate::apply::split_name(&full_name)
    };
    let location = prefs
        .preferred_locations
        .iter()
        .map(|place| place.trim())
        .find(|place| !place.is_empty() && !place.eq_ignore_ascii_case("remote"))
        .unwrap_or("")
        .to_string();
    let config = store::load_search_config(&s.db).await.map_err(internal)?;
    let (
        headline,
        summary,
        skills,
        current_title,
        current_company,
        companies,
        schools,
        education,
        resume_file,
    ) = match profile.as_ref() {
        Some(profile) => {
            let role = profile.experience.first();
            let companies =
                unique_names(profile.experience.iter().map(|item| item.company.as_str()));
            let schools = unique_names(
                profile
                    .education
                    .iter()
                    .map(|item| item.institution.as_str()),
            );
            let school = profile.education.first().map(|item| {
                if item.institution.trim().is_empty() {
                    item.qualification.clone()
                } else if item.qualification.trim().is_empty() {
                    item.institution.clone()
                } else {
                    format!("{}, {}", item.qualification.trim(), item.institution.trim())
                }
            });
            (
                profile.headline.clone().unwrap_or_default(),
                profile
                    .summary
                    .clone()
                    .unwrap_or_else(|| prefs.summary.clone()),
                profile
                    .skills
                    .iter()
                    .map(|skill| skill.name.clone())
                    .filter(|name| !name.trim().is_empty())
                    .collect::<Vec<_>>(),
                role.map(|item| item.title.clone()).unwrap_or_default(),
                role.map(|item| item.company.clone()).unwrap_or_default(),
                companies,
                schools,
                school.unwrap_or_default(),
                profile.source_file.clone(),
            )
        }
        None => (
            String::new(),
            prefs.summary.clone(),
            prefs.skills.clone(),
            String::new(),
            String::new(),
            Vec::new(),
            Vec::new(),
            String::new(),
            None,
        ),
    };
    let applications = store::applications(&s.db).await.map_err(internal)?;
    let matched = applications
        .into_iter()
        .find(|application| same_posting(&query.url, &application.job_url));
    let (cover_letter, matched_job) = match matched {
        Some(application) => (
            application.tailored_summary,
            Some(format!(
                "{} · {}",
                application.job_title, application.company
            )),
        ),
        None => (String::new(), None),
    };
    Ok(Json(AutofillPayload {
        first_name,
        last_name,
        full_name,
        email: prefs.email.trim().to_string(),
        phone: prefs.phone.trim().to_string(),
        location,
        country: config.target_country,
        headline,
        summary,
        cover_letter,
        skills,
        current_title,
        current_company,
        companies,
        schools,
        education,
        matched_job,
        resume_file,
    }))
}

fn unique_names<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = Vec::new();
    for name in names {
        let name = name.trim();
        if name.is_empty()
            || seen
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(name))
        {
            continue;
        }
        seen.push(name.to_string());
    }
    seen
}

fn same_posting(page: &str, job: &str) -> bool {
    let page = canonical_posting_url(page);
    let job = canonical_posting_url(job);
    if page.is_empty() || job.is_empty() {
        return false;
    }
    page == job || page.starts_with(&format!("{job}/")) || job.starts_with(&format!("{page}/"))
}

fn canonical_posting_url(url: &str) -> String {
    let without_query = url.trim().split(['?', '#']).next().unwrap_or("").trim();
    without_query
        .trim_end_matches('/')
        .to_ascii_lowercase()
        .replace("://www.", "://")
}

async fn get_applications(
    State(s): State<AppState>,
) -> Result<Json<Vec<crate::models::Application>>, (StatusCode, String)> {
    Ok(Json(store::applications(&s.db).await.map_err(internal)?))
}
#[derive(Deserialize)]
struct CreateApplication {
    external_id: String,
    tailored_summary: String,
    selected_bullets: Vec<String>,
}
async fn create_application(
    State(s): State<AppState>,
    Json(req): Json<CreateApplication>,
) -> Result<Json<crate::models::Application>, (StatusCode, String)> {
    let job = store::get_job(&s.db, &req.external_id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "Job not found".into()))?;
    Ok(Json(
        store::create_application(&s.db, &job, req.tailored_summary, req.selected_bullets)
            .await
            .map_err(internal)?,
    ))
}
#[derive(Deserialize)]
struct StatusUpdate {
    status: String,
    #[serde(default)]
    notes: String,
}
async fn update_application_status(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<StatusUpdate>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let allowed = [
        "pending_review",
        "approved",
        "applied",
        "applied_manually",
        "interview",
        "selected",
        "offer",
        "rejected",
        "withdrawn",
    ];
    if !allowed.contains(&req.status.as_str()) {
        return Err((StatusCode::BAD_REQUEST, "Invalid status".into()));
    }
    let ok = store::update_application(&s.db, &id, &req.status, &req.notes)
        .await
        .map_err(internal)?;
    if !ok {
        return Err((StatusCode::NOT_FOUND, "Application not found".into()));
    }
    Ok(Json(serde_json::json!({"ok":true,"status":req.status})))
}
async fn email_status(
    State(s): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let imap_configured = std::env::var("EMAIL_IMAP_USER")
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
        && std::env::var("EMAIL_IMAP_PASS")
            .ok()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
    let oauth_connected = crate::email::oauth_connected(&s.db).await;
    let last_sync = store::kv_get(&s.db, "email_sync_state")
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({
        "configured": imap_configured || oauth_connected,
        "imap_configured": imap_configured,
        "oauth_connected": oauth_connected,
        "oauth_configured": crate::email::oauth_client_config().is_some(),
        "last_sync": last_sync,
    })))
}
async fn email_oauth_start() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let Some(url) = crate::email::oauth_start_url() else {
        return Err((
            StatusCode::PRECONDITION_FAILED,
            "Set GOOGLE_OAUTH_CLIENT_ID and GOOGLE_OAUTH_CLIENT_SECRET first (Desktop-app client).".into(),
        ));
    };
    Ok(Json(serde_json::json!({"url": url})))
}
#[derive(Deserialize)]
struct OAuthCallback {
    #[serde(default)]
    code: String,
    #[serde(default)]
    error: String,
}
async fn email_oauth_callback(
    State(s): State<AppState>,
    Query(query): Query<OAuthCallback>,
) -> Result<axum::response::Html<String>, (StatusCode, String)> {
    if !query.error.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, format!("Google refused: {}", query.error)));
    }
    if query.code.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Missing code".into()));
    }
    let Some((client_id, client_secret)) = crate::email::oauth_client_config() else {
        return Err((
            StatusCode::PRECONDITION_FAILED,
            "GOOGLE_OAUTH_CLIENT_ID / GOOGLE_OAUTH_CLIENT_SECRET are not set".into(),
        ));
    };
    let body: serde_json::Value = reqwest::Client::new()
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", query.code.trim()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", crate::email::oauth_redirect_uri().as_str()),
        ])
        .send()
        .await
        .map_err(internal)?
        .json()
        .await
        .map_err(internal)?;
    let Some(refresh_token) = body
        .get("refresh_token")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|token| !token.is_empty())
    else {
        return Err((
            StatusCode::BAD_GATEWAY,
            "Google did not return a refresh token. Remove the app's access at myaccount.google.com/permissions and retry.".into(),
        ));
    };
    crate::email::save_refresh_token(&s.db, refresh_token)
        .await
        .map_err(internal)?;
    Ok(axum::response::Html(
        "<!doctype html><title>Gmail connected</title><body style=\"font-family:system-ui\">\
         <h1>Gmail connected</h1>\
         <p>The agent can now read recruiter replies (read-only). You can close this tab and press “Sync email outcomes” in the dashboard.</p>\
         </body>"
            .to_string(),
    ))
}
async fn email_disconnect(
    State(s): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    crate::email::disconnect_oauth(&s.db)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({"ok": true})))
}
/// Zips the local `extension/` folder (manifest + popup) so the dashboard can
/// offer a one-click download: unzip → chrome://extensions → Load unpacked.
async fn extension_download() -> Result<axum::response::Response, (StatusCode, String)> {
    let files = ["manifest.json", "popup.html", "popup.js"];
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for name in files {
            let path = std::path::Path::new("extension").join(name);
            let bytes = std::fs::read(&path)
                .map_err(|_| (StatusCode::NOT_FOUND, format!("extension/{name} not found next to the server binary")))?;
            zip.start_file(name, options)
                .map_err(internal)?;
            use std::io::Write;
            zip.write_all(&bytes).map_err(internal)?;
        }
        zip.finish().map_err(internal)?;
    }
    axum::http::Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"rust-job-agent-extension.zip\"",
        )
        .body(axum::body::Body::from(buffer.into_inner()))
        .map_err(internal)
}
async fn sync_email_outcomes(
    State(s): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let report = crate::email::sync_outcomes(&s.db).await.map_err(internal)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "skipped_no_config": report.skipped_no_config,
        "checked": report.checked,
        "matched": report.matched,
        "updated": report.updated,
    })))
}
#[derive(Deserialize)]
struct ReportApplied {
    #[serde(default)]
    url: String,
}
fn normalize_report_url(url: &str) -> String {
    url.trim()
        .trim_end_matches('/')
        .to_ascii_lowercase()
        .replace("://www.", "://")
}
async fn report_applied(
    State(s): State<AppState>,
    Json(req): Json<ReportApplied>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Called by the browser extension after the user submits a form it
    // filled: moves the matching draft to `applied_manually`. This is what
    // makes auto-apply universal — ATS APIs submit directly, every other
    // site flows through fill-then-report.
    let want = normalize_report_url(&req.url);
    if want.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Missing url".into()));
    }
    let applications = store::applications(&s.db).await.map_err(internal)?;
    let found = applications.iter().find(|application| {
        let have = normalize_report_url(&application.job_url);
        !have.is_empty() && (have == want || have.contains(&want) || want.contains(&have))
    });
    let Some(application) = found else {
        return Err((
            StatusCode::NOT_FOUND,
            "No application draft matches this page. Create a draft first.".into(),
        ));
    };
    let notes = if application.notes.trim().is_empty() {
        "Marked applied from the browser extension after manual submit.".to_string()
    } else {
        format!(
            "{}\nMarked applied from the browser extension after manual submit.",
            application.notes
        )
    };
    store::update_application(&s.db, &application.id, "applied_manually", &notes)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "id": application.id,
        "status": "applied_manually",
    })))
}
fn preview_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn bad(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}
fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::same_posting;

    #[test]
    fn matches_the_same_posting_without_the_query_string() {
        assert!(same_posting(
            "https://www.example.com/jobs/12?ref=agent",
            "https://example.com/jobs/12"
        ));
        assert!(!same_posting(
            "https://example.com/jobs/12",
            "https://example.com/jobs/99"
        ));
    }
}
