use crate::models::{Application, Job, JobResult, SearchConfig};
use chrono::Utc;
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

pub async fn connect(path: &str) -> anyhow::Result<SqlitePool> {
    let db = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(path)
        .await?;
    sqlx::query("PRAGMA busy_timeout = 5000")
        .execute(&db)
        .await?;
    Ok(db)
}

const SEARCH_CONFIG_KEY: &str = "search_config";

pub async fn load_search_config(db: &SqlitePool) -> anyhow::Result<SearchConfig> {
    let row = sqlx::query_as::<_, (String,)>("SELECT value FROM settings WHERE key=?")
        .bind(SEARCH_CONFIG_KEY)
        .fetch_optional(db)
        .await?;
    Ok(match row {
        Some((value,)) => serde_json::from_str::<SearchConfig>(&value)
            .unwrap_or_default()
            .normalized(),
        None => SearchConfig::default(),
    })
}

pub async fn save_search_config(
    db: &SqlitePool,
    config: &SearchConfig,
) -> anyhow::Result<SearchConfig> {
    let config = config.clone().normalized();
    let value = serde_json::to_string(&config)?;
    sqlx::query(
        "INSERT INTO settings(key, value) VALUES(?, ?)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .bind(SEARCH_CONFIG_KEY)
    .bind(value)
    .execute(db)
    .await?;
    Ok(config)
}
pub async fn migrate(db: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS jobs (external_id TEXT PRIMARY KEY, source TEXT NOT NULL, payload TEXT NOT NULL, score REAL NOT NULL, status TEXT NOT NULL, discovered_at TEXT NOT NULL)").execute(db).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS applications (id TEXT PRIMARY KEY, external_id TEXT NOT NULL UNIQUE, job_title TEXT NOT NULL, company TEXT NOT NULL, job_url TEXT NOT NULL, status TEXT NOT NULL, tailored_summary TEXT NOT NULL, selected_bullets TEXT NOT NULL, notes TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)").execute(db).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS runs (id INTEGER PRIMARY KEY AUTOINCREMENT, started_at TEXT NOT NULL, finished_at TEXT, discovered INTEGER NOT NULL DEFAULT 0, error TEXT)").execute(db).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(db)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS page_texts (url TEXT PRIMARY KEY, text TEXT NOT NULL, fetched_at TEXT NOT NULL)")
        .execute(db)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS secrets (provider TEXT PRIMARY KEY, nonce TEXT NOT NULL, ciphertext TEXT NOT NULL, last4 TEXT NOT NULL, updated_at TEXT NOT NULL)")
        .execute(db)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS prep_sheets (external_id TEXT PRIMARY KEY, payload TEXT NOT NULL, updated_at TEXT NOT NULL)")
        .execute(db)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS company_profiles (company_key TEXT PRIMARY KEY, display_name TEXT NOT NULL, notes TEXT NOT NULL DEFAULT '', guide_payload TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL)")
        .execute(db)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS kv_store (key TEXT PRIMARY KEY, payload TEXT NOT NULL, updated_at TEXT NOT NULL)")
        .execute(db)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS kv_history (key TEXT NOT NULL, version INTEGER NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL, PRIMARY KEY (key, version))")
        .execute(db)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS rope_docs (doc_id TEXT PRIMARY KEY, snapshot TEXT NOT NULL DEFAULT '', undo_stack TEXT NOT NULL DEFAULT '[]', redo_stack TEXT NOT NULL DEFAULT '[]', updated_at TEXT NOT NULL)")
        .execute(db)
        .await?;
    ensure_search_pages(db).await?;
    ensure_jobs_columns(db).await?;
    ensure_jobs_indexes(db).await?;
    retire_removed_provider(db, "exa").await?;
    Ok(())
}

