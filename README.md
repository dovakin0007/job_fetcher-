# Rust Job Agent — integrated local application

A local Axum dashboard for resume intelligence, job matching, scheduled discovery, and application-draft tracking.

## Current implementation

- PDF and DOCX resume upload and text extraction; LLM-assisted structured extraction with evidence excerpts and conservative fallback.
- Resume extraction supports Ollama and OpenAI-compatible APIs. `RESUME_LLM_PROVIDER=both` tries Ollama first and falls back to OpenAI.
- Original resume files are retained under `uploads/`; extracted candidate profile is saved to `candidate_profile.json`.
- Candidate matching preferences editable in the UI and persisted to `data/preferences.json`.
- Deterministic match score: `skills * 0.35 + experience * 0.20 + title * 0.25 + location * 0.20`, with excluded-keyword filtering and match explanations. The optional ONNX feature uses `ort = 2.0.0-rc.15`.
- Optional ONNX Runtime sentence embeddings (`--features onnx`) blended at 35% with deterministic scoring.
- Optional OpenAI-compatible LLM reranking above `LLM_RERANK_THRESHOLD`.
- SQLite persistence for jobs, applications, and discovery run history.
- Four-hour Tokio discovery loop plus a manual “Run discovery now” action.
- Provider trait and local JSON import provider; duplicate jobs upsert by `(source, external_id)` semantics currently keyed by `external_id`.
- Application draft queue, user approval, manual-applied status, reject/withdraw statuses, and notes.
- Local dashboard served at `127.0.0.1:8787`.

## Run (backend + UI)

```sh
# Any OS — backend first, UI opens in a new terminal once the API is healthy:
node dev.mjs
# Windows alternative: .\dev.cmd     macOS / Linux alternative: ./dev.sh
```

