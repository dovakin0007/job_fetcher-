//! Recruiter-email outcome sync.
//!
//! Reads the mailbox over IMAP (Gmail app password recommended), matches
//! replies to tracked applications by company name, and advances their
//! status: `interview`, `offer`, or `rejected`. Status only ever moves
//! forward — an older rejection never overwrites an `offer`.
//!
//! Config (env): `EMAIL_IMAP_HOST` (default `imap.gmail.com`),
//! `EMAIL_IMAP_PORT` (default 993), `EMAIL_IMAP_USER`, `EMAIL_IMAP_PASS`.
//! Last-sync state lives in `kv_store:email_sync_state` so each run only
//! scans mail since the previous one.

use mailparse::MailHeaderMap;
use sqlx::SqlitePool;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Interview,
    Offer,
    Rejected,
}

#[derive(Debug, Clone)]
pub struct EmailHit {
    pub from: String,
    pub subject: String,
    pub date: String,
    pub snippet: String,
    pub outcome: Outcome,
}

#[derive(Debug, Default)]
pub struct SyncReport {
    pub checked: usize,
    pub matched: usize,
    pub updated: Vec<SyncUpdate>,
    pub skipped_no_config: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncUpdate {
    pub application_id: String,
    pub company: String,
    pub from: String,
    pub to: String,
}

/// Forward-only transition: returns the new status, or `None` to keep `current`.
pub fn next_status(current: &str, outcome: &Outcome) -> Option<&'static str> {
    match outcome {
        Outcome::Offer => {
            if current == "offer" {
                None
            } else {
                Some("offer")
            }
        }
        Outcome::Interview => match current {
            "pending_review" | "approved" | "applied" | "applied_manually" => {
                Some("interview")
            }
            _ => None,
        },
        Outcome::Rejected => match current {
            "offer" | "selected" | "rejected" | "withdrawn" => None,
            _ => Some("rejected"),
        },
    }
}

fn outcome_keywords() -> &'static [(&'static str, Outcome)] {
    &[
        ("not moving forward", Outcome::Rejected),
        ("decided not to move forward", Outcome::Rejected),
        ("decided to move forward with other", Outcome::Rejected),
        ("pursuing other candidates", Outcome::Rejected),
        ("other candidates whose", Outcome::Rejected),
        ("regret to inform", Outcome::Rejected),
        ("unfortunately, we will not", Outcome::Rejected),
        ("unfortunately we will not", Outcome::Rejected),
        ("not selected", Outcome::Rejected),
        ("no longer under consideration", Outcome::Rejected),
        ("position has been filled", Outcome::Rejected),
        ("pleased to offer", Outcome::Offer),
        ("we would like to offer", Outcome::Offer),
        ("offer letter", Outcome::Offer),
        ("job offer", Outcome::Offer),
        ("welcome aboard", Outcome::Offer),
        ("congratulations! you", Outcome::Offer),
        ("congratulations, you", Outcome::Offer),
        ("next steps", Outcome::Interview),
        ("schedule a call", Outcome::Interview),
        ("schedule an interview", Outcome::Interview),
        ("invite you to interview", Outcome::Interview),
        ("phone screen", Outcome::Interview),
        ("coding challenge", Outcome::Interview),
        ("technical assessment", Outcome::Interview),
        ("take-home", Outcome::Interview),
        ("onsite", Outcome::Interview),
        ("video call", Outcome::Interview),
        ("meet the team", Outcome::Interview),
        ("your availability", Outcome::Interview),
        ("interview availability", Outcome::Interview),
    ]
}

/// Classify a recruiter mail. Offer phrases win over interview ones, which
/// win over rejections (an offer mail often quotes the interview thread).
pub fn classify_outcome(subject: &str, body: &str) -> Option<Outcome> {
    let text = format!("{subject}\n{body}").to_lowercase();
    let mut found: Option<&Outcome> = None;
    let mut rank = 0u8;
    for (phrase, outcome) in outcome_keywords() {
        if text.contains(phrase) {
            let next_rank = match outcome {
                Outcome::Offer => 3,
                Outcome::Interview => 2,
                Outcome::Rejected => 1,
            };
            if next_rank >= rank {
                rank = next_rank;
                found = Some(outcome);
            }
        }
    }
    found.cloned()
}

