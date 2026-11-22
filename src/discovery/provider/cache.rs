use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedSearchHit {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
    pub published_at: Option<String>,
}

pub fn today_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

pub async fn load_pages(
    db: &SqlitePool,
    today: &str,
    provider: &str,
    query: &str,
) -> anyhow::Result<Option<Vec<Vec<CachedSearchHit>>>> {
    let Some(rows) = crate::store::load_search_pages(db, today, provider, query).await? else {
        return Ok(None);
    };
    let mut pages = Vec::with_capacity(rows.len());
    for results in rows {
        pages.push(serde_json::from_str(&results)?);
    }
    Ok(Some(pages))
}

pub async fn store_pages(
    db: &SqlitePool,
    today: &str,
    provider: &str,
    query: &str,
    pages: &[Vec<CachedSearchHit>],
) -> anyhow::Result<()> {
    let encoded = pages
        .iter()
        .map(|page| serde_json::to_string(page).unwrap_or_else(|_| "[]".to_string()))
        .collect::<Vec<_>>();
    crate::store::replace_search_pages(db, today, provider, query, &encoded).await
}

pub async fn soft_delete_other_days(db: &SqlitePool, today: &str) -> anyhow::Result<()> {
    crate::store::soft_delete_old_search_pages(db, today).await
}

pub async fn clear_search_cache(db: &SqlitePool) -> anyhow::Result<()> {
    crate::store::soft_delete_search_pages(db).await
}

pub fn chunk_hits(hits: Vec<CachedSearchHit>, page_size: usize) -> Vec<Vec<CachedSearchHit>> {
    let page_size = page_size.max(1);
    if hits.is_empty() {
        return vec![Vec::new()];
    }
    hits.chunks(page_size).map(|page| page.to_vec()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(url: &str) -> CachedSearchHit {
        CachedSearchHit {
            title: url.into(),
            url: url.into(),
            snippet: None,
            published_at: None,
        }
    }

    #[test]
    fn splits_results_into_indexed_pages() {
        let pages = chunk_hits(
            (1..=5)
                .map(|n| hit(&format!("https://example.com/{n}")))
                .collect(),
            2,
        );
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].len(), 2);
        assert_eq!(pages[2].len(), 1);
        assert_eq!(pages[2][0].url, "https://example.com/5");
    }

    #[tokio::test]
    async fn pages_are_indexed_and_old_days_are_soft_deleted() {
        let path = std::env::temp_dir().join(format!(
            "rust-job-agent-pages-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let url = format!(
            "sqlite://{}?mode=rwc",
            path.to_string_lossy().replace('\\', "/")
        );
        let db = crate::store::connect(&url).await.unwrap();
        crate::store::migrate(&db).await.unwrap();
        let pages = chunk_hits(
            vec![hit("https://example.com/1"), hit("https://example.com/2")],
            1,
        );
        store_pages(&db, "2026-11-14", "exa", "old query", &pages)
            .await
            .unwrap();
        store_pages(&db, "2026-11-15", "tavily", "today query", &pages)
            .await
            .unwrap();
        soft_delete_other_days(&db, "2026-11-15").await.unwrap();
        assert!(load_pages(&db, "2026-11-14", "exa", "old query")
            .await
            .unwrap()
            .is_none());
        let retired = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM search_pages WHERE deleted_at IS NOT NULL",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(retired, 2);
        let loaded = load_pages(&db, "2026-11-15", "tavily", "today query")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1][0].url, "https://example.com/2");
        let old = (chrono::Utc::now() - chrono::Duration::days(8)).to_rfc3339();
        sqlx::query("UPDATE search_pages SET deleted_at=? WHERE cached_on='2026-11-14'")
            .bind(old)
            .execute(&db)
            .await
            .unwrap();
        let removed = crate::store::purge_expired_search_pages(&db).await.unwrap();
        assert_eq!(removed, 2);
        assert!(load_pages(&db, "2026-11-15", "tavily", "today query")
            .await
            .unwrap()
            .is_some());
        clear_search_cache(&db).await.unwrap();
        assert!(load_pages(&db, "2026-11-15", "tavily", "today query")
            .await
            .unwrap()
            .is_none());
        let still_there = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM search_pages")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(still_there, 2);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }
}