/// One-time cleanup for a dropped search provider: forget its sealed key,
/// erase its cached pages, and hide stored postings it sourced so stale
/// results never reach the dashboard.
async fn retire_removed_provider(db: &SqlitePool, provider: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM secrets WHERE provider=?")
        .bind(provider)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM search_pages WHERE provider=?")
        .bind(provider)
        .execute(db)
        .await?;
    sqlx::query("UPDATE jobs SET deleted_at=? WHERE source=? AND deleted_at IS NULL")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(provider)
        .execute(db)
        .await?;
    Ok(())
}

/// Adds the soft-delete column to older `jobs` tables and the range indexes
/// pagination and cleanup queries need. Idempotent.
async fn ensure_jobs_columns(db: &SqlitePool) -> anyhow::Result<()> {
    let columns = sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info('jobs')")
        .fetch_all(db)
        .await?;
    if !columns.iter().any(|name| name == "deleted_at") {
        sqlx::query("ALTER TABLE jobs ADD COLUMN deleted_at TEXT")
            .execute(db)
            .await?;
    }
    Ok(())
}

async fn ensure_jobs_indexes(db: &SqlitePool) -> anyhow::Result<()> {
    for statement in [
        "CREATE INDEX IF NOT EXISTS idx_jobs_score ON jobs(score DESC)",
        "CREATE INDEX IF NOT EXISTS idx_jobs_status_discovered ON jobs(status, discovered_at)",
        "CREATE INDEX IF NOT EXISTS idx_jobs_deleted ON jobs(deleted_at)",
        "CREATE INDEX IF NOT EXISTS idx_search_pages_query ON search_pages(query)",
    ] {
        sqlx::query(statement).execute(db).await?;
    }
    Ok(())
}

async fn ensure_search_pages(db: &SqlitePool) -> anyhow::Result<()> {
    let table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='search_pages'",
    )
    .fetch_one(db)
    .await?
        > 0;
    if table_exists {
        let columns =
            sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info('search_pages')")
                .fetch_all(db)
                .await?;
        if !columns.iter().any(|name| name == "deleted_at")
            || !columns.iter().any(|name| name == "id")
        {
            sqlx::query("ALTER TABLE search_pages RENAME TO search_pages_legacy")
                .execute(db)
                .await?;
            create_search_pages(db).await?;
            if columns.iter().any(|name| name == "deleted_at") {
                sqlx::query(
                    "INSERT INTO search_pages(cached_on, provider, query, page, results, deleted_at)
                     SELECT cached_on, provider, query, page, results, deleted_at FROM search_pages_legacy",
                )
                .execute(db)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO search_pages(cached_on, provider, query, page, results)
                     SELECT cached_on, provider, query, page, results FROM search_pages_legacy",
                )
                .execute(db)
                .await?;
            }
            sqlx::query("DROP TABLE search_pages_legacy")
                .execute(db)
                .await?;
        }
    } else {
        create_search_pages(db).await?;
    }
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS search_pages_live
         ON search_pages(cached_on, provider, query, page)
         WHERE deleted_at IS NULL",
    )
    .execute(db)
    .await?;
    Ok(())
}

async fn create_search_pages(db: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE search_pages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            cached_on TEXT NOT NULL,
            provider TEXT NOT NULL,
            query TEXT NOT NULL,
            page INTEGER NOT NULL,
            results TEXT NOT NULL,
            deleted_at TEXT
        )",
    )
    .execute(db)
    .await?;
    Ok(())
}
pub async fn upsert_job(db: &SqlitePool, result: &JobResult) -> anyhow::Result<()> {
    upsert_jobs(db, std::slice::from_ref(result)).await
}