fn words(value: &str) -> Vec<String> {
    value
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// Company tokens that must all appear in the mail (from/subject/body) for a
/// match. Short tokens (<3 chars) are dropped to avoid matching "at", "co".
pub fn company_tokens(company: &str) -> Vec<String> {
    let stop = ["inc", "llc", "ltd", "labs", "technologies", "technology", "systems"];
    words(company)
        .into_iter()
        .filter(|token| token.len() >= 3 && !stop.contains(&token.as_str()))
        .collect()
}

/// Mail senders that are job boards / alerts, never recruiter outcomes.
fn is_board_sender(from: &str) -> bool {
    let from = from.to_lowercase();
    [
        "linkedin.com",
        "indeed.com",
        "naukri.com",
        "glassdoor.com",
        "ziprecruiter.com",
        "monster.com",
        "simplyhired.com",
        "jobalert",
        "noreply@jobs",
    ]
    .iter()
    .any(|needle| from.contains(needle))
}

pub fn mail_matches_company(from: &str, subject: &str, body: &str, company: &str) -> bool {
    if is_board_sender(from) {
        return false;
    }
    let tokens = company_tokens(company);
    if tokens.is_empty() {
        return false;
    }
    let haystack = format!("{from}\n{subject}\n{body}").to_lowercase();
    let hay_words = words(&haystack);
    tokens.iter().all(|token| hay_words.iter().any(|word| word == token))
}

fn imap_config() -> Option<(String, u16, String, String)> {
    let user = std::env::var("EMAIL_IMAP_USER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    let pass = std::env::var("EMAIL_IMAP_PASS")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    let host = std::env::var("EMAIL_IMAP_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "imap.gmail.com".to_string());
    let port = std::env::var("EMAIL_IMAP_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(993);
    Some((host, port, user, pass))
}

fn since_query(state: Option<&str>) -> String {
    // IMAP SINCE takes `DD-Mon-YYYY`. Default to a 30-day lookback.
    let fallback = (chrono::Utc::now() - chrono::Duration::days(30)).format("%d-%b-%Y");
    let date = state
        .and_then(|raw| raw.split('|').next())
        .filter(|date| !date.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string());
    format!("SINCE {date}")
}

/// Scan the inbox for recruiter outcomes and advance matching applications.
/// Prefers Gmail OAuth (SSO) when connected, else IMAP app-password.
/// Runs the blocking IMAP session on a worker thread.
pub async fn sync_outcomes(db: &SqlitePool) -> anyhow::Result<SyncReport> {
    let state = crate::store::kv_get(db, "email_sync_state").await?;
    let messages = if oauth_connected(db).await {
        fetch_messages_gmail(db, state.as_deref()).await?
    } else if imap_config().is_some() {
        let query = since_query(state.as_deref());
        tokio::task::spawn_blocking(move || fetch_messages(query))
            .await
            .map_err(|error| anyhow::anyhow!("email scan task failed: {error}"))??
    } else {
        return Ok(SyncReport {
            skipped_no_config: true,
            ..Default::default()
        });
    };
    let applications = crate::store::applications(db).await?;
    let mut notes_by_id: HashMap<String, String> = HashMap::new();
    for application in &applications {
        notes_by_id.insert(application.id.clone(), application.notes.clone());
    }
    let mut report = SyncReport {
        checked: messages.len(),
        ..Default::default()
    };
    // Oldest first so the latest outcome wins per application.
    for hit in &messages {
        for application in &applications {
            if !mail_matches_company(&hit.from, &hit.subject, &hit.snippet, &application.company)
            {
                continue;
            }
            let Some(next) = next_status(&application.status, &hit.outcome) else {
                continue;
            };
            report.matched += 1;
            let existing = notes_by_id.get(&application.id).cloned().unwrap_or_default();
            let entry = format!(
                "[email {}] {} — {}",
                hit.date,
                hit.subject.chars().take(120).collect::<String>(),
                hit.from.chars().take(80).collect::<String>()
            );
            let notes = if existing.contains(&entry) {
                existing.clone()
            } else if existing.trim().is_empty() {
                entry.clone()
            } else {
                format!("{existing}\n{entry}")
            };
            if crate::store::update_application(db, &application.id, next, &notes).await? {
                notes_by_id.insert(application.id.clone(), notes);
                report.updated.push(SyncUpdate {
                    application_id: application.id.clone(),
                    company: application.company.clone(),
                    from: hit.from.clone(),
                    to: next.to_string(),
                });
                tracing::info!(
                    company = %application.company,
                    from = %hit.from,
                    status = next,
                    "email outcome advanced an application"
                );
            }
            break;
        }
    }
    let stamp = format!(
        "{}|{}",
        chrono::Utc::now().format("%d-%b-%Y"),
        chrono::Utc::now().to_rfc3339()
    );
    if let Err(error) = crate::store::kv_set(db, "email_sync_state", &stamp).await {
        tracing::warn!(%error, "email sync state was not saved");
    }
    Ok(report)
}

#[derive(Debug, Clone)]
struct RawMessage {
    from: String,
    subject: String,
    date: String,
    body: String,
}

fn fetch_messages(query: String) -> anyhow::Result<Vec<EmailHit>> {
    let Some((host, port, user, pass)) = imap_config() else {
        return Ok(Vec::new());
    };
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|error| anyhow::anyhow!("email TLS setup failed: {error}"))?;
    let client = imap::connect((host.as_str(), port), host.as_str(), &tls)
        .map_err(|error| anyhow::anyhow!("email IMAP connect failed: {error}"))?;
    let mut session = client
        .login(&user, &pass)
        .map_err(|error| anyhow::anyhow!("email IMAP login failed: {}", error.0))?;
    session
        .select("INBOX")
        .map_err(|error| anyhow::anyhow!("email INBOX select failed: {error}"))?;
    let uids = session
        .uid_search(&query)
        .map_err(|error| anyhow::anyhow!("email search failed: {error}"))?;
    // Bound the work: newest 100 messages only.
    let take: Vec<u32> = {
        let mut sorted: Vec<u32> = uids.into_iter().collect();
        sorted.sort_unstable();
        let at = sorted.len().saturating_sub(100);
        sorted[at..].to_vec()
    };
    let mut raw: Vec<RawMessage> = Vec::new();
    for chunk in take.chunks(25) {
        let seq: Vec<String> = chunk.iter().map(u32::to_string).collect();
        let fetched = session
            .uid_fetch(seq.join(","), "RFC822")
            .map_err(|error| anyhow::anyhow!("email fetch failed: {error}"))?;
        for message in fetched.iter() {
            let Some(bytes) = message.body() else { continue };
            match parse_message(bytes) {
                Ok(parsed) => raw.push(parsed),
                Err(error) => tracing::warn!(%error, "email parse failed; skipping"),
            }
        }
    }
    session
        .logout()
        .map_err(|error| anyhow::anyhow!("email logout failed: {error}"))?;
    // Oldest first so latest outcome wins.
    let mut hits = Vec::new();
    for message in &raw {
        if let Some(outcome) = classify_outcome(&message.subject, &message.body) {
            hits.push(EmailHit {
                from: message.from.clone(),
                subject: message.subject.clone(),
                date: message.date.clone(),
                snippet: message.body.chars().take(4_000).collect(),
                outcome,
            });
        }
    }
    Ok(hits)
}

fn parse_message(bytes: &[u8]) -> anyhow::Result<RawMessage> {
    let parsed = mailparse::parse_mail(bytes)
        .map_err(|error| anyhow::anyhow!("email parse failed: {error}"))?;
    let headers = &parsed.headers;
    let from = headers
        .get_first_value("From")
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    let subject = headers
        .get_first_value("Subject")
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect();
    let date = headers
        .get_first_value("Date")
        .unwrap_or_default()
        .chars()
        .take(80)
        .collect();
    let body = parsed
        .get_body()
        .unwrap_or_default()
        .chars()
        .take(20_000)
        .collect();
    Ok(RawMessage {
        from,
        subject,
        date,
        body,
    })
}

/// ---- Gmail OAuth (SSO) ----
///
/// One-time setup by the user: Google Cloud → APIs & Services → Credentials →
/// Create "OAuth client ID" of type **Desktop app**, enable the Gmail API,
/// then put the ID + secret in `GOOGLE_OAUTH_CLIENT_ID` /
/// `GOOGLE_OAUTH_CLIENT_SECRET`. Desktop clients may use a loopback redirect
/// without pre-registering it, so the callback below just works.
/// Only the read-only scope is requested.

pub const GMAIL_OAUTH_SECRET_KEY: &str = "gmail_oauth";
const GMAIL_OAUTH_SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";

pub fn oauth_redirect_uri() -> String {
    "http://127.0.0.1:8787/api/email/oauth/callback".to_string()
}

pub fn oauth_client_config() -> Option<(String, String)> {
    let id = std::env::var("GOOGLE_OAUTH_CLIENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value != "replace-me")?;
    let secret = std::env::var("GOOGLE_OAUTH_CLIENT_SECRET")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value != "replace-me")?;
    Some((id, secret))
}

/// Google consent URL. `access_type=offline` + `prompt=consent` guarantee a
/// refresh token on every connect (re-connects included).
pub fn oauth_start_url() -> Option<String> {
    let (id, _) = oauth_client_config()?;
    let params = [
        ("client_id", id.as_str()),
        ("redirect_uri", &oauth_redirect_uri()),
        ("response_type", "code"),
        ("scope", GMAIL_OAUTH_SCOPE),
        ("access_type", "offline"),
        ("prompt", "consent"),
    ];
    let query: Vec<String> = params
        .iter()
        .map(|(key, value)| {
            format!(
                "{key}={}",
                url_encode(value)
            )
        })
        .collect();
    Some(format!(
        "https://accounts.google.com/o/oauth2/v2/auth?{}",
        query.join("&")
    ))
}

fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub async fn save_refresh_token(db: &SqlitePool, refresh_token: &str) -> anyhow::Result<()> {
    let sealed = crate::secrets::seal(refresh_token)?;
    crate::store::save_secret(
        db,
        GMAIL_OAUTH_SECRET_KEY,
        &sealed.nonce_b64,
        &sealed.ciphertext_b64,
        &sealed.last4,
    )
    .await
}

pub async fn oauth_connected(db: &SqlitePool) -> bool {
    match crate::store::load_secret(db, GMAIL_OAUTH_SECRET_KEY).await {
        Ok(Some((nonce, ciphertext, _))) => crate::secrets::open(&nonce, &ciphertext)
            .map(|token| !token.trim().is_empty())
            .unwrap_or(false),
        _ => false,
    }
}

pub async fn disconnect_oauth(db: &SqlitePool) -> anyhow::Result<()> {
    crate::store::delete_secret(db, GMAIL_OAUTH_SECRET_KEY).await
}

async fn load_refresh_token(db: &SqlitePool) -> anyhow::Result<Option<String>> {
    let Ok(Some((nonce, ciphertext, _))) =
        crate::store::load_secret(db, GMAIL_OAUTH_SECRET_KEY).await
    else {
        return Ok(None);
    };
    Ok(Some(crate::secrets::open(&nonce, &ciphertext)?))
}

async fn gmail_access_token(db: &SqlitePool) -> anyhow::Result<String> {
    let Some(refresh_token) = load_refresh_token(db).await? else {
        anyhow::bail!("Gmail is not connected; connect it from the Applications panel first");
    };
    let Some((client_id, client_secret)) = oauth_client_config() else {
        anyhow::bail!("GOOGLE_OAUTH_CLIENT_ID / GOOGLE_OAUTH_CLIENT_SECRET are not set");
    };
    let response = reqwest::Client::new()
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("refresh_token", refresh_token.trim()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .map_err(|error| anyhow::anyhow!("Gmail token refresh failed: {error}"))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| anyhow::anyhow!("Gmail token reply was not JSON: {error}"))?;
    if !status.is_success() {
        let detail = body
            .get("error_description")
            .or_else(|| body.get("error"))
            .and_then(|value| value.as_str())
            .unwrap_or("token refresh failed");
        anyhow::bail!("Gmail token refresh failed ({status}): {detail}");
    }
    body.get("access_token")
        .and_then(|value| value.as_str())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Gmail token reply had no access_token"))
}

fn gmail_since_query(state: Option<&str>) -> String {
    let fallback = (chrono::Utc::now() - chrono::Duration::days(30)).format("%Y/%m/%d");
    let date = state
        .and_then(|raw| raw.split('|').next())
        .filter(|part| !part.trim().is_empty())
        .and_then(|part| {
            chrono::NaiveDate::parse_from_str(part.trim(), "%d-%b-%Y")
                .ok()
                .map(|day| day.format("%Y/%m/%d").to_string())
        })
        .unwrap_or_else(|| fallback.to_string());
    format!("after:{date}")
}

fn decode_b64url(value: &str) -> Option<String> {
    use base64::Engine;
    let cleaned: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&cleaned)
        .or_else(|_| {
            // Tolerate padded / standard-alphabet variants.
            let mut alt = cleaned.replace('-', "+").replace('_', "/");
            while alt.len() % 4 != 0 {
                alt.push('=');
            }
            base64::engine::general_purpose::STANDARD.decode(&alt)
        })
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

fn gmail_header(headers: &[serde_json::Value], name: &str) -> String {
    headers
        .iter()
        .find(|header| {
            header
                .get("name")
                .and_then(|value| value.as_str())
                .is_some_and(|key| key.eq_ignore_ascii_case(name))
        })
        .and_then(|header| header.get("value"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect()
}

fn gmail_text_part(part: &serde_json::Value, out: &mut String) {
    if out.len() >= 20_000 {
        return;
    }
    let mime = part
        .get("mimeType")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if mime.starts_with("text/plain") {
        if let Some(data) = part
            .get("body")
            .and_then(|body| body.get("data"))
            .and_then(|value| value.as_str())
        {
            if let Some(text) = decode_b64url(data) {
                out.push_str(&text);
                out.push('\n');
            }
        }
    }
    if let Some(parts) = part.get("parts").and_then(|value| value.as_array()) {
        for child in parts {
            gmail_text_part(child, out);
            if out.len() >= 20_000 {
                break;
            }
        }
    }
}

async fn fetch_messages_gmail(
    db: &SqlitePool,
    state: Option<&str>,
) -> anyhow::Result<Vec<EmailHit>> {
    let token = gmail_access_token(db).await?;
    let client = reqwest::Client::new();
    let query = gmail_since_query(state);
    // Newest 200 message IDs max (two pages).
    let mut ids: Vec<String> = Vec::new();
    let mut page: Option<String> = None;
    for _ in 0..2 {
        let mut request = client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
            .bearer_auth(&token)
            .query(&[("q", query.as_str()), ("maxResults", "100")]);
        if let Some(next) = &page {
            request = request.query(&[("pageToken", next.as_str())]);
        }
        let response = request
            .send()
            .await
            .map_err(|error| anyhow::anyhow!("Gmail list failed: {error}"))?;
        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|error| anyhow::anyhow!("Gmail list was not JSON: {error}"))?;
        if !status.is_success() {
            let detail = body
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(|value| value.as_str())
                .unwrap_or("message list failed");
            anyhow::bail!("Gmail list failed ({status}): {detail}");
        }
        let batch: Vec<String> = body
            .get("messages")
            .and_then(|value| value.as_array())
            .map(|messages| {
                messages
                    .iter()
                    .filter_map(|message| message.get("id")?.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        ids.extend(batch);
        page = body
            .get("nextPageToken")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        if page.is_none() {
            break;
        }
    }
    // Full bodies, oldest roughly first (list order is newest-first).
    let mut hits = Vec::new();
    for id in ids.iter().rev() {
        let response = client
            .get(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}"
            ))
            .bearer_auth(&token)
            .query(&[("format", "full")])
            .send()
            .await
            .map_err(|error| anyhow::anyhow!("Gmail get failed: {error}"))?;
        if !response.status().is_success() {
            continue;
        }
        let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
        let payload = body.get("payload").cloned().unwrap_or(serde_json::Value::Null);
        let headers = payload
            .get("headers")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let from = gmail_header(&headers, "From");
        let subject = gmail_header(&headers, "Subject");
        let date = gmail_header(&headers, "Date");
        let mut text = String::new();
        gmail_text_part(&payload, &mut text);
        let snippet_source = if text.trim().is_empty() {
            body.get("snippet")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string()
        } else {
            text
        };
        if let Some(outcome) = classify_outcome(&subject, &snippet_source) {
            hits.push(EmailHit {
                from,
                subject,
                date,
                snippet: snippet_source.chars().take(4_000).collect(),
                outcome,
            });
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_recruiter_outcomes() {
        assert_eq!(
            classify_outcome("Thanks", "Unfortunately we will not be moving forward."),
            Some(Outcome::Rejected)
        );
        assert_eq!(
            classify_outcome("Next steps", "We'd like to schedule a call next week."),
            Some(Outcome::Interview)
        );
        assert_eq!(
            classify_outcome("Offer", "We are pleased to offer you the role."),
            Some(Outcome::Offer)
        );
        assert!(classify_outcome("Weekly digest", "New jobs in Bengaluru.").is_none());
    }

    #[test]
    fn offer_wins_over_quoted_interview_thread() {
        assert_eq!(
            classify_outcome(
                "Offer letter",
                "Original thread: schedule a call. We are pleased to offer you the role."
            ),
            Some(Outcome::Offer)
        );
    }

    #[test]
    fn company_matching_ignores_boards_and_short_tokens() {
        assert!(mail_matches_company(
            "Priya <priya@acme.co>",
            "Interview invite",
            "Hi, Acme would like to meet you.",
            "Acme Technologies Inc"
        ));
        assert!(!mail_matches_company(
            "alerts@linkedin.com",
            "Acme jobs",
            "New Acme postings.",
            "Acme Technologies Inc"
        ));
        assert!(!mail_matches_company(
            "hr@other.com",
            "Hello",
            "Thanks for applying.",
            "Acme Technologies Inc"
        ));
    }

    #[test]
    fn status_moves_only_forward() {
        assert_eq!(next_status("applied", &Outcome::Interview), Some("interview"));
        assert_eq!(next_status("interview", &Outcome::Interview), None);
        assert_eq!(next_status("applied", &Outcome::Rejected), Some("rejected"));
        assert_eq!(next_status("offer", &Outcome::Rejected), None);
        assert_eq!(next_status("rejected", &Outcome::Interview), None);
        assert_eq!(next_status("rejected", &Outcome::Offer), Some("offer"));
    }

    #[test]
    fn gmail_oauth_url_and_helpers() {
        std::env::set_var("GOOGLE_OAUTH_CLIENT_ID", "test-id.apps.googleusercontent.com");
        std::env::set_var("GOOGLE_OAUTH_CLIENT_SECRET", "test-secret");
        let url = oauth_start_url().expect("oauth url");
        assert!(url.contains("accounts.google.com/o/oauth2/v2/auth"));
        assert!(url.contains("gmail.readonly"));
        assert!(url.contains("access_type"));
        assert!(url.contains("127.0.0.1"));
        assert_eq!(
            gmail_since_query(Some("22-Oct-2026|2026-10-22T00:00:00+00:00")),
            "after:2026/10/22"
        );
        assert_eq!(
            decode_b64url("SGVsbG8gV29ybGQ"),
            Some("Hello World".to_string())
        );
        std::env::remove_var("GOOGLE_OAUTH_CLIENT_ID");
        std::env::remove_var("GOOGLE_OAUTH_CLIENT_SECRET");
    }
}