The launcher starts `cargo run --bin rust-job-agent` in the current terminal,
polls `/api/health`, then opens the Vite UI (`web/`, http://127.0.0.1:5173) in a
new terminal window. Ctrl+C stops the backend; close the UI window separately.
`web/node_modules` is installed automatically when missing.

Configure the resume LLM before the first upload:

## Run on Windows PowerShell (manual)

```powershell
Copy-Item data/jobs.example.json data/jobs.json
Copy-Item data/preferences.example.json data/preferences.json
# Option 1: Ollama only
$env:RESUME_LLM_PROVIDER = "ollama"
$env:OLLAMA_BASE_URL = "http://localhost:11434"
$env:OLLAMA_MODEL = "your-instruct-model"

# Option 2: both providers (Ollama first, OpenAI fallback)
# $env:RESUME_LLM_PROVIDER = "both"
# $env:OPENAI_API_KEY = "..."
# $env:OPENAI_MODEL = "gpt-4o-mini"

cargo run
```

Open http://127.0.0.1:8787. Without a configured LLM, uploads fall back to explicit skill detection. PDF and DOCX must contain selectable text; scanned PDFs need OCR, which is intentionally deferred. OpenAI and Ollama use an OpenAI-compatible `/v1/chat/completions` endpoint with JSON response mode.

## ONNX setup

The ONNX module is optional. Build with:

```powershell
$env:EMBEDDING_MODEL = "models/all-MiniLM-L6-v2.onnx"
$env:TOKENIZER_JSON = "models/tokenizer.json"
cargo run --features onnx
```

Supply a compatible ONNX graph and tokenizer yourself. The graph must accept `input_ids` and `attention_mask` and return token embeddings shaped `[1, sequence, hidden]`. The implementation performs attention-mask mean pooling and L2 normalization. Some exported models also require `token_type_ids`; those graphs need a small adapter change. Do not assume arbitrary ONNX exports are compatible.

## Job import format

Copy `data/jobs.example.json` to `data/jobs.json`. Each item must match `models::Job`: `external_id`, `source`, `title`, `company`, `description`, `url`, `discovered_at` (RFC3339), plus optional location, skills, experience bounds and work mode. Import only data obtained from permitted sources, official feeds, or authorized APIs.

## Application workflow and platform boundary

Shortlisted jobs become truthful drafts from resume-backed bullets. Auto-apply
submits where an application API exists — Greenhouse (official board API), Lever
(public postings apply endpoint), and Ashby (posting API) — whenever your email
and resume file are saved. Postings that demand answers the agent cannot know
(custom required questions, EEO) are rejected by those APIs and filed as
needs-manual instead of submitted blind. Everything else (including Workday,
which has no public apply API) stays a draft for you to send from the job page.
The user reviews and edits drafts, then can mark a draft approved or record that
they applied manually. The `ApplicationProvider` trait is the integration point
for further platform/employer submission APIs. LinkedIn/Naukri browser automation,
unauthorized scraping, CAPTCHA bypass, or anti-bot evasion are not implemented.

## LLM providers

Primary: OpenCode inference (`https://opencode.ai/inference`, Bearer key from the
Console — paid models or a Go subscription, since free-tier models return 403
from outside OpenCode). Set `OPENCODE_API_KEY` + `OPENCODE_MODEL`, and match
`OPENCODE_API_FAMILY` to the model: `openai` for kimi/glm/minimax …, `anthropic`
for claude/qwen …, `gemini` for Gemini … (each family is served at a different
path). The default order tries OpenCode first with local Ollama as the offline
fallback; override per flow with `TAILOR_LLM_PROVIDER` / `RESUME_LLM_PROVIDER`.

Model guidance: `kimi-k3` is the balanced default for strict JSON extraction and
grounded rewrites. If extraction drops evidence or rewrites feel flat, step up
to `claude-sonnet-5-5` with `OPENCODE_API_FAMILY=anthropic`. Keep small local
Qwen/Ollama models as fallback only — they are too limited for the 24k-char
evidence-anchored extraction this pipeline needs.

## Persistence

SQLite (`data/job_agent.db`) is the single source of truth. The resume profile
and matching preferences are stored as JSON documents in the `kv_store` table.
`candidate_profile.json` and `data/preferences.json` are only read once, on
first startup with an empty database, to import into the store — they are never
written by the app again.

## Community knowledge sources

Study guides pull real interview Q&A from public GitHub repos:

- **DevOps Q&A (quoted verbatim)**: [`bregman-arie/devops-exercises`](https://github.com/bregman-arie/devops-exercises)
  (CC BY-NC 3.0 — personal non-commercial use, attributed with a link on every
  quote). Only topics matching the company's demanded skills are fetched
  (Kubernetes, Docker, Python, SQL, Git, Linux, Kafka, Terraform, AWS/GCP/Azure,
  CI/CD, observability, SRE), cached 30 days, and ranked by skill overlap.
  When no repo questions match (or `KNOWLEDGE_OFFLINE=1`), Q&A is generated
  instead: LLM-written answers validated against your resume vocabulary first,
  otherwise deterministic questions answered with your own resume bullets —
  labeled "Generated for you" in the UI.
- **DSA**: curated lists only (Blind 75, Top 100 Liked, Top 150, AlgoMaster 300) —
  problem statements live on LeetCode, so nothing is copied, only linked.
- **LLD**: classic prompts in our own wording plus the verified
  [awesome-low-level-design](https://github.com/ashishps1/awesome-low-level-design)
  repo (GPL-3.0, linked only).
- **HLD**: primer solution links (unlicensed, linked only) plus the
  [awesome-system-design-resources](https://github.com/ashishps1/awesome-system-design-resources)
  list (GPL-3.0, linked only).
- **Linked reading**: [`donnemartin/system-design-primer`](https://github.com/donnemartin/system-design-primer)
  declares no license, so nothing is copied — the guide links to verified
  solution pages (Twitter, Pastebin, web crawler, caching, AWS scaling) plus the
  full guide.

`Many interview repos without a usable license (e.g. the archived
DopplerHQ/awesome-interview-questions, which declares none) are deliberately
not ingested.

## API keys at rest

- Search keys (Exa/Tavily/Brave) entered in the dashboard are sealed with
  AES-256-GCM into SQLite (`secrets` table: nonce + ciphertext + last-4 hint).
  Plaintext is never written to disk; legacy `data/search.json` keys are
  auto-migrated into the store and wiped from the file on the next discovery.
- Hashing alone cannot work here — the app must decrypt keys to call the APIs.
- The master key comes from `AGENT_MASTER_KEY` (64 hex chars) or an auto-created
  `data/.master_key`. Back that file up: losing it orphans the sealed keys.
- The UI only ever receives masked hints (`••••••••ab12`); env vars still
  override stored keys. Per-key Remove buttons forget dashboard-saved keys.

## API

- `GET /api/health`
- `GET /api/resume`; `POST /api/resume/upload` (multipart `resume`, PDF or DOCX, 10 MB max)
- `GET|PUT /api/preferences`
- `GET /api/jobs`
- `POST /api/discovery/run`; `GET /api/discovery/status`
- `PUT /api/resume` — resume builder save (full profile JSON, normalised server-side).
  Every save archives the previous version (newest 20 kept);
  `GET /api/resume/versions`, `GET /api/resume/versions/:v`, and
  `POST /api/resume/versions/:v/restore` browse and roll back history.
- `POST /api/resume/assist` (`{task: improve_bullet|generate_summary|suggest_bullets, ...}`)
  — AI writing help; rewrites are groundedness-checked, suggestions marked generated.
- `GET /api/resume/export?format=md|html` — Markdown download or print-ready HTML (print to PDF).
- The resume Source editor is a chunked rope (undo/redo as inverse ops, `web/src/rope.ts`
  with `npm run test:rope`); sessions auto-save to SQLite (`rope_docs`), so closing
  the app never loses in-progress edits. Offline kill-switch: `KNOWLEDGE_OFFLINE=1`
  also disables community fetches (local fallback Q&A still works).
- `POST /api/jobs/{external_id}/tailor` (JSON `{ "job": { ... } }`) — skill overlap, relevant bullets, integrity note.
- `POST /api/jobs/{external_id}/tailored-resume` (JSON `{ "job": { ... } }`) — per-job
  resume rewrite from the parsed resume. Uses `TAILOR_LLM_PROVIDER`
  (opencode / ollama / openai / anthropic / gemini) when configured and validates every
  line against the resume; otherwise returns the deterministic build.
- `POST /api/jobs/{external_id}/interview-prep` (JSON `{ "job": { ... }, "refresh": false }`)
  — per-job prep sheet (technical + behavioral questions with your resume bullets as
  evidence, questions to ask them), cached in SQLite. Same LLM providers as above.
- `GET /api/companies` — knowledge base: per-company posting/application counts,
  top role, statuses, guide + notes presence.
- `GET /api/companies/{name}/guide` and `POST` (JSON `{ "refresh": false }`) —
  per-company DSA topics, system-design brief, capacity notes, and resume tips
  inferred from that company's postings; LLM-sharpened when configured.
- `PUT /api/companies/{name}/notes` (JSON `{ "notes": "..." }`) — your own
  process intel per company.
- Set `KNOWLEDGE_OFFLINE=1` to skip fetching community sources.
- `GET|POST /api/applications`
- `POST /api/applications/{id}/status` (`pending_review`, `approved`, `applied_manually`, `rejected`, `withdrawn`)

## Important limitations

- This is a local-first implementation. It binds only to loopback; do not expose it publicly without authentication, CSRF protection, secure secrets handling, and a threat review.
- The included discovery provider is a JSON import adapter, not a live LinkedIn or Naukri connector. Add permitted API/feed providers behind `JobProvider`.
- Four-hour discovery runs while the process is alive. The scheduler's first automatic tick is four hours after startup; run manually for an immediate scan.
- Free default search: Remotive and Arbeitnow (official keyless JSON APIs) run on every
  discovery with no keys needed, filtered by your preferred titles/skills and the same
  country/age rules. Exa, Tavily, and Brave keys add deeper coverage on top —
  discovery no longer sits idle when all three are empty.
- Search scope is multi-city: a primary city plus up to 5 extra cities, then the
  nearest detected places (capped at 4 search places), with separate toggles for
  remote and hybrid postings — all editable from the dashboard and stored in SQLite.
- Search depth: Exa requests up to 8,000 characters per result with highlights; Tavily
  uses advanced depth with raw-content fallback. Postings whose snippet is still thin
  are enriched to full page text (Tavily `/extract`, else Exa `/contents`, capped by
  `SEARCH_ENRICH_MAX`, cached in SQLite for 30 days), and skills are extracted from
  the full text — descriptions are stored up to 8,000 characters.
- Scanned-image PDFs need OCR; no OCR engine is bundled.
- The ONNX feature is opt-in and needs compatible model/tokenizer artifacts; default build uses deterministic matching and optional LLM reranking.
- LLM extraction and reranking are best-effort; manually verify every extracted claim and application draft.
- No blind/automatic application submission is enabled.

## Tests

```powershell
cargo test
cargo test --features onnx
```

The ONNX test/build requires downloading/building the optional runtime dependencies and compatible model artifacts are still needed at runtime.

## Browser-assisted application handoff (ChromeDriver)

This version includes an explicit user-driven browser handoff. It opens a supplied job URL in a local Chrome session so you can review the listing and complete the application yourself. It does not inspect page DOM, scrape LinkedIn, click Apply, fill/submit forms, bypass CAPTCHAs, or evade platform controls.

1. Download ChromeDriver matching your installed Chrome version from https://googlechromelabs.github.io/chrome-for-testing/.
2. Start it in a separate PowerShell window: `chromedriver.exe --port=9515`.
3. Start the Rust app and open the **Browser assistant** tab.
4. Check the connection, paste a job URL, and select **Open job in Chrome**. You can also open a job directly from the Discover Jobs card.

Optional local endpoint override: `WEBDRIVER_URL=http://127.0.0.1:9515`. Only loopback HTTP endpoints are accepted. Do not expose ChromeDriver's port to your LAN or the public internet. Browser handoff requires the user to review and submit manually. For sites that explicitly authorize automation, implement a separate, site-specific provider after verifying its permitted scope and terms.

New API routes: `GET /api/browser/status`, `POST /api/browser/open` with `{ "url": "https://..." }`, and `POST /api/browser/close`.


## Public job-page discovery

The app includes a conservative `PublicJobPageProvider` for explicitly configured public job-posting URLs. It extracts Schema.org `JobPosting` JSON-LD (title, employer, description, location, skills, URL, and basic experience range). It checks the page origin's `robots.txt` before fetching and skips pages disallowed by the site's rules. It does not log in, crawl LinkedIn/Naukri, bypass access controls, or scrape pages that prohibit automated access.

You can add sources in **Discover jobs → Add a permitted public job page**. The same configuration can be edited in `data/job_sources.json`:

```json
{
  "sources": [
    { "name": "Example Careers", "url": "https://careers.example.com/jobs/backend-engineer" }
  ]
}
```

Only add individual public job posting URLs (or pages that expose a `JobPosting` JSON-LD record) from sources that permit automated fetching. General career landing pages without structured job data will not yield a job record. Discovery runs immediately when you press **Run discovery now**, and the scheduler checks configured sources every four hours. The local JSON fixture importer remains available as well.

The crawler uses a descriptive user agent, a 20-second request timeout, limited redirects, a 1.2-second pause between configured pages, and a robots.txt check. If robots.txt cannot be fetched, the page is skipped with an error rather than fetched blindly.


## Multi-job careers listing discovery

In `data/job_sources.json`, set `kind` to `listing` for a permitted public company careers index and `max_pages` (1–50, default 15). The provider fetches the listing, parses all Schema.org `JobPosting` entries, and follows candidate job-detail links **on the same origin only**. It checks robots.txt for every page, waits 1.2 seconds between requests, skips failures per source, and deduplicates URLs. `kind: posting` preserves single-page behavior.

Copy `data/job_sources.example.json` to `data/job_sources.json` and replace the example URLs with real permitted sources. This is an HTML/JSON-LD crawler, not a JavaScript-rendered browser crawler. Company sites with unusual URL structures need custom adapters. It must not be used on sites that prohibit automated access.


## Resume profile UI fix

The `/api/resume` endpoint now includes `uploaded: true` when a profile exists.
The dashboard also accepts a structurally valid profile response when that flag is
absent, fixing the previous empty-state check that hid successfully extracted profiles.

The dashboard is a React TypeScript app in `web/`, styled with Tailwind CSS v4
(theme tokens in `web/src/styles.css`, utilities in the components). After changing it, run `npm run build` in `web/`, restart the Rust app, and hard-refresh the browser (Ctrl+F5). The server serves `web/dist`.