pub async fn upsert_jobs(db: &SqlitePool, results: &[JobResult]) -> anyhow::Result<()> {    if results.is_empty() {
        return Ok(());
    }
    let mut tx = db.begin().await?;
    for result in results {
        let payload = serde_json::to_string(result)?;
        sqlx::query(
            "INSERT INTO jobs(external_id,source,payload,score,status,discovered_at,deleted_at) VALUES(?,?,?,?,?,?,NULL)
             ON CONFLICT(external_id) DO UPDATE SET payload=excluded.payload,score=excluded.score,deleted_at=NULL",
        )
        .bind(&result.job.external_id)
        .bind(&result.job.source)
        .bind(payload)
        .bind(result.score)
        .bind(&result.status)
        .bind(result.job.discovered_at.to_rfc3339())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Soft-deletes stored jobs that fail the posting quality gates (thin bodies,
/// unknown company, missing location, zero skill overlap) by stamping
/// `deleted_at` instead of removing rows. Runs after every discovery so
/// already-saved empties disappear from the dashboard too; re-discovered
/// postings are revived by upsert. Returns the number of newly hidden rows.
pub async fn purge_low_quality_jobs(
    db: &SqlitePool,
    min_body_chars: usize,
    require_company: bool,
    require_location: bool,
    require_skills: bool,
) -> anyhow::Result<u64> {
    let company = if require_company {
        "OR TRIM(COALESCE(JSON_EXTRACT(payload, '$.job.company'), '')) IN ('', 'Unknown company')"
    } else {
        ""
    };
    let location = if require_location {
        "OR JSON_EXTRACT(payload, '$.job.location') IS NULL OR TRIM(COALESCE(JSON_EXTRACT(payload, '$.job.location'), '')) = ''"
    } else {
        ""
    };
    let skills = if require_skills {
        "OR COALESCE(JSON_ARRAY_LENGTH(JSON_EXTRACT(payload, '$.matched_skills')), 0) = 0"
    } else {
        ""
    };
    let sql = format!(
        "UPDATE jobs SET deleted_at=? WHERE deleted_at IS NULL AND (LENGTH(TRIM(COALESCE(JSON_EXTRACT(payload, '$.job.description'), ''))) < ? {company} {location} {skills})"
    );
    let result = sqlx::query(&sql)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(min_body_chars as i64)
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}

pub async fn soft_delete_jobs(db: &SqlitePool, ids: &[String]) -> anyhow::Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = db.begin().await?;
    let mut hidden = 0u64;
    for id in ids {
        let result = sqlx::query("UPDATE jobs SET deleted_at=? WHERE external_id=? AND deleted_at IS NULL")
            .bind(&now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        hidden += result.rows_affected();
    }
    tx.commit().await?;
    Ok(hidden)
}

/// Permanently removes soft-deleted jobs older than `older_than_days`.
/// Runs on the weekly cleanup alongside expired search pages.
pub async fn purge_deleted_jobs(db: &SqlitePool, older_than_days: i64) -> anyhow::Result<u64> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(older_than_days)).to_rfc3339();
    let result = sqlx::query("DELETE FROM jobs WHERE deleted_at IS NOT NULL AND deleted_at < ?")
        .bind(cutoff)
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}
pub async fn jobs(db: &SqlitePool) -> anyhow::Result<Vec<JobResult>> {
    let rows = sqlx::query_as::<_, (String, f64, String)>(
        "SELECT payload,score,status FROM jobs WHERE deleted_at IS NULL ORDER BY score DESC",
    )
    .fetch_all(db)
    .await?;
    let mut out = vec![];
    for (payload, score, status) in rows {
        let mut result: JobResult = serde_json::from_str(&payload)?;
        result.score = score as f32;
        result.status = status;
        out.push(result);
    }
    Ok(out)
}
pub async fn get_job(db: &SqlitePool, id: &str) -> anyhow::Result<Option<Job>> {
    let row = sqlx::query_as::<_, (String,)>("SELECT payload FROM jobs WHERE external_id=? AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row
        .map(|x| serde_json::from_str::<JobResult>(&x.0).map(|r| r.job))
        .transpose()?)
}
pub async fn has_application(db: &SqlitePool, external_id: &str) -> anyhow::Result<bool> {
    let row = sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM applications WHERE external_id=?")
        .bind(external_id)
        .fetch_one(db)
        .await?;
    Ok(row.0 > 0)
}
pub async fn create_application(
    db: &SqlitePool,
    job: &Job,
    summary: String,
    bullets: Vec<String>,
) -> anyhow::Result<Application> {
    let now = Utc::now();
    let a = Application {
        id: uuid::Uuid::new_v4().to_string(),
        external_id: job.external_id.clone(),
        job_title: job.title.clone(),
        company: job.company.clone(),
        job_url: job.url.clone(),
        status: "pending_review".into(),
        tailored_summary: summary,
        selected_bullets: bullets,
        notes: String::new(),
        created_at: now,
        updated_at: now,
    };
    sqlx::query("INSERT INTO applications VALUES(?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&a.id)
        .bind(&a.external_id)
        .bind(&a.job_title)
        .bind(&a.company)
        .bind(&a.job_url)
        .bind(&a.status)
        .bind(&a.tailored_summary)
        .bind(serde_json::to_string(&a.selected_bullets)?)
        .bind(&a.notes)
        .bind(a.created_at.to_rfc3339())
        .bind(a.updated_at.to_rfc3339())
        .execute(db)
        .await?;
    Ok(a)
}
pub async fn applications(db: &SqlitePool) -> anyhow::Result<Vec<Application>> {
    let rows=sqlx::query_as::<_,(String,String,String,String,String,String,String,String,String,String,String)>("SELECT id,external_id,job_title,company,job_url,status,tailored_summary,selected_bullets,notes,created_at,updated_at FROM applications ORDER BY created_at DESC").fetch_all(db).await?;
    rows.into_iter()
        .map(|r| {
            Ok(Application {
                id: r.0,
                external_id: r.1,
                job_title: r.2,
                company: r.3,
                job_url: r.4,
                status: r.5,
                tailored_summary: r.6,
                selected_bullets: serde_json::from_str(&r.7)?,
                notes: r.8,
                created_at: r.9.parse()?,
                updated_at: r.10.parse()?,
            })
        })
        .collect()
}
pub async fn update_application(
    db: &SqlitePool,
    id: &str,
    status: &str,
    notes: &str,
) -> anyhow::Result<bool> {
    let result = sqlx::query("UPDATE applications SET status=?,notes=?,updated_at=? WHERE id=?")
        .bind(status)
        .bind(notes)
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() > 0)
}
pub async fn run_started(db: &SqlitePool) -> anyhow::Result<i64> {
    Ok(
        sqlx::query("INSERT INTO runs(started_at,discovered) VALUES(?,0)")
            .bind(Utc::now().to_rfc3339())
            .execute(db)
            .await?
            .last_insert_rowid(),
    )
}
pub async fn run_finished(
    db: &SqlitePool,
    id: i64,
    count: usize,
    error: Option<String>,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE runs SET finished_at=?,discovered=?,error=? WHERE id=?")
        .bind(Utc::now().to_rfc3339())
        .bind(count as i64)
        .bind(error)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}
pub async fn load_search_pages(
    db: &SqlitePool,
    cached_on: &str,
    provider: &str,
    query: &str,
) -> anyhow::Result<Option<Vec<String>>> {
    let rows = sqlx::query_as::<_, (String,)>(
        "SELECT results FROM search_pages
         WHERE cached_on=? AND provider=? AND query=? AND deleted_at IS NULL
         ORDER BY page",
    )
    .bind(cached_on)
    .bind(provider)
    .bind(query)
    .fetch_all(db)
    .await?;
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(rows.into_iter().map(|row| row.0).collect()))
}

pub async fn replace_search_pages(
    db: &SqlitePool,
    cached_on: &str,
    provider: &str,
    query: &str,
    pages: &[String],
) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE search_pages SET deleted_at=?
         WHERE cached_on=? AND provider=? AND query=? AND deleted_at IS NULL",
    )
    .bind(&now)
    .bind(cached_on)
    .bind(provider)
    .bind(query)
    .execute(&mut *tx)
    .await?;
    let stored = if pages.is_empty() {
        vec!["[]".to_string()]
    } else {
        pages.to_vec()
    };
    for (index, results) in stored.iter().enumerate() {
        sqlx::query(
            "INSERT INTO search_pages(cached_on, provider, query, page, results) VALUES(?,?,?,?,?)",
        )
        .bind(cached_on)
        .bind(provider)
        .bind(query)
        .bind((index as i64) + 1)
        .bind(results)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn soft_delete_old_search_pages(db: &SqlitePool, today: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE search_pages SET deleted_at=? WHERE cached_on <> ? AND deleted_at IS NULL")
        .bind(Utc::now().to_rfc3339())
        .bind(today)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn soft_delete_search_pages(db: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query("UPDATE search_pages SET deleted_at=? WHERE deleted_at IS NULL")
        .bind(Utc::now().to_rfc3339())
        .execute(db)
        .await?;
    Ok(())
}

/// Encrypted search keys (SerpApi/Tavily/Brave), sealed via `crate::secrets`.
/// Only the nonce, ciphertext, and last-four hint are stored — never plaintext.
pub async fn save_secret(
    db: &SqlitePool,
    provider: &str,
    nonce_b64: &str,
    ciphertext_b64: &str,
    last4: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO secrets(provider,nonce,ciphertext,last4,updated_at) VALUES(?,?,?,?,?)
         ON CONFLICT(provider) DO UPDATE SET nonce=excluded.nonce, ciphertext=excluded.ciphertext, last4=excluded.last4, updated_at=excluded.updated_at",
    )
    .bind(provider)
    .bind(nonce_b64)
    .bind(ciphertext_b64)
    .bind(last4)
    .bind(Utc::now().to_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn load_secret(
    db: &SqlitePool,
    provider: &str,
) -> anyhow::Result<Option<(String, String, String)>> {
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT nonce,ciphertext,last4 FROM secrets WHERE provider=?",
    )
    .bind(provider)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

pub async fn delete_secret(db: &SqlitePool, provider: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM secrets WHERE provider=?")
        .bind(provider)
        .execute(db)
        .await?;
    Ok(())
}

/// Generic JSON document store. The resume profile and matching preferences
/// live here as JSON payloads instead of loose files, so SQLite is the single
/// source of truth and everything is transactional with the rest of the data.
pub async fn kv_get(db: &SqlitePool, key: &str) -> anyhow::Result<Option<String>> {
    let row = sqlx::query_as::<_, (String,)>("SELECT payload FROM kv_store WHERE key=?")
        .bind(key)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|row| row.0))
}

pub async fn kv_set(db: &SqlitePool, key: &str, payload: &str) -> anyhow::Result<()> {
    // Archive the outgoing value first, so every overwrite is undoable.
    if let Some(previous) = kv_get(db, key).await? {
        if previous != payload {
            let next: i64 = sqlx::query_as::<_, (Option<i64>,)>(
                "SELECT MAX(version) FROM kv_history WHERE key=?",
            )
            .bind(key)
            .fetch_one(db)
            .await?
            .0
            .unwrap_or(0)
                + 1;
            sqlx::query("INSERT INTO kv_history(key,version,payload,created_at) VALUES(?,?,?,?)")
                .bind(key)
                .bind(next)
                .bind(previous)
                .bind(Utc::now().to_rfc3339())
                .execute(db)
                .await?;
            sqlx::query(
                "DELETE FROM kv_history WHERE key=? AND version <= (SELECT MAX(version) - ? FROM kv_history WHERE key=?)",
            )
            .bind(key)
            .bind(KV_HISTORY_LIMIT)
            .bind(key)
            .execute(db)
            .await?;
        }
    }
    sqlx::query(
        "INSERT INTO kv_store(key,payload,updated_at) VALUES(?,?,?)
         ON CONFLICT(key) DO UPDATE SET payload=excluded.payload, updated_at=excluded.updated_at",
    )
    .bind(key)
    .bind(payload)
    .bind(Utc::now().to_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}

/// Persisted rope editing session: snapshot text plus undo/redo op stacks as
/// JSON, so closing the app never loses in-progress source edits.
pub struct RopeDoc {
    pub snapshot: String,
    pub undo_stack: String,
    pub redo_stack: String,
}

pub async fn load_rope_doc(db: &SqlitePool, doc_id: &str) -> anyhow::Result<Option<RopeDoc>> {
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT snapshot,undo_stack,redo_stack FROM rope_docs WHERE doc_id=?",
    )
    .bind(doc_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| RopeDoc {
        snapshot: row.0,
        undo_stack: row.1,
        redo_stack: row.2,
    }))
}

pub async fn save_rope_doc(
    db: &SqlitePool,
    doc_id: &str,
    snapshot: &str,
    undo_stack: &str,
    redo_stack: &str,
) -> anyhow::Result<()> {
    let snapshot: String = snapshot.chars().take(200_000).collect();
    sqlx::query(
        "INSERT INTO rope_docs(doc_id,snapshot,undo_stack,redo_stack,updated_at) VALUES(?,?,?,?,?)
         ON CONFLICT(doc_id) DO UPDATE SET snapshot=excluded.snapshot, undo_stack=excluded.undo_stack, redo_stack=excluded.redo_stack, updated_at=excluded.updated_at",
    )
    .bind(doc_id)
    .bind(snapshot)
    .bind(undo_stack.chars().take(200_000).collect::<String>())
    .bind(redo_stack.chars().take(200_000).collect::<String>())
    .bind(Utc::now().to_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn delete_rope_doc(db: &SqlitePool, doc_id: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM rope_docs WHERE doc_id=?")
        .bind(doc_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Maximum retained versions per key. Restores themselves create new
/// versions, so history is never destroyed by rolling back.
pub const KV_HISTORY_LIMIT: i64 = 20;

pub async fn kv_history(db: &SqlitePool, key: &str) -> anyhow::Result<Vec<(i64, String)>> {
    let rows = sqlx::query_as::<_, (i64, String)>(
        "SELECT version,created_at FROM kv_history WHERE key=? ORDER BY version DESC",
    )
    .bind(key)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn kv_version(
    db: &SqlitePool,
    key: &str,
    version: i64,
) -> anyhow::Result<Option<String>> {
    let row =
        sqlx::query_as::<_, (String,)>("SELECT payload FROM kv_history WHERE key=? AND version=?")
            .bind(key)
            .bind(version)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|row| row.0))
}

/// Restore a historical payload as the new current value. The pre-restore
/// current value is archived first, so restoring is itself undoable.
pub async fn kv_restore(db: &SqlitePool, key: &str, version: i64) -> anyhow::Result<String> {
    let Some(payload) = kv_version(db, key, version).await? else {
        anyhow::bail!("No version {version} for {key}");
    };
    kv_set(db, key, &payload).await?;
    Ok(payload)
}
pub const CANDIDATE_PROFILE_KEY: &str = "candidate_profile";
pub const PREFERENCES_KEY: &str = "preferences";

/// One-time migration: import the legacy loose JSON files into the DB store
/// when the DB does not have them yet. Files are left in place as backups.
pub async fn import_legacy_json_files(db: &SqlitePool) -> anyhow::Result<()> {
    for (key, path) in [
        (CANDIDATE_PROFILE_KEY, "candidate_profile.json"),
        (PREFERENCES_KEY, "data/preferences.json"),
    ] {
        if kv_get(db, key).await?.is_some() {
            continue;
        }
        if let Ok(bytes) = tokio::fs::read(path).await {
            if serde_json::from_slice::<serde_json::Value>(&bytes).is_ok() {
                kv_set(db, key, &String::from_utf8_lossy(&bytes)).await?;
                tracing::info!(key, path, "imported legacy JSON file into the database");
            }
        }
    }
    Ok(())
}

pub async fn load_candidate_profile(
    db: &SqlitePool,
) -> anyhow::Result<Option<crate::resume::CandidateProfile>> {
    let Some(payload) = kv_get(db, CANDIDATE_PROFILE_KEY).await? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&payload).ok())
}

pub async fn save_candidate_profile(
    db: &SqlitePool,
    profile: &crate::resume::CandidateProfile,
) -> anyhow::Result<()> {
    kv_set(
        db,
        CANDIDATE_PROFILE_KEY,
        &serde_json::to_string_pretty(profile)?,
    )
    .await
}

pub async fn load_preferences(
    db: &SqlitePool,
) -> anyhow::Result<Option<crate::models::CandidateProfile>> {
    let Some(payload) = kv_get(db, PREFERENCES_KEY).await? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&payload).ok())
}

pub async fn save_preferences(
    db: &SqlitePool,
    prefs: &crate::models::CandidateProfile,
) -> anyhow::Result<()> {
    kv_set(db, PREFERENCES_KEY, &serde_json::to_string_pretty(prefs)?).await
}

/// Company knowledge-base row: user notes (process intel, known questions)
/// plus the cached study guide. Created on demand; never deleted implicitly.
pub async fn load_company_profile(
    db: &SqlitePool,
    key: &str,
) -> anyhow::Result<Option<(String, String, String)>> {
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT display_name,notes,guide_payload FROM company_profiles WHERE company_key=?",
    )
    .bind(key)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

pub async fn save_company_profile(
    db: &SqlitePool,
    key: &str,
    display_name: &str,
    notes: &str,
    guide_payload: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO company_profiles(company_key,display_name,notes,guide_payload,updated_at) VALUES(?,?,?,?,?)
         ON CONFLICT(company_key) DO UPDATE SET display_name=excluded.display_name, notes=excluded.notes, guide_payload=excluded.guide_payload, updated_at=excluded.updated_at",
    )
    .bind(key)
    .bind(display_name)
    .bind(notes)
    .bind(guide_payload)
    .bind(Utc::now().to_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}
/// Cached per-job interview prep sheet (JSON payload).
pub async fn load_prep_sheet(db: &SqlitePool, external_id: &str) -> anyhow::Result<Option<String>> {
    let row = sqlx::query_as::<_, (String,)>("SELECT payload FROM prep_sheets WHERE external_id=?")
        .bind(external_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|row| row.0))
}

pub async fn save_prep_sheet(
    db: &SqlitePool,
    external_id: &str,
    payload: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO prep_sheets(external_id,payload,updated_at) VALUES(?,?,?)
         ON CONFLICT(external_id) DO UPDATE SET payload=excluded.payload, updated_at=excluded.updated_at",
    )
    .bind(external_id)
    .bind(payload)
    .bind(Utc::now().to_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}
/// Removes rows that were soft-deleted at least seven days ago.
pub async fn purge_expired_search_pages(db: &SqlitePool) -> anyhow::Result<u64> {
    let cutoff = (Utc::now() - chrono::Duration::days(7)).to_rfc3339();
    let result =
        sqlx::query("DELETE FROM search_pages WHERE deleted_at IS NOT NULL AND deleted_at < ?")
            .bind(cutoff)
            .execute(db)
            .await?;
    Ok(result.rows_affected())
}

/// Full page text cached by enrich.rs. Fresh for 30 days so repeated discovery
/// runs do not re-fetch the same posting.
pub async fn page_text(db: &SqlitePool, url: &str) -> anyhow::Result<Option<String>> {
    let row = sqlx::query_as::<_, (String,)>(
        "SELECT text FROM page_texts WHERE url=? AND fetched_at > datetime('now','-30 days')",
    )
    .bind(url)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| row.0))
}

pub async fn save_page_text(db: &SqlitePool, url: &str, text: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO page_texts(url,text,fetched_at) VALUES(?,?,?)
         ON CONFLICT(url) DO UPDATE SET text=excluded.text, fetched_at=excluded.fetched_at",
    )
    .bind(url)
    .bind(text)
    .bind(Utc::now().to_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn latest_run(db: &SqlitePool) -> anyhow::Result<Option<serde_json::Value>> {
    let r = sqlx::query_as::<_, (String, Option<String>, i64, Option<String>)>(
        "SELECT started_at,finished_at,discovered,error FROM runs ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    Ok(r.map(
        |x| serde_json::json!({"started_at":x.0,"finished_at":x.1,"discovered":x.2,"error":x.3}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_db() -> SqlitePool {
        let path = std::env::temp_dir().join(format!(
            "rust-job-agent-kv-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let url = format!(
            "sqlite://{}?mode=rwc",
            path.to_string_lossy().replace('\\', "/")
        );
        let db = connect(&url).await.unwrap();
        migrate(&db).await.unwrap();
        db
    }

    #[tokio::test]
    async fn kv_store_roundtrips_json() {
        let db = temp_db().await;
        assert!(kv_get(&db, "missing").await.unwrap().is_none());
        kv_set(&db, "doc", r#"{"a":1}"#).await.unwrap();
        assert_eq!(
            kv_get(&db, "doc").await.unwrap().as_deref(),
            Some(r#"{"a":1}"#)
        );
        kv_set(&db, "doc", r#"{"a":2}"#).await.unwrap();
        assert_eq!(
            kv_get(&db, "doc").await.unwrap().as_deref(),
            Some(r#"{"a":2}"#)
        );
    }

    #[tokio::test]
    async fn kv_history_versions_and_restores() {
        let db = temp_db().await;
        kv_set(&db, "doc", r#"{"v":1}"#).await.unwrap();
        // Identical rewrites do not create versions.
        kv_set(&db, "doc", r#"{"v":1}"#).await.unwrap();
        assert!(kv_history(&db, "doc").await.unwrap().is_empty());
        kv_set(&db, "doc", r#"{"v":2}"#).await.unwrap();
        kv_set(&db, "doc", r#"{"v":3}"#).await.unwrap();
        let history = kv_history(&db, "doc").await.unwrap();
        assert_eq!(history.len(), 2);
        assert!(history[0].0 > history[1].0, "newest first");
        // Restore archives the current value first, so nothing is lost.
        let restored = kv_restore(&db, "doc", history[1].0).await.unwrap();
        assert_eq!(restored, r#"{"v":1}"#);
        assert_eq!(
            kv_get(&db, "doc").await.unwrap().as_deref(),
            Some(r#"{"v":1}"#)
        );
        assert_eq!(kv_history(&db, "doc").await.unwrap().len(), 3);
        assert!(kv_version(&db, "doc", 9999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rope_docs_roundtrip_and_forget() {
        let db = temp_db().await;
        assert!(load_rope_doc(&db, "resume-source").await.unwrap().is_none());
        let undo = serde_json::json!([[{ "t": "insert", "pos": 0, "text": "# " }]]).to_string();
        save_rope_doc(&db, "resume-source", "# Ada", &undo, "[]")
            .await
            .unwrap();
        let doc = load_rope_doc(&db, "resume-source")
            .await
            .unwrap()
            .expect("saved doc");
        assert_eq!(doc.snapshot, "# Ada");
        assert!(doc.undo_stack.contains("insert"));
        assert_eq!(doc.redo_stack, "[]");
        delete_rope_doc(&db, "resume-source").await.unwrap();
        assert!(load_rope_doc(&db, "resume-source").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn preferences_live_in_the_db() {
        let db = temp_db().await;
        assert!(load_preferences(&db).await.unwrap().is_none());
        let mut prefs = crate::models::CandidateProfile {
            name: "Test".into(),
            summary: String::new(),
            skills: vec!["Rust".into()],
            experience_years: 4.0,
            preferred_titles: vec![],
            role_keywords: vec![],
            preferred_locations: vec![],
            excluded_keywords: vec![],
            minimum_match_score: 60.0,
            projects: vec![],
            preferred_work_modes: vec![],
            auto_apply: false,
            email: "t@example.com".into(),
            phone: String::new(),
            skill_weights: vec![],
        };
        save_preferences(&db, &prefs).await.unwrap();
        prefs.experience_years = 5.0;
        save_preferences(&db, &prefs).await.unwrap();
        let loaded = load_preferences(&db).await.unwrap().expect("saved prefs");
        assert_eq!(loaded.experience_years, 5.0);
        assert_eq!(loaded.skills, vec!["Rust".to_string()]);
    }
}
