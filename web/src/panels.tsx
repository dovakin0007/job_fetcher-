import { FormEvent, useEffect, useRef, useState, type CSSProperties, type MouseEvent } from "react";
import { ThemeButton } from "./theme";
import { api } from "./api";
import { Rope, UndoStack, applyEdit } from "./rope";
import type {
  Application,
  CompanyEntry,
  CompanyGuideResult,
  DiscoveryStatus,
  Experience,
  JobResult,
  SearchConfig,
  Preferences,
  ResumeProfile,
  SkillWeight,
  Source,
  Tab,
  TailorResult,
  TailoredResumeResult,
  PrepSheetResult,
  UploadResult,
} from "./types";

function list(value: string): string[] {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

const RESUME_STYLES = [
  { id: "classic", label: "Classic", blurb: "Serif, centered, ATS-safe" },
  { id: "modern", label: "Modern", blurb: "Navy banner, skill chips" },
  { id: "compact", label: "Compact", blurb: "Two columns, dense page" },
  { id: "timeline", label: "Timeline", blurb: "Dates in a left rail" },
] as const;

const GOOGLE_FONTS = [
  "Inter",
  "Roboto",
  "Source Sans 3",
  "IBM Plex Sans",
  "Merriweather",
  "Libre Baskerville",
  "Lora",
  "Playfair Display",
] as const;

function StylePicker({ value, onChange }: { value: string; onChange: (id: string) => void }) {
  return (
    <div className="style-picker">
      {RESUME_STYLES.map((style) => (
        <button
          type="button"
          key={style.id}
          className={`style-chip ${value === style.id ? "on" : ""}`}
          onClick={() => onChange(style.id)}
        >
          <b>{style.label}</b>
          <span>{style.blurb}</span>
        </button>
      ))}
    </div>
  );
}

function ResumePreview({ html }: { html: string }) {
  if (!html) return <p className="text-muted">Preview will appear here.</p>;
  return <iframe className="resume-preview" title="Resume preview" srcDoc={html} />;
}

type AiBullet = { role: number; index: number; text: string };
type EditorSection = "design" | "layout" | "profile" | "summary" | "experience" | "education" | "skills" | "projects" | "ai";

const BASE_LAYOUT = ["summary", "skills", "experience", "projects", "education", "open_source"];

function layoutOf(profile: ResumeProfile): string[] {
  const custom = (profile.custom_fields ?? []).map((field) => field.id).filter(Boolean);
  const allowed = new Set([...BASE_LAYOUT, ...custom]);
  const seen = new Set<string>();
  const out: string[] = [];
  for (const id of profile.layout ?? []) {
    if (allowed.has(id) && !seen.has(id)) {
      seen.add(id);
      out.push(id);
    }
  }
  for (const id of [...BASE_LAYOUT, ...custom]) {
    if (!seen.has(id)) out.push(id);
  }
  return out;
}

const EDITOR_SECTIONS: { id: EditorSection; label: string }[] = [
  { id: "design", label: "Design" },
  { id: "layout", label: "Layout" },
  { id: "profile", label: "Profile" },
  { id: "summary", label: "Summary" },
  { id: "experience", label: "Work" },
  { id: "education", label: "Education" },
  { id: "skills", label: "Skills" },
  { id: "projects", label: "Projects" },
  { id: "ai", label: "AI" },
];

function wordAtCaret(value: string, caret: number): { word: string; start: number } | null {
  const left = value.slice(0, caret);
  const match = /[A-Za-z][A-Za-z'-]*$/.exec(left);
  if (!match || match[0].length < 2) return null;
  return { word: match[0], start: caret - match[0].length };
}

type DictMeaning = { partOfSpeech?: string; definitions?: { definition?: string; example?: string; synonyms?: string[] }[] };

function DictionaryCard({
  word,
  onUse,
}: {
  word: string;
  onUse: (next: string) => void;
}) {
  const [phonetic, setPhonetic] = useState("");
  const [meanings, setMeanings] = useState<DictMeaning[]>([]);
  const [note, setNote] = useState("");

  useEffect(() => {
    const clean = word.toLowerCase().replace(/[^a-z'-]/g, "");
    if (clean.length < 2) {
      setMeanings([]);
      setNote("");
      return;
    }
    let cancelled = false;
    setNote("Looking up…");
    const timer = window.setTimeout(() => {
      void fetch(`https://api.dictionaryapi.dev/api/v2/entries/en/${encodeURIComponent(clean)}`)
        .then(async (response) => {
          if (!response.ok) throw new Error("No entry");
          return response.json() as Promise<{ phonetic?: string; meanings?: DictMeaning[] }[]>;
        })
        .then((entries) => {
          if (cancelled) return;
          const entry = entries[0];
          setPhonetic(entry?.phonetic || "");
          setMeanings(entry?.meanings ?? []);
          setNote(entry ? "" : "No definition for that word.");
        })
        .catch(() => {
          if (!cancelled) {
            setMeanings([]);
            setNote("No definition for that word.");
          }
        });
    }, 400);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [word]);

  const synonyms = meanings
    .flatMap((meaning) => meaning.definitions ?? [])
    .flatMap((definition) => definition.synonyms ?? [])
    .filter((item, index, all) => item && all.indexOf(item) === index)
    .slice(0, 8);

  return (
    <div className="dict-card">
      <b>{word}</b>
      {phonetic ? <span className="dict-phonetic"> {phonetic}</span> : null}
      {note ? <p className="text-xs text-muted">{note}</p> : null}
      {meanings.slice(0, 2).map((meaning) => (
        <p key={meaning.partOfSpeech} className="text-xs">
          <em>{meaning.partOfSpeech}</em> — {meaning.definitions?.[0]?.definition}
          {meaning.definitions?.[0]?.example ? ` “${meaning.definitions[0].example}”` : ""}
        </p>
      ))}
      {synonyms.length > 0 ? (
        <div className="flex flex-wrap gap-1">
          {synonyms.map((item) => (
            <button key={item} type="button" className="btn secondary" onClick={() => onUse(item)}>{item}</button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

const AI_PROMPTS = [
  "Tighten every bullet to one line. Keep the same facts.",
  "Use stronger action verbs. Do not add metrics or tools.",
  "Rewrite the summary in two sentences for a senior backend role.",
  "Shorten the headline so it leads with my strongest skills.",
];

function AiEditor({
  form,
  onApply,
}: {
  form: ResumeProfile;
  onApply: (patch: { headline?: string; summary?: string; bullets: AiBullet[] }) => void;
}) {
  const [instruction, setInstruction] = useState(AI_PROMPTS[0]);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("Ask for a rewrite. Changes stay as suggestions until you apply them.");
  const [headline, setHeadline] = useState<string | null>(null);
  const [summary, setSummary] = useState<string | null>(null);
  const [bullets, setBullets] = useState<AiBullet[]>([]);

  async function run(text: string) {
    const instruction = text.trim();
    if (!instruction) return;
    setBusy(true);
    setNote("Rewriting from your resume…");
    try {
      const result = await api<{
        headline?: string | null;
        summary?: string | null;
        bullets?: AiBullet[];
        note?: string;
        llm_used?: boolean;
        provider?: string;
      }>("/api/resume/assist", {
        method: "POST",
        body: JSON.stringify({ task: "edit", instruction, profile: form }),
      });
      setHeadline(result.headline || null);
      setSummary(result.summary || null);
      setBullets(result.bullets ?? []);
      const who = result.llm_used ? result.provider || "the model" : "no model";
      setNote(`${result.note || "Done."} (${who})`);
    } catch (error) {
      setNote(error instanceof Error ? error.message : "Editor failed");
    } finally {
      setBusy(false);
    }
  }

  const pending = Boolean(headline || summary || bullets.length);

  return (
    <div className="ai-editor">
      <div className="flex items-center justify-between gap-2">
        <h3>AI editor</h3>
        <span className="status">{busy ? "Writing" : pending ? "Review" : "Ready"}</span>
      </div>
      <p className="text-xs text-muted">It rewrites your headline, summary, and bullets. Employers, dates, and new claims are rejected.</p>
      <div className="flex flex-wrap gap-1.5">
        {AI_PROMPTS.map((prompt) => (
          <button key={prompt} type="button" className="style-chip" onClick={() => { setInstruction(prompt); void run(prompt); }} disabled={busy}>
            <b>{prompt.split(".")[0]}</b>
          </button>
        ))}
      </div>
      <textarea rows={3} value={instruction} onChange={(event) => setInstruction(event.target.value)} placeholder="e.g. Make the ClickHouse bullets more specific without adding numbers." />
      <button className="btn" disabled={busy} onClick={() => void run(instruction)}>{busy ? "Rewriting…" : "Rewrite"}</button>
      <p className="text-xs text-muted">{note}</p>
      {headline ? (
        <div className="ai-change">
          <b>Headline</b>
          <p>{headline}</p>
          <button className="btn secondary" onClick={() => { onApply({ headline, bullets: [] }); setHeadline(null); }}>Apply headline</button>
        </div>
      ) : null}
      {summary ? (
        <div className="ai-change">
          <b>Summary</b>
          <p>{summary}</p>
          <button className="btn secondary" onClick={() => { onApply({ summary, bullets: [] }); setSummary(null); }}>Apply summary</button>
        </div>
      ) : null}
      {bullets.map((bullet) => (
        <div className="ai-change" key={`${bullet.role}-${bullet.index}`}>
          <b>Bullet · role {bullet.role + 1}</b>
          <p>{bullet.text}</p>
          <button className="btn secondary" onClick={() => { onApply({ bullets: [bullet] }); setBullets((current) => current.filter((item) => item !== bullet)); }}>Apply bullet</button>
        </div>
      ))}
      {pending ? (
        <button
          className="btn"
          onClick={() => {
            onApply({ headline: headline ?? undefined, summary: summary ?? undefined, bullets });
            setHeadline(null);
            setSummary(null);
            setBullets([]);
          }}
        >Apply all</button>
      ) : null}
    </div>
  );
}

async function downloadResumePdf(body: Record<string, unknown>, filename: string) {
  const response = await fetch("/api/resume/render", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...body, format: "pdf" }),
  });
  if (!response.ok) {
    throw new Error((await response.text()) || response.statusText);
  }
  const blob = await response.blob();
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  anchor.click();
  URL.revokeObjectURL(url);
}

export function Overview({
  jobs,
  shortlisted,
  pendingApps,
  status,
}: {
  jobs: number;
  shortlisted: number;
  pendingApps: number;
  status: DiscoveryStatus | null;
}) {
  const run = status?.last_run;
  return (
    <section>
      <h2>Home</h2>
      <div className="grid grid-cols-1 gap-3.5 md:grid-cols-2 xl:grid-cols-3">
        <div className="card"><div className="text-muted">Discovered jobs</div><div className="text-[30px] font-bold tracking-tight">{jobs}</div></div>
        <div className="card"><div className="text-muted">Shortlisted</div><div className="text-[30px] font-bold tracking-tight">{shortlisted}</div></div>
        <div className="card"><div className="text-muted">Applications in review</div><div className="text-[30px] font-bold tracking-tight">{pendingApps}</div></div>
      </div>
      <div className="card">
        <h3>Discovery status</h3>
        <div className="text-muted">
          {run
            ? `Last run: ${run.finished_at || "running"} · ${run.discovered ?? 0} jobs · ${run.error || "No error"}`
            : "No discovery run yet"}
        </div>
        <p className="text-xs text-muted">Automatic discovery runs every four hours while this local application is running.</p>
      </div>
      <div className="card">
        <h3>Getting started</h3>
        <ol>
          <li>Upload your PDF resume.</li>
          <li>Review matching preferences and set realistic experience and location requirements.</li>
          <li>Add Exa, Tavily, or Brave keys, a Greenhouse board, or a permitted careers page, then run discovery.</li>
          <li>Review matches and create application drafts. Apply on the employer site yourself.</li>
        </ol>
      </div>
    </section>
  );
}

export function ResumePanel({
  profile,
  onUploaded,
  onEditing,
}: {
  profile: ResumeProfile | null;
  onUploaded: () => Promise<void>;
  onEditing?: (editing: boolean) => void;
}) {
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [mode, setMode] = useState<"view" | "edit" | "source" | "history">("view");
  const [pendingImport, setPendingImport] = useState<ResumeProfile | null>(null);
  const [style, setStyle] = useState("classic");
  const [previewHtml, setPreviewHtml] = useState("");

  useEffect(() => {
    onEditing?.(mode === "edit");
    return () => onEditing?.(false);
  }, [mode, onEditing]);

  const afterSaved = async () => {
    setMode("view");
    setPendingImport(null);
    await api("/api/rope/resume-source", { method: "DELETE" }).catch(() => undefined);
    await onUploaded();
  };
  const hasProfile = Boolean(
    profile &&
      (profile.uploaded === true ||
        typeof profile.name === "string" ||
        Array.isArray(profile.skills) ||
        Array.isArray(profile.experience)),
  ) && profile?.uploaded !== false;

  useEffect(() => {
    if (mode !== "view" || !hasProfile) return;
    let cancelled = false;
    (async () => {
      try {
        const response = await fetch(`/api/resume/export?format=html&style=${encodeURIComponent(style)}`);
        if (!response.ok || cancelled) return;
        setPreviewHtml(await response.text());
      } catch {
        if (!cancelled) setPreviewHtml("");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [mode, style, profile, hasProfile]);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setBusy(true);
    setMessage("Parsing…");
    try {
      const data = await api<UploadResult>("/api/resume/upload", {
        method: "POST",
        body: new FormData(event.currentTarget),
      });
      setMessage(
        `Parsed ${data.skills} skills, ${data.experience} experience entries, ${data.projects} projects and ${data.education} education entries (${data.format.toUpperCase()}). ${data.llm_configured ? "LLM extraction used." : "Fallback extraction used; configure an LLM for full profile extraction."}`,
      );
      await onUploaded();
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Upload failed");
    } finally {
      setBusy(false);
    }
  }

  if (mode === "edit") {
    return (
      <ResumeBuilder
        key={pendingImport ? "imported" : hasProfile ? "saved" : "empty"}
        initial={pendingImport ?? profile ?? { skills: [], experience: [], projects: [], education: [], open_source: [] }}
        style={style}
        onStyle={setStyle}
        onSaved={afterSaved}
        onCancel={() => { setMode("view"); setPendingImport(null); }}
      />
    );
  }

  return (
    <section className="p-7 max-md:p-4">
      <h2>Resume</h2>
      <div className="card max-w-[720px]">
        <form onSubmit={onSubmit}>
          <label>Upload resume (PDF or DOCX, max 10 MB)</label>
          <input type="file" name="resume" accept=".pdf,.docx,application/pdf" required />
          <button className="btn" disabled={busy}>Upload and parse</button>
        </form>
        <div className="text-[13px] text-muted">{message}</div>
      </div>
      <div className="card">
        {mode === "source" ? (
          <SourceEditor
            onImported={(profile) => {
              setPendingImport(profile);
              setMode("edit");
            }}
            onExit={() => setMode("view")}
          />
        ) : null}
        {mode === "history" ? <VersionsPanel onRestored={async () => { await onUploaded(); }} /> : null}
        {mode === "view" ? (
          !hasProfile || !profile ? (
            <span>No resume uploaded yet. Upload one above, or <button className="btn secondary" onClick={() => setMode("edit")}>start an empty draft</button>.</span>
          ) : (
            <>
              <div className="flex flex-wrap items-center gap-2.5" style={{ marginBottom: 12 }}>
                <button className="btn" onClick={() => { setPendingImport(null); setMode("edit"); }}>Edit resume</button>
                <button className="btn secondary" onClick={() => setMode("source")}>Source (rope)</button>
                <button className="btn secondary" onClick={() => setMode("history")}>History</button>
                <button
                  className="btn"
                  onClick={() => {
                    const anchor = document.createElement("a");
                    anchor.href = `/api/resume/export?format=pdf&style=${encodeURIComponent(style)}`;
                    anchor.download = "resume.pdf";
                    anchor.click();
                  }}
                >Download PDF</button>
              </div>
              <p className="text-xs text-muted">Layouts pull only from your uploaded resume. Pick a style, then download a PDF — nothing is invented.</p>
              <StylePicker value={style} onChange={setStyle} />
              <ResumePreview html={previewHtml} />
              <ResumeView profile={profile} />
            </>
          )
        ) : null}
      </div>
    </section>
  );
}

function ResumeView({ profile }: { profile: ResumeProfile }) {
  const skills = profile.skills ?? [];
  const experience = profile.experience ?? [];
  const projects = profile.projects ?? [];
  const education = profile.education ?? [];
  const openSource = profile.open_source ?? [];
  const warnings = profile.extraction_warnings ?? [];
  return (
    <>
      <div className="flex flex-wrap items-center justify-between gap-2.5">
        <div>
          <h3>{profile.name || "Candidate"}</h3>
          <div className="text-muted">{profile.headline || "Resume profile"}</div>
        </div>
        <span className="status">{profile.source_file || "Resume loaded"}</span>
      </div>
      {profile.summary ? <p>{profile.summary}</p> : null}
      <div className="grid grid-cols-1 gap-3.5 md:grid-cols-2 xl:grid-cols-3">
        <div className="card"><div className="text-xs text-muted">Skills</div><div className="text-[30px] font-bold tracking-tight">{skills.length}</div></div>
        <div className="card"><div className="text-xs text-muted">Experience entries</div><div className="text-[30px] font-bold tracking-tight">{experience.length}</div></div>
        <div className="card"><div className="text-xs text-muted">Projects</div><div className="text-[30px] font-bold tracking-tight">{projects.length}</div></div>
      </div>
      <h3>Skills and evidence</h3>
      {skills.length === 0 ? <p className="text-muted">No skills extracted.</p> : skills.map((skill) => (
        <div key={skill.name} style={{ margin: "8px 0" }}>
          <span className="tag">{skill.name || "Unnamed skill"} · {Math.round((skill.confidence || 0) * 100)}%</span>
          <div className="text-xs text-muted">{(skill.evidence ?? []).join(" · ") || "No supporting excerpt retained"}</div>
        </div>
      ))}
      <h3>Experience</h3>
      {experience.length === 0 ? <p className="text-muted">No experience extracted.</p> : experience.map((item) => (
        <article className="card job-hover" key={`${item.company}-${item.title}`}>
          <div className="flex flex-wrap items-center justify-between gap-2.5">
            <b>{item.title || "Role"} · {item.company || "Company"}</b>
            <span className="text-xs text-muted">{item.dates || ""}</span>
          </div>
          {item.location ? <div className="text-xs text-muted">{item.location}</div> : null}
          {(item.bullets ?? []).length === 0 ? <p className="text-xs text-muted">No validated experience bullets.</p> : (
            <ul>
              {(item.bullets ?? []).map((bullet) => (
                <li key={bullet.text}>
                  {bullet.text || ""}
                  <div className="text-xs text-muted">Evidence: {bullet.source_excerpt || ""}</div>
                </li>
              ))}
            </ul>
          )}
        </article>
      ))}
      <h3>Projects</h3>
      {projects.length === 0 ? <p className="text-muted">No projects extracted.</p> : projects.map((project) => (
        <article className="card job-hover" key={project.name}>
          <b>{project.name || "Project"}</b>
          <p>{project.description || ""}</p>
          <div className="text-xs text-muted">{(project.technologies ?? []).join(", ")}</div>
        </article>
      ))}
      <h3>Education</h3>
      {education.length === 0 ? <p className="text-muted">No education extracted.</p> : education.map((item) => (
        <p key={`${item.institution}-${item.qualification}`}>{item.qualification || ""} · {item.institution || ""} {item.dates || ""}</p>
      ))}
      <h3>Open source</h3>
      {openSource.length === 0 ? <p className="text-muted">No open-source contributions extracted.</p> : openSource.map((item) => (
        <p key={item.project}><b>{item.project || ""}</b> — {item.contribution || ""}</p>
      ))}
      {warnings.length > 0 ? (
        <>
          <h3>Extraction notes</h3>
          {warnings.map((warning) => <p className="text-xs text-muted" key={warning}>{warning}</p>)}
        </>
      ) : null}
    </>
  );
}

function ResumeBuilder({ initial, style, onStyle, onSaved, onCancel }: {
  initial: ResumeProfile;
  style: string;
  onStyle: (id: string) => void;
  onSaved: () => Promise<void> | void;
  onCancel: () => void;
}) {
  const normalized = (profile: ResumeProfile): ResumeProfile => ({
    ...profile,
    skills: (profile.skills ?? []).map((s) => ({ name: s.name ?? "", evidence: s.evidence ?? [], confidence: s.confidence ?? 0.7 })),
    experience: (profile.experience ?? []).map((e) => ({
      company: e.company ?? "",
      title: e.title ?? "",
      dates: e.dates ?? "",
      location: e.location ?? "",
      bullets: (e.bullets ?? []).map((b) => ({ text: b.text ?? "", source_excerpt: b.source_excerpt ?? "" })),
    })),
    projects: (profile.projects ?? []).map((p) => ({ name: p.name ?? "", description: p.description ?? "", technologies: p.technologies ?? [] })),
    education: (profile.education ?? []).map((e) => ({ qualification: e.qualification ?? "", institution: e.institution ?? "", dates: e.dates ?? "" })),
    open_source: profile.open_source ?? [],
    extraction_warnings: profile.extraction_warnings ?? [],
    layout: profile.layout ?? [],
    custom_fields: (profile.custom_fields ?? []).map((field) => ({
      id: field.id || `c${Math.random().toString(36).slice(2, 8)}`,
      label: field.label ?? "",
      body: field.body ?? "",
    })),
  });
  const [form, setForm] = useState<ResumeProfile>(() => normalized(initial));
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState("");
  const [suggest, setSuggest] = useState<{ role: number; bullet: number; text: string; note?: string } | null>(null);
  const [drafts, setDrafts] = useState<string[]>([]);
  const [draftNote, setDraftNote] = useState("");
  const [draftSkill, setDraftSkill] = useState("");
  const [draftRole, setDraftRole] = useState(0);
  const [previewHtml, setPreviewHtml] = useState("");
  const [pdfBusy, setPdfBusy] = useState(false);
  const [section, setSection] = useState<EditorSection>("design");
  const [font, setFont] = useState("Inter");
  const [bodySize, setBodySize] = useState(10.5);
  const [headingSize, setHeadingSize] = useState(26);
  const [fontWeight, setFontWeight] = useState(400);
  const [subtitleSize, setSubtitleSize] = useState(13);
  const [sectionSize, setSectionSize] = useState(11);
  const [sectionTitles, setSectionTitles] = useState({
    summary_title: "Summary",
    skills_title: "Skills",
    experience_title: "Experience",
    projects_title: "Projects",
    education_title: "Education",
  });
  const [ideas, setIdeas] = useState<string[]>([]);
  const [ideaNote, setIdeaNote] = useState("");
  const type = {
    font,
    body_size: bodySize,
    heading_size: headingSize,
    font_weight: fontWeight,
    subtitle_size: subtitleSize,
    section_size: sectionSize,
    ...sectionTitles,
  };
  const [designPreviews, setDesignPreviews] = useState<Record<string, string>>({});
  const [lookup, setLookup] = useState<{ word: string; replace: (next: string) => void } | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);

  const watchWord = (value: string, caret: number, write: (next: string) => void) => {
    const found = wordAtCaret(value, caret);
    if (!found) {
      setLookup(null);
      return;
    }
    setLookup({
      word: found.word,
      replace: (next) => write(value.slice(0, found.start) + next + value.slice(caret)),
    });
  };

  useEffect(() => {
    const timer = window.setTimeout(() => {
      void api<{ html: string }>("/api/resume/render", {
        method: "POST",
        body: JSON.stringify({ style, profile: form, ...type }),
      }).then((result) => setPreviewHtml(result.html ?? "")).catch(() => undefined);
    }, 400);
    return () => window.clearTimeout(timer);
  }, [form, style, font, bodySize, headingSize, fontWeight, subtitleSize, sectionSize, sectionTitles]);

  useEffect(() => {
    if (section !== "design") return;
    let cancelled = false;
    for (const item of RESUME_STYLES) {
      void api<{ html: string }>("/api/resume/render", {
        method: "POST",
        body: JSON.stringify({ style: item.id, profile: form, ...type }),
      }).then((result) => {
        if (!cancelled) setDesignPreviews((current) => ({ ...current, [item.id]: result.html ?? "" }));
      }).catch(() => undefined);
    }
    return () => {
      cancelled = true;
    };
  }, [section, form, font, bodySize, headingSize, fontWeight, subtitleSize, sectionSize, sectionTitles]);

  useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      const data = event.data as { type?: string; order?: string[] } | null;
      const order = data?.order;
      if (!data || data.type !== "resume-layout" || !Array.isArray(order)) return;
      setForm((current) => {
        const currentOrder = layoutOf(current);
        const visible = new Set(order);
        const queue = order.filter((id) => currentOrder.includes(id));
        const next = currentOrder.map((id) => (visible.has(id) ? queue.shift() ?? id : id));
        return { ...current, layout: next };
      });
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  const set = (patch: Partial<ResumeProfile>) => setForm((f) => ({ ...f, ...patch }));
  const setRole = (index: number, patch: Partial<Experience>) =>
    setForm((f) => ({ ...f, experience: (f.experience ?? []).map((r, i) => (i === index ? { ...r, ...patch } : r)) }));
  const setBullet = (role: number, bullet: number, text: string) =>
    setForm((f) => ({
      ...f,
      experience: (f.experience ?? []).map((r, i) =>
        i === role
          ? { ...r, bullets: (r.bullets ?? []).map((b, j) => (j === bullet ? { ...b, text } : b)) }
          : r,
      ),
    }));

  async function save() {
    setMessage("");
    setBusy("save");
    try {
      await api("/api/resume", { method: "PUT", body: JSON.stringify(form) });
      setMessage("Resume saved. Matching now uses this version.");
      await onSaved();
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Could not save resume");
    } finally {
      setBusy("");
    }
  }

  async function improveBullet(role: number, bullet: number, text: string) {
    if (!text.trim()) return;
    setBusy(`improve-${role}-${bullet}`);
    try {
      const result = await api<{ bullet: string; llm_used?: boolean; provider?: string; note?: string }>(
        "/api/resume/assist",
        { method: "POST", body: JSON.stringify({ task: "improve_bullet", bullet: text }) },
      );
      setSuggest({ role, bullet, text: result.bullet, note: result.llm_used ? `Rewritten${result.provider ? ` by ${result.provider}` : ""}.` : (result.note || "Original kept.") });
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Assist failed");
    } finally {
      setBusy("");
    }
  }

  async function generateSummary() {
    setBusy("summary");
    try {
      const result = await api<{ summary: string; llm_used?: boolean; provider?: string; note?: string }>(
        "/api/resume/assist",
        { method: "POST", body: JSON.stringify({ task: "generate_summary" }) },
      );
      set({ summary: result.summary });
      setMessage(result.llm_used ? `Summary drafted${result.provider ? ` by ${result.provider}` : ""}. Review before saving.` : (result.note || "Summary drafted."));
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Assist failed");
    } finally {
      setBusy("");
    }
  }

  async function suggestBullets() {
    if (!draftSkill.trim()) {
      setDraftNote("Enter a skill first.");
      return;
    }
    setBusy("drafts");
    try {
      const result = await api<{ bullets: string[]; note?: string }>(
        "/api/resume/assist",
        { method: "POST", body: JSON.stringify({ task: "suggest_bullets", skill: draftSkill, role: (form.experience ?? [])[draftRole]?.title || "", count: 3 }) },
      );
      setDrafts(result.bullets ?? []);
      setDraftNote(result.note || "Review each draft before adding it.");
    } catch (error) {
      setDraftNote(error instanceof Error ? error.message : "Assist failed");
    } finally {
      setBusy("");
    }
  }

  async function loadSuggestions() {
    setBusy("ideas");
    setIdeaNote("");
    try {
      const result = await api<{ suggestions?: string[]; note?: string; provider?: string; llm_used?: boolean }>(
        "/api/resume/suggestions",
        { method: "POST", body: JSON.stringify({ section, profile: form }) },
      );
      setIdeas(result.suggestions ?? []);
      setIdeaNote(result.note || (result.llm_used ? `From ${result.provider || "the model"}.` : ""));
    } catch (error) {
      setIdeaNote(error instanceof Error ? error.message : "Suggestions failed");
    } finally {
      setBusy("");
    }
  }

  function applyIdea(idea: string) {
    if (section === "summary") {
      set({ summary: idea });
    } else if (section === "profile") {
      set({ headline: idea });
    } else if (section === "skills") {
      setForm((current) => ({
        ...current,
        skills: [...(current.skills ?? []), { name: idea, evidence: [], confidence: 0.7 }],
      }));
    } else if (section === "experience") {
      setForm((current) => {
        const roles = [...(current.experience ?? [])];
        if (roles.length === 0) {
          roles.push({ company: "", title: "", dates: "", location: "", bullets: [{ text: idea, source_excerpt: idea }] });
        } else {
          const last = roles.length - 1;
          roles[last] = { ...roles[last], bullets: [...(roles[last].bullets ?? []), { text: idea, source_excerpt: idea }] };
        }
        return { ...current, experience: roles };
      });
    } else if (section === "projects") {
      setForm((current) => {
        const projects = [...(current.projects ?? [])];
        if (projects.length === 0) projects.push({ name: "", description: idea, technologies: [] });
        else projects[0] = { ...projects[0], description: idea };
        return { ...current, projects };
      });
    }
    setMessage("Suggestion applied. Check the page, then save.");
  }

  const roles = form.experience ?? [];
  const moveRole = (index: number, direction: -1 | 1) => {
    setForm((current) => {
      const list = [...(current.experience ?? [])];
      const next = index + direction;
      if (next < 0 || next >= list.length) return current;
      const [item] = list.splice(index, 1);
      list.splice(next, 0, item);
      return { ...current, experience: list };
    });
  };

  return (
    <div className="flow-shell">
      <header className="flow-top">
        <button className="btn secondary" onClick={onCancel}>Back</button>
        <ThemeButton />
        <strong>Resume editor</strong>
        <span className="flow-note">{message || `Your parsed resume · ${(form.skills ?? []).length} skills · ${(form.experience ?? []).length} jobs · ${(form.projects ?? []).length} projects · ${(form.education ?? []).length} education`}</span>
        <button className="btn secondary" disabled={pdfBusy} onClick={() => {
          setPdfBusy(true);
          void downloadResumePdf({ style, profile: form, ...type }, "resume.pdf")
            .catch((error: unknown) => setMessage(error instanceof Error ? error.message : "PDF failed"))
            .finally(() => setPdfBusy(false));
        }}>{pdfBusy ? "Building PDF…" : "Download"}</button>
        <button className="btn" disabled={busy === "save"} onClick={() => void save()}>{busy === "save" ? "Saving…" : "Save"}</button>
      </header>
      <div className="flow-body">
        <nav className="flow-rail" aria-label="Resume sections">
          {EDITOR_SECTIONS.map((item) => (
            <button key={item.id} type="button" className={section === item.id ? "on" : ""} onClick={() => setSection(item.id)}>
              {item.label}
            </button>
          ))}
        </nav>
        <div className="flow-panel">
          {section === "profile" ? (
            <>
              <h3>Profile</h3>
              <label>Full name</label>
              <input value={form.name || ""} onChange={(e) => set({ name: e.target.value })} placeholder="Your name" />
              <label>Headline</label>
              <input value={form.headline || ""} onChange={(e) => set({ headline: e.target.value })} placeholder="Senior Software Engineer" />
            </>
          ) : null}
          {section === "summary" ? (
            <>
              <div className="flex items-center justify-between gap-2">
                <h3>Summary</h3>
                <button className="btn secondary" disabled={busy === "summary"} onClick={() => void generateSummary()}>{busy === "summary" ? "Drafting…" : "Draft with AI"}</button>
              </div>
              <textarea
                rows={8}
                value={form.summary || ""}
                placeholder="Two or three lines on what you do."
                onChange={(e) => set({ summary: e.target.value })}
                onKeyUp={(e) => watchWord(e.currentTarget.value, e.currentTarget.selectionStart ?? 0, (summary) => set({ summary }))}
                onClick={(e) => watchWord(e.currentTarget.value, e.currentTarget.selectionStart ?? 0, (summary) => set({ summary }))}
              />
            </>
          ) : null}
          {section === "skills" ? (
            <>
              <h3>Skills</h3>
              <div className="flex flex-wrap gap-2">
                {(form.skills ?? []).map((skill, i) => (
                  <span className="tag" key={`${skill.name}-${i}`}>{skill.name || "Unnamed"}
                    <button type="button" aria-label={`Remove ${skill.name}`} className="flow-x" onClick={() => setForm((f) => ({ ...f, skills: (f.skills ?? []).filter((_, j) => j !== i) }))}>✕</button>
                  </span>
                ))}
              </div>
              <input placeholder="Add a skill and press Enter" onKeyDown={(e) => {
                if (e.key !== "Enter") return;
                e.preventDefault();
                const value = e.currentTarget.value.trim();
                if (value) setForm((f) => ({ ...f, skills: [...(f.skills ?? []), { name: value, evidence: [], confidence: 0.7 }] }));
                e.currentTarget.value = "";
              }} />
            </>
          ) : null}
          {section === "experience" ? (
            <>
              <div className="flex items-center justify-between gap-2">
                <h3>Experience</h3>
                <button className="btn secondary" onClick={() => setForm((f) => ({ ...f, experience: [...(f.experience ?? []), { company: "", title: "", dates: "", location: "", bullets: [] }] }))}>Add job</button>
              </div>
              {roles.length === 0 ? <p className="text-xs text-muted">No roles yet.</p> : null}
              {roles.map((role, ri) => (
                <div className="flow-entry" key={ri}>
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <b>{role.title || "Untitled role"}{role.company ? ` · ${role.company}` : ""}</b>
                    <span className="flex gap-1">
                      <button className="btn secondary" type="button" onClick={() => moveRole(ri, -1)} disabled={ri === 0}>Up</button>
                      <button className="btn secondary" type="button" onClick={() => moveRole(ri, 1)} disabled={ri === roles.length - 1}>Down</button>
                    </span>
                  </div>
                  <label>Title</label>
                  <input value={role.title || ""} onChange={(e) => setRole(ri, { title: e.target.value })} />
                  <label>Company</label>
                  <input value={role.company || ""} onChange={(e) => setRole(ri, { company: e.target.value })} />
                  <label>Dates</label>
                  <input value={role.dates || ""} onChange={(e) => setRole(ri, { dates: e.target.value })} placeholder="2022 – Present" />
                  <label>Location</label>
                  <input value={role.location || ""} onChange={(e) => setRole(ri, { location: e.target.value })} />
                  {(role.bullets ?? []).map((bullet, bi) => (
                    <div key={bi}>
                      <label>Bullet</label>
                      <textarea
                        value={bullet.text || ""}
                        onChange={(e) => setBullet(ri, bi, e.target.value)}
                        onKeyUp={(e) => watchWord(e.currentTarget.value, e.currentTarget.selectionStart ?? 0, (text) => setBullet(ri, bi, text))}
                        onClick={(e) => watchWord(e.currentTarget.value, e.currentTarget.selectionStart ?? 0, (text) => setBullet(ri, bi, text))}
                      />
                      <div className="flex flex-wrap gap-2">
                        <button className="btn secondary" disabled={busy === `improve-${ri}-${bi}`} onClick={() => void improveBullet(ri, bi, bullet.text || "")}>
                          {busy === `improve-${ri}-${bi}` ? "Improving…" : "Improve"}
                        </button>
                        <button className="btn secondary" onClick={() => setRole(ri, { bullets: (role.bullets ?? []).filter((_, j) => j !== bi) })}>Remove</button>
                      </div>
                      {suggest && suggest.role === ri && suggest.bullet === bi ? (
                        <div className="ai-change">
                          <p>{suggest.text}</p>
                          {suggest.note ? <p className="text-xs text-muted">{suggest.note}</p> : null}
                          <button className="btn" onClick={() => { setBullet(ri, bi, suggest.text); setSuggest(null); }}>Use this</button>
                          <button className="btn secondary" onClick={() => setSuggest(null)}>Dismiss</button>
                        </div>
                      ) : null}
                    </div>
                  ))}
                  <div className="flex flex-wrap gap-2">
                    <button className="btn secondary" onClick={() => setRole(ri, { bullets: [...(role.bullets ?? []), { text: "", source_excerpt: "" }] })}>Add bullet</button>
                    <button className="btn secondary" onClick={() => setForm((f) => ({ ...f, experience: (f.experience ?? []).filter((_, j) => j !== ri) }))}>Remove job</button>
                  </div>
                </div>
              ))}
              <h3>Draft a bullet</h3>
              <input value={draftSkill} onChange={(e) => setDraftSkill(e.target.value)} placeholder="Skill, e.g. ClickHouse" />
              <select value={draftRole} onChange={(e) => setDraftRole(Number(e.target.value))}>
                {roles.map((r, i) => <option key={i} value={i}>{r.title || "Role"} · {r.company || "Company"}</option>)}
              </select>
              <button className="btn secondary" disabled={busy === "drafts"} onClick={() => void suggestBullets()}>{busy === "drafts" ? "Drafting…" : "Draft bullets"}</button>
              {draftNote ? <p className="text-xs text-muted">{draftNote}</p> : null}
              {drafts.map((draft) => (
                <div className="flow-entry" key={draft}>
                  <p>{draft}</p>
                  <button className="btn secondary" onClick={() => {
                    if (roles.length === 0) {
                      setMessage("Add a job first.");
                      return;
                    }
                    const index = Math.max(0, Math.min(draftRole, roles.length - 1));
                    setRole(index, { bullets: [...(roles[index].bullets ?? []), { text: draft, source_excerpt: draft }] });
                    setDrafts((d) => d.filter((x) => x !== draft));
                  }}>Add to job</button>
                </div>
              ))}
            </>
          ) : null}
          {section === "projects" ? (
            <>
              <div className="flex items-center justify-between gap-2">
                <h3>Projects</h3>
                <button className="btn secondary" onClick={() => setForm((f) => ({ ...f, projects: [...(f.projects ?? []), { name: "", description: "", technologies: [] }] }))}>Add project</button>
              </div>
              {(form.projects ?? []).map((project, i) => (
                <div className="flow-entry" key={i}>
                  <label>Name</label>
                  <input value={project.name || ""} onChange={(e) => setForm((f) => ({ ...f, projects: (f.projects ?? []).map((p, j) => (j === i ? { ...p, name: e.target.value } : p)) }))} />
                  <label>Description</label>
                  <textarea
                    value={project.description || ""}
                    onChange={(e) => setForm((f) => ({ ...f, projects: (f.projects ?? []).map((p, j) => (j === i ? { ...p, description: e.target.value } : p)) }))}
                    onKeyUp={(e) => watchWord(e.currentTarget.value, e.currentTarget.selectionStart ?? 0, (description) => setForm((f) => ({ ...f, projects: (f.projects ?? []).map((p, j) => (j === i ? { ...p, description } : p)) })))}
                    onClick={(e) => watchWord(e.currentTarget.value, e.currentTarget.selectionStart ?? 0, (description) => setForm((f) => ({ ...f, projects: (f.projects ?? []).map((p, j) => (j === i ? { ...p, description } : p)) })))}
                  />
                  <label>Technologies</label>
                  <input value={(project.technologies ?? []).join(", ")} onChange={(e) => setForm((f) => ({ ...f, projects: (f.projects ?? []).map((p, j) => (j === i ? { ...p, technologies: e.target.value.split(",").map((t) => t.trim()).filter(Boolean) } : p)) }))} />
                  <button className="btn secondary" onClick={() => setForm((f) => ({ ...f, projects: (f.projects ?? []).filter((_, j) => j !== i) }))}>Remove</button>
                </div>
              ))}
            </>
          ) : null}
          {section === "education" ? (
            <>
              <div className="flex items-center justify-between gap-2">
                <h3>Education</h3>
                <button className="btn secondary" onClick={() => setForm((f) => ({ ...f, education: [...(f.education ?? []), { qualification: "", institution: "", dates: "" }] }))}>Add education</button>
              </div>
              {(form.education ?? []).map((item, i) => (
                <div className="flow-entry" key={i}>
                  <label>Qualification</label>
                  <input value={item.qualification || ""} onChange={(e) => setForm((f) => ({ ...f, education: (f.education ?? []).map((x, j) => (j === i ? { ...x, qualification: e.target.value } : x)) }))} />
                  <label>Institution</label>
                  <input value={item.institution || ""} onChange={(e) => setForm((f) => ({ ...f, education: (f.education ?? []).map((x, j) => (j === i ? { ...x, institution: e.target.value } : x)) }))} />
                  <label>Dates</label>
                  <input value={item.dates || ""} onChange={(e) => setForm((f) => ({ ...f, education: (f.education ?? []).map((x, j) => (j === i ? { ...x, dates: e.target.value } : x)) }))} />
                  <button className="btn secondary" onClick={() => setForm((f) => ({ ...f, education: (f.education ?? []).filter((_, j) => j !== i) }))}>Remove</button>
                </div>
              ))}
            </>
          ) : null}
          {section === "layout" ? (
            <>
              <h3>Layout</h3>
              <p className="text-xs text-muted">The template stays at the top. Drag these blocks to change their order. You can also drag a section on the page.</p>
              <div className="layout-list">
                {layoutOf(form).map((id) => {
                  const field = (form.custom_fields ?? []).find((item) => item.id === id);
                  const label = field?.label
                    || (id === "summary" ? sectionTitles.summary_title
                      : id === "skills" ? sectionTitles.skills_title
                        : id === "experience" ? sectionTitles.experience_title
                          : id === "projects" ? sectionTitles.projects_title
                            : id === "education" ? sectionTitles.education_title
                              : id === "open_source" ? "Open source" : "Custom");
                  return (
                    <div
                      key={id}
                      className="layout-row"
                      draggable
                      onDragStart={() => setDragId(id)}
                      onDragOver={(event) => event.preventDefault()}
                      onDrop={() => {
                        if (!dragId || dragId === id) return;
                        setForm((current) => {
                          const list = layoutOf(current);
                          const from = list.indexOf(dragId);
                          const to = list.indexOf(id);
                          if (from < 0 || to < 0) return current;
                          const [item] = list.splice(from, 1);
                          list.splice(to, 0, item);
                          return { ...current, layout: list };
                        });
                        setDragId(null);
                      }}
                    >
                      <span aria-hidden="true">⋮⋮</span>
                      <strong>{label}</strong>
                    </div>
                  );
                })}
              </div>
              <h3>Custom fields</h3>
              <p className="text-xs text-muted">These sit on the template as extra sections. Edit the title and the text, then drag the block.</p>
              {(form.custom_fields ?? []).map((field, index) => (
                <div key={field.id} className="flow-entry">
                  <label>Title</label>
                  <input
                    value={field.label}
                    onChange={(event) => setForm((current) => ({
                      ...current,
                      custom_fields: (current.custom_fields ?? []).map((item, i) => i === index ? { ...item, label: event.target.value } : item),
                    }))}
                  />
                  <label>Text</label>
                  <textarea
                    rows={4}
                    value={field.body}
                    onChange={(event) => {
                      const value = event.target.value;
                      setForm((current) => ({
                        ...current,
                        custom_fields: (current.custom_fields ?? []).map((item, i) => i === index ? { ...item, body: value } : item),
                      }));
                      watchWord(value, event.target.selectionStart ?? value.length, (next) => setForm((current) => ({
                        ...current,
                        custom_fields: (current.custom_fields ?? []).map((item, i) => i === index ? { ...item, body: next } : item),
                      })));
                    }}
                  />
                  <button
                    type="button"
                    className="btn secondary"
                    onClick={() => setForm((current) => ({
                      ...current,
                      custom_fields: (current.custom_fields ?? []).filter((item) => item.id !== field.id),
                      layout: layoutOf(current).filter((item) => item !== field.id),
                    }))}
                  >Remove</button>
                </div>
              ))}
              <button
                type="button"
                className="btn secondary"
                onClick={() => {
                  const id = `c${Date.now().toString(36)}`;
                  setForm((current) => ({
                    ...current,
                    custom_fields: [...(current.custom_fields ?? []), { id, label: "Custom", body: "" }],
                    layout: [...layoutOf(current), id],
                  }));
                }}
              >Add field</button>
            </>
          ) : null}
          {section === "design" ? (
            <>
              <h3>Design</h3>
              <p className="text-xs text-muted">The page on the right is your parsed resume: name, headline, summary, skills, jobs, projects, education, and open source. Titles below are the section headings on that page.</p>
              <StylePicker value={style} onChange={onStyle} />
              <label>Google font</label>
              <select value={font} onChange={(e) => setFont(e.target.value)} style={{ fontFamily: font }}>
                {GOOGLE_FONTS.map((name) => <option key={name} value={name} style={{ fontFamily: name }}>{name}</option>)}
              </select>
              <label>Text style</label>
              <select value={fontWeight} onChange={(e) => setFontWeight(Number(e.target.value))}>
                <option value={400}>Regular</option>
                <option value={600}>Medium</option>
                <option value={700}>Bold</option>
              </select>
              <label>Body size ({bodySize} pt)</label>
              <input type="range" min={9} max={14} step={0.5} value={bodySize} onChange={(e) => setBodySize(Number(e.target.value))} />
              <label>Name size ({headingSize} px)</label>
              <input type="range" min={16} max={36} step={1} value={headingSize} onChange={(e) => setHeadingSize(Number(e.target.value))} />
              <label>Subtitle size ({subtitleSize} px)</label>
              <p className="text-xs text-muted">Headline under your name, and each job title.</p>
              <input type="range" min={10} max={22} step={1} value={subtitleSize} onChange={(e) => setSubtitleSize(Number(e.target.value))} />
              <label>Section title size ({sectionSize} px)</label>
              <input type="range" min={8} max={18} step={1} value={sectionSize} onChange={(e) => setSectionSize(Number(e.target.value))} />
              <h3>Section titles</h3>
              {(["summary_title", "skills_title", "experience_title", "projects_title", "education_title"] as const).map((key) => (
                <div key={key}>
                  <label>{key.replace("_title", "")}</label>
                  <input value={sectionTitles[key]} onChange={(e) => setSectionTitles((current) => ({ ...current, [key]: e.target.value }))} />
                </div>
              ))}
            </>
          ) : null}
          {section !== "design" && section !== "ai" ? (
            <div className="flow-entry">
              <div className="flex items-center justify-between gap-2">
                <h3>Suggestions</h3>
                <button className="btn secondary" disabled={busy === "ideas"} onClick={() => void loadSuggestions()}>{busy === "ideas" ? "Asking…" : "Ask the API"}</button>
              </div>
              {ideaNote ? <p className="text-xs text-muted">{ideaNote}</p> : null}
              {ideas.map((idea) => (
                <div key={idea} className="ai-change">
                  <p>{idea}</p>
                  <button className="btn secondary" onClick={() => applyIdea(idea)}>Use this</button>
                </div>
              ))}
            </div>
          ) : null}
          {lookup && section !== "design" ? <DictionaryCard word={lookup.word} onUse={lookup.replace} /> : null}
          {section === "ai" ? (
            <AiEditor
              form={form}
              onApply={(patch) => {
                setForm((current) => {
                  const experience = (current.experience ?? []).map((role, roleIndex) => ({
                    ...role,
                    bullets: (role.bullets ?? []).map((bullet, index) => {
                      const next = patch.bullets.find((item) => item.role === roleIndex && item.index === index);
                      return next ? { ...bullet, text: next.text } : bullet;
                    }),
                  }));
                  return {
                    ...current,
                    headline: patch.headline ?? current.headline,
                    summary: patch.summary ?? current.summary,
                    experience,
                  };
                });
                setMessage("Applied. The page on the right is updated. Save when it looks right.");
                if (patch.headline) setSection("profile");
                else if (patch.summary) setSection("summary");
                else if (patch.bullets.length) setSection("experience");
              }}
            />
          ) : null}
        </div>
        <div className="flow-canvas">
          <p className="flow-canvas-label">
            {section === "design"
              ? "Choose a design"
              : `How ${EDITOR_SECTIONS.find((item) => item.id === section)?.label ?? "this"} looks`}
          </p>
          {section === "design" ? (
            <div className="design-grid">
              {RESUME_STYLES.map((item) => (
                <button key={item.id} type="button" className={`design-card ${style === item.id ? "on" : ""}`} onClick={() => onStyle(item.id)}>
                  {designPreviews[item.id] ? <iframe className="design-thumb" title={item.label} srcDoc={designPreviews[item.id]} /> : <span className="design-thumb design-thumb-empty">Preview</span>}
                  <b>{item.label}</b>
                  <span>{item.blurb}</span>
                </button>
              ))}
            </div>
          ) : (
            <ResumePreview html={previewHtml} />
          )}
        </div>
      </div>
    </div>
  );
}
function SourceEditor({ onImported, onExit }: {
  onImported: (profile: ResumeProfile) => void;
  onExit: () => void;
}) {
  const ropeRef = useRef<Rope | null>(null);
  const stackRef = useRef<UndoStack | null>(null);
  const [text, setText] = useState("");
  const [ready, setReady] = useState(false);
  const [status, setStatus] = useState("Loading editing session…");
  const [canUndo, setCanUndo] = useState(false);
  const [canRedo, setCanRedo] = useState(false);
  const saveTimer = useRef<number | null>(null);

  const syncFlags = () => {
    const stack = stackRef.current;
    setCanUndo(stack?.canUndo ?? false);
    setCanRedo(stack?.canRedo ?? false);
  };

  const persist = (snapshot: string) => {
    const stack = stackRef.current;
    if (!stack) return;
    if (saveTimer.current) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      const body = JSON.stringify({ snapshot, ...stack.serialize() });
      void api("/api/rope/resume-source", { method: "PUT", body }).catch(() => {});
    }, 1500);
    syncFlags();
  };

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const doc = await api<{ snapshot: string; undo: unknown; redo: unknown }>("/api/rope/resume-source");
        if (cancelled) return;
        ropeRef.current = Rope.fromString(doc.snapshot || "");
        stackRef.current = UndoStack.load(doc);
        setText(ropeRef.current.toString());
        setStatus(doc.snapshot ? "Restored your unsent editing session — nothing is lost on restart." : "Blank document.");
      } catch {
        if (cancelled) return;
        try {
          const response = await fetch("/api/resume/export?format=md");
          const md = response.ok ? await response.text() : "# Candidate\n";
          ropeRef.current = Rope.fromString(md);
        } catch {
          ropeRef.current = Rope.fromString("# Candidate\n");
        }
        stackRef.current = new UndoStack();
        setText(ropeRef.current?.toString() ?? "");
        setStatus("Editing the Markdown source. Undo/redo included, session auto-saved.");
      } finally {
        if (!cancelled) {
          setReady(true);
          syncFlags();
        }
      }
    })();
    return () => {
      cancelled = true;
      if (saveTimer.current) window.clearTimeout(saveTimer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onChange = (value: string) => {
    const rope = ropeRef.current;
    const stack = stackRef.current;
    if (!rope || !stack) {
      setText(value);
      return;
    }
    applyEdit(rope, stack, rope.toString(), value);
    const next = rope.toString();
    setText(next);
    persist(next);
  };

  const doUndo = () => {
    const rope = ropeRef.current;
    const stack = stackRef.current;
    if (!rope || !stack || !stack.undoOnce(rope)) return;
    setText(rope.toString());
    persist(rope.toString());
  };

  const doRedo = () => {
    const rope = ropeRef.current;
    const stack = stackRef.current;
    if (!rope || !stack || !stack.redoOnce(rope)) return;
    setText(rope.toString());
    persist(rope.toString());
  };

  const doImport = async () => {
    setStatus("Parsing Markdown…");
    try {
      const result = await api<{ profile: ResumeProfile }>("/api/resume/import", {
        method: "POST",
        body: JSON.stringify({ markdown: ropeRef.current?.toString() ?? text }),
      });
      await api("/api/rope/resume-source", { method: "DELETE" }).catch(() => {});
      onImported(result.profile);
    } catch (error) {
      setStatus(error instanceof Error ? error.message : "Import failed");
    }
  };

  return (
    <div className="card">
      <div className="flex flex-wrap items-center justify-between gap-2.5">
        <h3>Resume source (rope editor)</h3>
        <div className="flex flex-wrap items-center gap-2.5">
          <button className="btn secondary" disabled={!canUndo} onClick={doUndo}>Undo</button>
          <button className="btn secondary" disabled={!canRedo} onClick={doRedo}>Redo</button>
          <button className="btn" onClick={() => void doImport()}>Review as structured form</button>
          <button className="btn secondary" onClick={onExit}>Back</button>
        </div>
      </div>
      <p className="text-xs text-muted">
        Every keystroke edits a rope (chunked text), Ctrl+Z / Ctrl+Shift+Z work, and the session —
        text plus undo/redo — is auto-saved to SQLite. {status}
      </p>
      {!ready ? <p className="text-muted">Loading…</p> : (
        <textarea
          value={text}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={(e) => {
            if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== "z") {
              if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "y") {
                e.preventDefault();
                doRedo();
              }
              return;
            }
            e.preventDefault();
            if (e.shiftKey) doRedo();
            else doUndo();
          }}
          rows={28}
          style={{ fontFamily: "ui-monospace, monospace", fontSize: 12.5 }}
          spellCheck={false}
        />
      )}
    </div>
  );
}

function VersionsPanel({ onRestored }: { onRestored: () => Promise<void> | void }) {
  const [versions, setVersions] = useState<{ version: number; created_at: string }[]>([]);
  const [message, setMessage] = useState("");
  const [preview, setPreview] = useState<{ version: number; name?: string; skills: number; roles: number } | null>(null);

  const load = async () => {
    try {
      const result = await api<{ versions: { version: number; created_at: string }[] }>("/api/resume/versions");
      setVersions(result.versions ?? []);
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Could not load history");
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const showPreview = async (version: number) => {
    try {
      const result = await api<{ profile: ResumeProfile }>(`/api/resume/versions/${version}`);
      const profile = result.profile ?? {};
      setPreview({
        version,
        name: profile.name,
        skills: (profile.skills ?? []).length,
        roles: (profile.experience ?? []).length,
      });
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Could not load version");
    }
  };

  const restore = async (version: number) => {
    try {
      await api(`/api/resume/versions/${version}`, { method: "POST" });
      await api("/api/rope/resume-source", { method: "DELETE" }).catch(() => undefined);
      setMessage(`Restored version ${version}. The replaced version was kept as history.`);
      await onRestored();
      await load();
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Restore failed");
    }
  };

  return (
    <div className="card">
      <div className="flex flex-wrap items-center justify-between gap-2.5">
        <h3>Version history</h3>
        <button className="btn secondary" onClick={() => void load()}>Refresh</button>
      </div>
      <p className="text-xs text-muted">Every save archives the previous version (newest first, last 20 kept). Restoring archives the current one first, so nothing is ever lost.</p>
      {message ? <div className="text-[13px] text-muted">{message}</div> : null}
      {versions.length === 0 ? <p className="text-muted">No archived versions yet — save the resume to create the first one.</p> : (
        <ul>
          {versions.map((v) => (
            <li key={v.version}>
              v{v.version} · {v.created_at.slice(0, 19).replace("T", " ")}{" "}
              <button className="btn secondary" onClick={() => void showPreview(v.version)}>Preview</button>{" "}
              <button className="btn" onClick={() => void restore(v.version)}>Restore</button>
              {preview?.version === v.version ? (
                <span className="text-xs text-muted"> — {preview.name || "Candidate"}, {preview.skills} skills, {preview.roles} roles</span>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function PreferencesPanel({
  initial,
  onSaved,
}: {
  initial: Preferences | null;
  onSaved: () => Promise<void>;
}) {
  const [message, setMessage] = useState("");
  if (!initial) return <section><h2>Matching preferences</h2><p className="text-muted">Loading…</p></section>;
  return (
    <section>
      <h2>Matching preferences</h2>
      <div className="card max-w-[720px]">
        <form
          onSubmit={async (event) => {
            event.preventDefault();
            const form = new FormData(event.currentTarget);
            const value = (name: string) => String(form.get(name) ?? "");
            const body: Preferences = {
              name: value("name"),
              summary: value("summary"),
              skills: list(value("skills")),
              experience_years: Number(value("experience_years")),
              preferred_titles: list(value("preferred_titles")),
              role_keywords: list(value("role_keywords")),
              preferred_locations: list(value("preferred_locations")),
              excluded_keywords: list(value("excluded_keywords")),
              minimum_match_score: Number(value("minimum_match_score")),
              projects: [],
              preferred_work_modes: list(value("preferred_work_modes")),
              auto_apply: form.get("auto_apply") === "on",
              email: value("email"),
              phone: value("phone"),
              skill_weights: JSON.parse(value("skill_weights") || "[]") as SkillWeight[],
            };
            try {
              await api("/api/preferences", { method: "PUT", body: JSON.stringify(body) });
              setMessage("Preferences saved.");
              await onSaved();
            } catch (error) {
              setMessage(error instanceof Error ? error.message : "Save failed");
            }
          }}
        >
          <label>Name</label>
          <input name="name" defaultValue={initial.name} />
          <label>Professional summary</label>
          <textarea name="summary" defaultValue={initial.summary} />
          <label>Skills (comma-separated)</label>
          <textarea name="skills" defaultValue={initial.skills.join(", ")} />
          <WeightageResume initial={initial} />
          <label>Experience in years</label>
          <input name="experience_years" type="number" step="0.5" min="0" defaultValue={initial.experience_years} />
          <label>Preferred job titles</label>
          <input name="preferred_titles" defaultValue={initial.preferred_titles.join(", ")} />
          <label>Extra role keywords</label>
          <input name="role_keywords" placeholder="fellow, sre, quant" defaultValue={initial.role_keywords.join(", ")} />
          <p className="text-xs text-muted">These are added to the built-in role words and to words from your preferred titles when a search result title is split into role and company.</p>
          <label>Preferred locations</label>
          <input name="preferred_locations" defaultValue={initial.preferred_locations.join(", ")} />
          <label>Excluded keywords</label>
          <input name="excluded_keywords" defaultValue={initial.excluded_keywords.join(", ")} />
          <label>Preferred work modes</label>
          <input name="preferred_work_modes" defaultValue={initial.preferred_work_modes.join(", ")} />
          <label>Email for applications</label>
          <input name="email" type="email" defaultValue={initial.email ?? ""} placeholder="you@example.com" />
          <label>Phone (optional)</label>
          <input name="phone" defaultValue={initial.phone ?? ""} />
          <label className="check m-0 flex cursor-pointer items-start gap-2 text-[13px] font-normal normal-case tracking-normal text-inherit">
            <input name="auto_apply" type="checkbox" defaultChecked={Boolean(initial.auto_apply)} />
            <span>Auto-apply to every job at or above the minimum score. Greenhouse, Lever, and Ashby jobs are submitted. Other sites are saved as drafts for you to send.</span>
          </label>
          <ScoreField initial={initial.minimum_match_score} />
          <button className="btn">Save preferences</button>
        </form>
        <div className="text-[13px] text-muted">{message}</div>
      </div>
    </section>
  );
}

function WeightageResume({ initial }: { initial: Preferences }) {
  const starting = initial.skill_weights?.length
    ? initial.skill_weights
    : initial.skills.map((skill) => ({ skill, weight: 1 }));
  const [rows, setRows] = useState<SkillWeight[]>(starting);
  const [extra, setExtra] = useState("");
  const [note, setNote] = useState("");

  async function build(event: MouseEvent<HTMLButtonElement>) {
    const form = event.currentTarget.form;
    if (!form) return;
    const data = new FormData(form);
    try {
      const suggested = await api<SkillWeight[]>("/api/preferences/suggested-weights", {
        method: "POST",
        body: JSON.stringify({
          summary: String(data.get("summary") ?? ""),
          skills: list(String(data.get("skills") ?? "")),
        }),
      });
      setRows(suggested);
      setNote("Built from your summary and experience. Set a skill to 0 if you do not want it to shortlist jobs, then save.");
    } catch (error) {
      setNote(error instanceof Error ? error.message : "Could not build weights");
    }
  }

  return (
    <div>
      <label>Resume weightage</label>
      <p className="text-xs text-muted">Each skill is 0 to 10. Saved weights are what shortlisting uses. 10 is your strongest. 0 ignores that skill even if the summary mentions it.</p>
      <input type="hidden" name="skill_weights" value={JSON.stringify(rows)} />
      {rows.map((row, index) => (
        <div className="weight-row my-1 flex items-center justify-between gap-3" key={`${row.skill}-${index}`}>
          <span>{row.skill}</span>
          <input
            type="number"
            min={0}
            max={10}
            step={1}
            value={row.weight}
            onChange={(event) => {
              const weight = Number(event.target.value);
              setRows(rows.map((item, itemIndex) => (itemIndex === index ? { ...item, weight } : item)));
            }}
          />
        </div>
      ))}
      <div className="flex flex-wrap items-center gap-2.5">
        <input value={extra} onChange={(event) => setExtra(event.target.value)} placeholder="Add a skill, such as Node.js" />
        <button
          className="btn secondary"
          type="button"
          onClick={() => {
            const skill = extra.trim();
            if (!skill) return;
            setRows([...rows, { skill, weight: 0 }]);
            setExtra("");
          }}
        >
          Add
        </button>
        <button className="btn secondary" type="button" onClick={(event) => void build(event)}>Build from resume</button>
      </div>
      <p className="text-xs text-muted">{note}</p>
    </div>
  );
}

function ScoreField({ initial }: { initial: number }) {
  const [score, setScore] = useState(initial);
  return (
    <>
      <label>Minimum match score: <b>{score}</b></label>
      <input name="minimum_match_score" type="range" min="0" max="100" value={score} onChange={(event) => setScore(Number(event.target.value))} />
    </>
  );
}

function SearchKeyRow({ name, blurb, configured, hint, inputName, onRemove }: {
  name: string;
  blurb: string;
  configured: boolean;
  hint?: string | null;
  inputName: string;
  onRemove: () => void;
}) {
  return (
    <div className="card mb-3">
      <div className="flex flex-wrap items-center justify-between gap-2.5">
        <div>
          <strong>{name}</strong>
          <div className="text-xs text-muted">{blurb}</div>
        </div>
        <div className="flex flex-wrap items-center gap-2.5">
          <span className={configured ? "status ok" : "status"}>
            {configured ? (hint || "key saved") : "not connected"}
          </span>
          {configured ? <button className="btn secondary" type="button" onClick={onRemove}>Remove</button> : null}
        </div>
      </div>
      <label>{name} API key</label>
      <input name={inputName} type="password" autoComplete="off" placeholder={configured ? "Leave blank to keep the saved key" : `Paste a ${name} key to enable it`} />
    </div>
  );
}

export function JobsPanel({ jobs,
  total,
  page,
  perPage,
  query,
  similarId,
  similarTitle,
  status,
  sources,
  onRefresh,
  onSearch,
  onClearSearch,
  onSimilar,
  onPage,
  onPerPage,
  onDrafted,
}: {
  jobs: JobResult[];
  total: number;
  page: number;
  perPage: number;
  query: string;
  similarId: string | null;
  similarTitle: string | null;
  status: DiscoveryStatus | null;
  sources: Source[];
  onRefresh: () => Promise<void>;
  onSearch: (query: string) => void;
  onClearSearch: () => void;
  onSimilar: (id: string, title: string) => void;
  onPage: (page: number) => void;
  onPerPage: (perPage: number) => void;
  onDrafted: () => Promise<void>;
}) {
  const [searchConfig, setSearchConfig] = useState<SearchConfig | null>(null);
  const [locationMessage, setLocationMessage] = useState("");
  const [searchMessage, setSearchMessage] = useState("");
  const [boardMessage, setBoardMessage] = useState("");
  const [sourceMessage, setSourceMessage] = useState("");
  const [searchInput, setSearchInput] = useState(query);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  useEffect(() => {
    setSearchInput(query);
  }, [query]);
  const [draftFor, setDraftFor] = useState<JobResult | null>(null);
  const [bullets, setBullets] = useState<string[]>([]);
  const [summary, setSummary] = useState("");
  const [note, setNote] = useState("");
  const [selected, setSelected] = useState<Record<string, boolean>>({});
  const [tailoredFor, setTailoredFor] = useState<JobResult | null>(null);
  const [tailored, setTailored] = useState<TailoredResumeResult | null>(null);
  const [tailoredStyle, setTailoredStyle] = useState("classic");
  const [tailoredHtml, setTailoredHtml] = useState("");
  const [tailoredMessage, setTailoredMessage] = useState("");
  const [prepFor, setPrepFor] = useState<JobResult | null>(null);
  const [prep, setPrep] = useState<PrepSheetResult | null>(null);
  const [prepMessage, setPrepMessage] = useState("");
  const draftRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    draftRef.current?.scrollIntoView({ behavior: "smooth", block: "start" });
  }, [draftFor, prepFor, tailoredFor]);
  useEffect(() => {
    api<SearchConfig>("/api/discovery/search-config")
      .then(setSearchConfig)
      .catch(() => setLocationMessage("Location settings are unavailable until the dashboard is restarted."));
  }, [status?.last_run?.finished_at]);

  async function saveLocation(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!searchConfig) return;
    try {
      const saved = await api<SearchConfig>("/api/discovery/search-config", {
        method: "PUT",
        body: JSON.stringify(searchConfig),
      });
      setSearchConfig(saved);
      setLocationMessage(saved.location
        ? `Saved. Search starts in ${saved.location}${saved.nearby.length ? `, then ${saved.nearby.join(", ")}` : ""}.`
        : "Saved. The next search fills this from your current location.");
    } catch (error) {
      setLocationMessage(error instanceof Error ? error.message : "Could not save location");
    }
  }

  async function saveKeys(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const serpapi = String(form.get("serpapi") ?? "").trim();
    const jsearch = String(form.get("jsearch") ?? "").trim();
    const tavily = String(form.get("tavily") ?? "").trim();
    const brave = String(form.get("brave") ?? "").trim();
    if (!serpapi && !jsearch && !tavily && !brave) {
      setSearchMessage("Enter a SerpApi, JSearch, Tavily, or Brave key. A blank field keeps the saved key.");
      return;
    }
    try {
      const next = await api<DiscoveryStatus & { note?: string }>("/api/discovery/search", {
        method: "POST",
        body: JSON.stringify({ serpapi_api_key: serpapi || null, jsearch_api_key: jsearch || null, tavily_api_key: tavily || null, brave_api_key: brave || null }),
      });
      event.currentTarget.reset();
      setSearchMessage(next.note || "Search keys saved.");
      await onRefresh();
    } catch (error) {
      setSearchMessage(error instanceof Error ? error.message : "Could not save keys");
    }
  }

  async function removeKey(which: "serpapi" | "jsearch" | "tavily" | "brave") {
    try {
      const body: Record<string, boolean> = {};
      if (which === "serpapi") body.remove_serpapi = true;
      if (which === "jsearch") body.remove_jsearch = true;
      if (which === "tavily") body.remove_tavily = true;
      if (which === "brave") body.remove_brave = true;
      const next = await api<DiscoveryStatus & { note?: string }>("/api/discovery/search", {
        method: "POST",
        body: JSON.stringify(body),
      });
      setSearchMessage(next.note || "Key removed.");
      await onRefresh();
    } catch (error) {
      setSearchMessage(error instanceof Error ? error.message : "Could not remove key");
    }
  }

  async function saveBoard(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const token = String(form.get("token") ?? "").trim();
    const name = String(form.get("name") ?? "").trim();
    if (!token) {
      setBoardMessage("Enter a Greenhouse board token.");
      return;
    }
    try {
      const next = await api<DiscoveryStatus & { duplicate?: boolean }>("/api/discovery/greenhouse", {
        method: "POST",
        body: JSON.stringify({ token, name: name || null }),
      });
      event.currentTarget.reset();
      setBoardMessage(next.duplicate ? "That board is already saved." : "Greenhouse board saved. Run discovery to fetch its jobs.");
      await onRefresh();
    } catch (error) {
      setBoardMessage(error instanceof Error ? error.message : "Could not save board");
    }
  }

  async function saveSource(event: FormEvent<HTMLFormElement>, open: boolean) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const url = String(form.get("url") ?? "").trim();
    const name = String(form.get("name") ?? "").trim();
    if (!url) {
      setSourceMessage("Enter a careers-page or job-posting URL first.");
      return;
    }
    try {
      const result = await api<{ note?: string; duplicate?: boolean }>("/api/discovery/sources", {
        method: "POST",
        body: JSON.stringify({ url, name: name || null }),
      });
      setSourceMessage(result.note || (result.duplicate ? "Source already exists" : "Source saved."));
      if (open) window.open(url, "_blank", "noopener");
      await onRefresh();
    } catch (error) {
      setSourceMessage(error instanceof Error ? error.message : "Could not save source");
    }
  }

  async function startDraft(job: JobResult) {
    const tailored = await api<TailorResult>(`/api/jobs/${encodeURIComponent(job.job.external_id)}/tailor`, {
      method: "POST",
      body: JSON.stringify({ job: job.job }),
    });
    const next = tailored.relevant_experience_bullets ?? [];
    setDraftFor(job);
    setBullets(next);
    setSummary(`Application draft for ${job.job.title} at ${job.job.company}.`);
    setNote(tailored.integrity_note || "Review each claim. Never add qualifications that are not supported by your resume.");
    setSelected(Object.fromEntries(next.map((bullet) => [bullet, true])));
  }

  async function createDraft() {
    if (!draftFor) return;
    await api("/api/applications", {
      method: "POST",
      body: JSON.stringify({
        external_id: draftFor.job.external_id,
        tailored_summary: summary,
        selected_bullets: bullets.filter((bullet) => selected[bullet]),
      }),
    });
    setDraftFor(null);
    await onDrafted();
  }

  async function buildTailoredResume(job: JobResult) {
    setTailoredMessage("Building tailored resume…");
    try {
      const result = await api<TailoredResumeResult>(
        `/api/jobs/${encodeURIComponent(job.job.external_id)}/tailored-resume`,
        { method: "POST", body: JSON.stringify({ job: job.job, style: tailoredStyle }) },
      );
      setTailoredFor(job);
      setTailored(result);
      setTailoredHtml(result.html ?? "");
      if (result.style) setTailoredStyle(result.style);
      setTailoredMessage(result.llm_used
        ? `Rewritten by ${result.llm_provider || "LLM"} from your uploaded resume. Pick a layout and download a PDF.`
        : "Built from your uploaded resume. Pick a layout and download a PDF — nothing was invented.");
    } catch (error) {
      setTailoredMessage(error instanceof Error ? error.message : "Could not build tailored resume");
    }
  }

  async function restyleTailored(next: string) {
    if (!tailored) {
      setTailoredStyle(next);
      return;
    }
    setTailoredStyle(next);
    try {
      const result = await api<{ html: string }>("/api/resume/render", {
        method: "POST",
        body: JSON.stringify({ style: next, tailored }),
      });
      setTailoredHtml(result.html ?? "");
    } catch (error) {
      setTailoredMessage(error instanceof Error ? error.message : "Could not restyle resume");
    }
  }

  async function buildPrep(job: JobResult, refresh: boolean) {
    setPrepMessage(refresh ? "Regenerating prep sheet…" : "Building prep sheet…");
    try {
      const result = await api<{ sheet: PrepSheetResult; cached: boolean }>(
        `/api/jobs/${encodeURIComponent(job.job.external_id)}/interview-prep`,
        { method: "POST", body: JSON.stringify({ job: job.job, refresh }) },
      );
      setPrepFor(job);
      setPrep(result.sheet);
      setPrepMessage(result.cached
        ? "Loaded saved prep sheet."
        : (result.sheet.generated_by
          ? `Rewritten by ${result.sheet.generated_by}. Practice material — verify before interviews.`
          : "Deterministic build — no prep LLM configured."));
    } catch (error) {
      setPrepMessage(error instanceof Error ? error.message : "Could not build prep sheet");
    }
  }

  async function downloadTailoredPdf() {
    if (!tailored || !tailoredFor) return;
    const company = tailoredFor.job.company.replace(/[^a-z0-9]+/gi, "-").replace(/^-|-$/g, "") || "job";
    await downloadResumePdf({ style: tailoredStyle, tailored }, `resume-${company}-${tailoredStyle}.pdf`);
  }

  const boards = status?.greenhouse_boards ?? [];
  return (
    <section>
      <div className="flex flex-wrap items-center justify-between gap-2.5">
        <h2>Discover jobs</h2>
        <button className="btn secondary" onClick={() => void onRefresh()}>Refresh</button>
      </div>
      <div className="card max-w-[720px]">
        <h3>Search locations</h3>
        <p className="text-xs text-muted">Searches run in the primary city first, then each added city, then the nearest detected cities. Remote and hybrid postings are included when their toggles are on — the scheduled run still uses the public-IP city for detection and never stores the IP. Turn off detection to keep exactly the cities you type.</p>
        {searchConfig && (
          <form onSubmit={(event) => void saveLocation(event)}>
            <label>Primary city</label>
            <input
              value={searchConfig.location}
              onChange={(event) => setSearchConfig({ ...searchConfig, location: event.target.value })}
              placeholder="Detected when a search starts"
            />
            <label>Additional cities (comma separated)</label>
            <input
              value={(searchConfig.extra_locations ?? []).join(", ")}
              onChange={(event) => setSearchConfig({
                ...searchConfig,
                extra_locations: event.target.value.split(",").map((place) => place.trim()).filter(Boolean),
              })}
              placeholder="e.g. Hyderabad, Mumbai, Pune"
            />
            <label className="header-check m-0 flex cursor-pointer items-center gap-2 text-[13px] font-normal normal-case tracking-normal text-inherit">
              <input
                type="checkbox"
                checked={searchConfig.include_remote ?? true}
                onChange={(event) => setSearchConfig({ ...searchConfig, include_remote: event.target.checked })}
              />
              Include remote jobs
            </label>
            <label className="header-check m-0 flex cursor-pointer items-center gap-2 text-[13px] font-normal normal-case tracking-normal text-inherit">
              <input
                type="checkbox"
                checked={searchConfig.include_hybrid ?? true}
                onChange={(event) => setSearchConfig({ ...searchConfig, include_hybrid: event.target.checked })}
              />
              Include hybrid jobs
            </label>
            <label>Country</label>
            <input
              value={searchConfig.target_country}
              onChange={(event) => setSearchConfig({ ...searchConfig, target_country: event.target.value })}
            />
            <label>Maximum age (days, 1–90)</label>
            <input
              type="number"
              min={1}
              max={90}
              value={searchConfig.max_age_days}
              onChange={(event) => setSearchConfig({ ...searchConfig, max_age_days: Number(event.target.value) })}
            />
            <p className="text-xs text-muted">Applies to every provider: Exa uses the exact date, Tavily/Brave use the smallest covering bucket (day/week/month/year), then postings older than this are dropped. Changing it starts a fresh cache slice.</p>
            <label className="header-check m-0 flex cursor-pointer items-center gap-2 text-[13px] font-normal normal-case tracking-normal text-inherit">
              <input
                type="checkbox"
                checked={searchConfig.follow_location}
                onChange={(event) => setSearchConfig({ ...searchConfig, follow_location: event.target.checked })}
              />
              Update the city from my current location when a search starts
            </label>
            <button className="btn" type="submit">Save location</button>
          </form>
        )}
        {searchConfig?.detected_city && (
          <p className="text-xs text-muted">
            Last detected: {searchConfig.detected_city}
            {searchConfig.detected_region ? `, ${searchConfig.detected_region}` : ""}
            {searchConfig.detected_country ? `, ${searchConfig.detected_country}` : ""}
            {searchConfig.location_source ? ` (${searchConfig.location_source})` : ""}
            {searchConfig.nearby.length ? `. Nearby: ${searchConfig.nearby.join(", ")}` : ""}
          </p>
        )}
        <div className="text-[13px] text-muted">{locationMessage}</div>
      </div>
      <div className="card max-w-[720px]">
        <div className="flex flex-wrap items-center justify-between gap-2.5">
          <h3>Web search providers</h3>
          <span className="status">{[status?.serpapi_configured, status?.jsearch_configured, status?.tavily_configured, status?.brave_configured].filter(Boolean).length} of 4 connected</span>
        </div>
        <p className="text-xs text-muted">Keyless providers (Remotive, Arbeitnow) always run. Add keys below for deeper SerpApi Google Jobs, JSearch, Tavily, or Brave coverage. Keys are sealed with AES-256-GCM in SQLite; the dashboard only ever shows masked hints like ••••••••ab12. An environment variable overrides a saved key.</p>
        <form onSubmit={saveKeys}>
          <SearchKeyRow
            name="SerpApi (Google Jobs + Glassdoor)"
            blurb="Structured postings: posted dates, salary, skills"
            configured={Boolean(status?.serpapi_configured)}
            hint={status?.serpapi_hint}
            inputName="serpapi"
            onRemove={() => void removeKey("serpapi")}
          />
          <SearchKeyRow
            name="JSearch (RapidAPI)"
            blurb="Aggregated LinkedIn, Indeed, Glassdoor, ZipRecruiter postings"
            configured={Boolean(status?.jsearch_configured)}
            hint={status?.jsearch_hint}
            inputName="jsearch"
            onRemove={() => void removeKey("jsearch")}
          />
          <SearchKeyRow
            name="Tavily"
            blurb="Answer-oriented search with page extracts"
            configured={Boolean(status?.tavily_configured)}
            hint={status?.tavily_hint}
            inputName="tavily"
            onRemove={() => void removeKey("tavily")}
          />
          <SearchKeyRow
            name="Brave"
            blurb="Independent index, country-scoped results"
            configured={Boolean(status?.brave_configured)}
            hint={status?.brave_hint}
            inputName="brave"
            onRemove={() => void removeKey("brave")}
          />
          <button className="btn" type="submit">Save search keys</button>
        </form>
        <div className="text-[13px] text-muted">{searchMessage}</div>
      </div>
      <div className="card max-w-[720px]">
        <h3>Greenhouse boards</h3>
        <p className="text-xs text-muted">Use the company token from job-boards.greenhouse.io/your-token. Discovery calls Greenhouse's official job board API.</p>
        <form onSubmit={saveBoard}>
          <label>Board token</label>
          <input name="token" placeholder="your-board-token" />
          <label>Company name (optional)</label>
          <input name="name" placeholder="Company name" />
          <button className="btn" type="submit">Save board</button>
        </form>
        <div className="text-[13px] text-muted">{boardMessage}</div>
        <ul>
          {boards.length === 0 ? <li className="text-muted">No Greenhouse boards saved yet.</li> : boards.map((board) => (
            <li key={board.token}>{board.name || board.token} <span className="text-muted">({board.token})</span></li>
          ))}
        </ul>
      </div>
      <div className="card max-w-[720px]">
        <h3>Add a permitted public job page</h3>
        <p className="text-xs text-muted">Add a public company career-page URL that permits automated access. The crawler checks robots.txt. Greenhouse board URLs use the official API. It does not crawl LinkedIn or Naukri.</p>
        <form onSubmit={(event) => void saveSource(event, false)}>
          <label>Job posting or career-page URL</label>
          <input name="url" type="url" placeholder="https://careers.example.com/jobs/backend-engineer" />
          <label>Source label (optional)</label>
          <input name="name" placeholder="Example Careers" />
          <div className="flex flex-wrap items-center gap-2.5">
            <button className="btn" type="submit">Save source</button>
            <button className="btn secondary" type="button" onClick={(event) => {
              const form = event.currentTarget.form;
              if (!form) return;
              const url = String(new FormData(form).get("url") ?? "").trim();
              if (!url) setSourceMessage("Enter a URL first.");
              else window.open(url, "_blank", "noopener");
            }}>Open URL in Chrome</button>
            <button className="btn" type="button" onClick={(event) => {
              const form = event.currentTarget.form;
              if (form) void saveSource({ preventDefault() {}, currentTarget: form } as FormEvent<HTMLFormElement>, true);
            }}>Save + open</button>
          </div>
        </form>
        <div className="text-[13px] text-muted">{sourceMessage}</div>
        <div>
          {sources.length === 0 ? "No public page sources configured." : sources.map((source) => (
            <div key={source.url}>• {source.name || "Public source"}: <a href={source.url} target="_blank" rel="noreferrer">{source.url}</a></div>
          ))}
        </div>
      </div>
      {draftFor ? (
        <div className="card draft draft-accent" ref={draftRef}>
          <h3>Generated draft · {draftFor.job.title}</h3>
          <p className="text-xs text-muted">{note}</p>
          <label>Generated cover letter</label>
          <textarea value={summary} onChange={(event) => setSummary(event.target.value)} />
          <h4>Resume lines generated for this job</h4>
          {bullets.length === 0 ? <p className="text-muted">No resume lines matched this job.</p> : null}
          {bullets.map((bullet) => (
            <label className="check m-0 flex cursor-pointer items-start gap-2 text-[13px] font-normal normal-case tracking-normal text-inherit" key={bullet}>
              <input type="checkbox" checked={Boolean(selected[bullet])} onChange={(event) => setSelected((current) => ({ ...current, [bullet]: event.target.checked }))} />
              <span>{bullet}</span>
            </label>
          ))}
          <div className="flex flex-wrap items-center gap-2.5">
            <button className="btn" onClick={() => void createDraft()}>Save draft</button>
            <button className="btn secondary" onClick={() => setDraftFor(null)}>Cancel</button>
          </div>
        </div>
      ) : null}
      {tailoredFor && tailored ? (
        <div className="card draft draft-accent" ref={draftRef}>
          <h3>Tailored resume · {tailoredFor.job.title}</h3>
          <p className="text-xs text-muted">{tailored.integrity_note || ""} {tailoredMessage}</p>
          <StylePicker value={tailoredStyle} onChange={(id) => void restyleTailored(id)} />
          <ResumePreview html={tailoredHtml} />
          <div className="flex flex-wrap items-center gap-2.5">
            <button className="btn" onClick={() => void downloadTailoredPdf().catch((error: unknown) => setTailoredMessage(error instanceof Error ? error.message : "PDF failed"))}>Download PDF</button>
            <button className="btn secondary" onClick={() => { setTailoredFor(null); setTailored(null); setTailoredHtml(""); }}>Close</button>
          </div>
        </div>
      ) : null}
      {prepFor && prep ? (
        <div className="card draft draft-accent" ref={draftRef}>
          <h3>Interview prep · {prepFor.job.title}</h3>
          <p className="text-xs text-muted">{prep.overview} {prepMessage}</p>
          <h4>Technical questions</h4>
          {prep.technical_questions.length === 0 ? <p className="text-muted">No matched skills to drill.</p> : prep.technical_questions.map((item, index) => (
            <div key={`tq-${index}`}>
              <p><strong>Q: {item.question}</strong></p>
              {item.talking_points.length > 0 ? (
                <ul>{item.talking_points.map((point) => <li key={point}>{point}</li>)}</ul>
              ) : null}
            </div>
          ))}
          <h4>Behavioral questions</h4>
          {prep.behavioral_questions.map((item, index) => (
            <div key={`bq-${index}`}>
              <p><strong>Q: {item.question}</strong></p>
              {item.talking_points.length > 0 ? (
                <ul>{item.talking_points.map((point) => <li key={point}>{point}</li>)}</ul>
              ) : null}
            </div>
          ))}
          <h4>Questions to ask them</h4>
          <ul>{prep.questions_to_ask.map((question) => <li key={question}>{question}</li>)}</ul>
          <div className="flex flex-wrap items-center gap-2.5">
            <button className="btn secondary" onClick={() => void buildPrep(prepFor, true)}>Regenerate</button>
            <button className="btn secondary" onClick={() => { setPrepFor(null); setPrep(null); }}>Close</button>
          </div>
        </div>
      ) : null}
      <Pager total={total} page={page} perPage={perPage} onPage={onPage} onPerPage={onPerPage} />
      {similarId ? (
        <div className="card">
          <p className="text-[13px] text-muted">Showing jobs similar to “{similarTitle ?? similarId}”. <button className="btn secondary" onClick={() => onClearSearch()}>Back to all jobs</button></p>
        </div>
      ) : (
        <div className="card">
          <form
            onSubmit={(event) => { event.preventDefault(); onSearch(searchInput); }}
          >
            <label>Search postings (keyword index)</label>
            <input
              value={searchInput}
              onChange={(event) => setSearchInput(event.target.value)}
              placeholder="e.g. rust kubernetes backend"
            />
            <div className="flex flex-wrap items-center gap-2.5">
              <button className="btn" type="submit">Search</button>
              {query.trim() ? <button className="btn secondary" type="button" onClick={() => { setSearchInput(""); onClearSearch(); }}>Clear</button> : null}
            </div>
          </form>
          {query.trim() ? <p className="text-[13px] text-muted">Keyword matches for “{query}”. <button className="btn secondary" onClick={() => { setSearchInput(""); onClearSearch(); }}>Back to all jobs</button></p> : null}
        </div>
      )}
      {jobs.length === 0 ? <div className="card">No jobs yet. Add a search key or permitted source, then run discovery.</div> : jobs.map((result) => (
        <article className="card job-hover" key={result.job.external_id}>
          <div className="flex flex-wrap items-center justify-between gap-2.5">
            <div>
              <h3>{result.rank ? `#${result.rank} ` : ""}{result.job.title} · {result.job.company}</h3>
              <div className="text-xs text-muted">{result.job.location || "Location unspecified"} · {result.job.source}{result.job.published_at ? ` · Posted ${result.job.published_at.slice(0, 10)}` : ""}</div>
            </div>
            <div
              className="score-ring"
              style={{ "--p": Math.max(0, Math.min(100, result.score)) } as CSSProperties}
              title={`Match score ${Math.round(result.score)}%`}
            >
              <span>{Math.round(result.score)}%</span>
            </div>
          </div>
          <div className="my-2.5 h-[7px] overflow-hidden rounded-lg bg-ink-700"><span className="block h-full rounded-full bg-accent transition-[width] duration-500" style={{ width: `${Math.max(0, Math.min(100, result.score))}%` }} /></div>
          {expandedId === result.job.external_id ? (
            <p>{result.job.description}</p>
          ) : (
            <p>{result.job.description.slice(0, 280)}{result.job.description.length > 280 ? "…" : ""}</p>
          )}
          <div>
            {(result.matched_skills ?? []).map((skill) => <span className="tag good" key={`m-${skill}`}>{skill}</span>)}
            {(result.missing_skills ?? []).map((skill) => <span className="tag bad" key={`x-${skill}`}>{skill}</span>)}
          </div>
          <p className="text-xs text-muted">{result.explanation || ""}</p>
          <div className="flex flex-wrap items-center gap-2.5">
            <a href={result.job.url} target="_blank" rel="noreferrer">Open job posting</a>
            {result.job.description.length > 280 ? (
              <button className="btn secondary" onClick={() => setExpandedId(expandedId === result.job.external_id ? null : result.job.external_id)}>
                {expandedId === result.job.external_id ? "Show less" : "Show more"}
              </button>
            ) : null}
            <button className="btn secondary" onClick={() => onSimilar(result.job.external_id, `${result.job.title} · ${result.job.company}`)}>Similar</button>
            <button className="btn" onClick={() => void startDraft(result).catch((error: unknown) => setSourceMessage(error instanceof Error ? error.message : "Could not tailor"))}>Create application draft</button>
            <button className="btn secondary" onClick={() => void buildTailoredResume(result)}>Tailored resume</button>
            <button className="btn secondary" onClick={() => void buildPrep(result, false)}>Interview prep</button>
          </div>
        </article>
      ))}
      <Pager total={total} page={page} perPage={perPage} onPage={onPage} onPerPage={onPerPage} />
    </section>
  );
}

function Pager({ total, page, perPage, onPage, onPerPage }: {
  total: number;
  page: number;
  perPage: number;
  onPage: (page: number) => void;
  onPerPage: (perPage: number) => void;
}) {
  const pages = Math.max(1, Math.ceil(total / Math.max(1, perPage)));
  return (
    <div className="card">
      <div className="flex flex-wrap items-center justify-between gap-2.5">
        <span className="text-[13px] text-muted">Page {Math.min(page, pages)} of {pages} · {total} jobs</span>
        <div className="flex flex-wrap items-center gap-2.5">
          <label className="m-0 text-[13px] text-muted">Per page</label>
          <select value={perPage} onChange={(event) => onPerPage(Number(event.target.value))}>
            {[10, 15, 25, 50].map((n) => <option key={n} value={n}>{n}</option>)}
          </select>
          <button className="btn secondary" disabled={page <= 1} onClick={() => onPage(page - 1)}>Prev</button>
          <button className="btn secondary" disabled={page >= pages} onClick={() => onPage(page + 1)}>Next</button>
        </div>
      </div>
    </div>
  );
}

export function ApplicationsPanel({
  applications,
  onChanged,
}: {
  applications: Application[];
  onChanged: () => Promise<void>;
}) {
  const [notes, setNotes] = useState<Record<string, string>>({});
  const [message, setMessage] = useState("");
  const [emailMessage, setEmailMessage] = useState("");
  const [emailStatus, setEmailStatus] = useState<{ configured: boolean; oauth_connected: boolean; oauth_configured: boolean; last_sync: string | null } | null>(null);
  const statuses = ["pending_review", "approved", "applied", "applied_manually", "interview", "selected", "offer", "rejected", "withdrawn"];
  useEffect(() => {
    api<{ configured: boolean; oauth_connected: boolean; oauth_configured: boolean; last_sync: string | null }>("/api/email/status")
      .then(setEmailStatus)
      .catch(() => setEmailStatus(null));
  }, []);
  async function applyAll() {
    if (!window.confirm("Draft every matching job, and submit Greenhouse, Lever, and Ashby applications when your email and resume are saved? Other sites stay drafts: approve them, fill with the extension, submit, and the extension marks them applied.")) {
      return;
    }
    try {
      const result = await api<{ drafted: number; submitted: number; needs_manual: number; skipped: number }>(
        "/api/applications/auto",
        { method: "POST" },
      );
      setMessage(`Drafted ${result.drafted}. Submitted ${result.submitted} applications. ${result.needs_manual} still need you to submit them. Skipped ${result.skipped} existing applications.`);
      await onChanged();
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Auto-apply failed");
    }
  }
  async function syncEmail() {
    setEmailMessage("Reading recruiter mail…");
    try {
      const result = await api<{ skipped_no_config: boolean; checked: number; matched: number; updated: { company: string; from: string; to: string }[] }>(
        "/api/applications/sync-email",
        { method: "POST" },
      );
      if (result.skipped_no_config) {
        setEmailMessage("Email sync is not configured. Set EMAIL_IMAP_USER and EMAIL_IMAP_PASS (a Gmail app password), then retry.");
      } else if (result.updated.length === 0) {
        setEmailMessage(`Checked ${result.checked} recent mails. No recruiter outcomes matched your applications.`);
      } else {
        setEmailMessage(`Checked ${result.checked} mails. Updated: ${result.updated.map((u) => `${u.company} → ${u.to}`).join(", ")}.`);
      }
      await onChanged();
    } catch (error) {
      setEmailMessage(error instanceof Error ? error.message : "Email sync failed");
    }
  }
  async function connectGmail() {
    try {
      const result = await api<{ url: string }>("/api/email/oauth/start");
      window.open(result.url, "_blank", "width=520,height=640");
      setEmailMessage("A Google sign-in tab opened. Approve read-only mail access, then press “Sync email outcomes”.");
    } catch (error) {
      setEmailMessage(error instanceof Error ? error.message : "Gmail connect failed");
    }
  }
  async function disconnectGmail() {
    await api("/api/email/disconnect", { method: "POST" });
    setEmailStatus((current) => current ? { ...current, oauth_connected: false, configured: false } : current);
    setEmailMessage("Gmail disconnected. App-password IMAP still works if EMAIL_IMAP_USER/PASS are set.");
  }
  async function setStatus(id: string, status: string, existingNotes = "") {
    await api(`/api/applications/${id}/status`, {
      method: "POST",
      body: JSON.stringify({ status, notes: notes[id] ?? existingNotes }),
    });
    await onChanged();
  }
  return (
    <section>
      <div className="flex flex-wrap items-center justify-between gap-2.5">
        <h2>Applications</h2>
        <div className="flex flex-wrap items-center gap-2.5">
          <button className="btn secondary" onClick={() => void syncEmail()}>Sync email outcomes</button>
          <button className="btn" onClick={() => void applyAll()}>Auto-apply now</button>
        </div>
      </div>
      <p className="text-[13px] text-muted">Jobs at or above your minimum score are drafted automatically. Greenhouse, Lever, and Ashby submit through their application APIs and move straight to “applied”. Every other site: approve the draft, fill it with the browser extension, submit it yourself, and the extension marks it “applied”. Recruiter replies picked up by email sync move applications to “interview”, “offer”, or “rejected”.</p>
      {message ? <p className="text-[13px] text-muted">{message}</p> : null}
      {emailMessage ? <p className="text-[13px] text-muted">{emailMessage}</p> : null}
      <p className="text-[13px] text-muted">
        Gmail: {emailStatus == null ? "checking…" : emailStatus.oauth_connected ? "connected (SSO, read-only)" : emailStatus.configured ? "IMAP app-password configured" : "not connected"}
        {" · "}
        {emailStatus?.oauth_connected
          ? <button className="btn secondary" onClick={() => void disconnectGmail()}>Disconnect Gmail</button>
          : <button className="btn secondary" onClick={() => void connectGmail()}>Connect Gmail (SSO)</button>}
      </p>
      {applications.length === 0 ? <div className="card">No application drafts yet.</div> : applications.map((application) => {
        const bullets = application.selected_bullets ?? [];
        return (
          <article className="card application" key={application.id}>
            <div className="flex flex-wrap items-center justify-between gap-2.5">
              <div>
                <h3>{application.job_title} · {application.company}</h3>
                <span className="status">{application.status}</span>
              </div>
              <a href={application.job_url} target="_blank" rel="noreferrer">Open posting</a>
            </div>
            <div className="draft">
              <h4>Generated cover letter</h4>
              <pre className="draft-body">{application.tailored_summary || "No cover letter was stored."}</pre>
              <h4>Resume lines used</h4>
              {bullets.length === 0 ? <p className="text-muted">No resume lines were selected.</p> : (
                <ol>
                  {bullets.map((bullet) => <li key={bullet}>{bullet}</li>)}
                </ol>
              )}
              {application.notes ? <><h4>Saved note</h4><p>{application.notes}</p></> : null}
            </div>
            <label>Optional note</label>
            <input value={notes[application.id] ?? application.notes ?? ""} onChange={(event) => setNotes((current) => ({ ...current, [application.id]: event.target.value }))} placeholder="Application URL or confirmation" />
            <div className="flex flex-wrap items-center gap-2.5">
              <select
                value={application.status}
                onChange={(event) => void setStatus(application.id, event.target.value, application.notes ?? "")}
              >
                {statuses.map((s) => <option key={s} value={s}>{s}</option>)}
              </select>
              <button className="btn secondary" onClick={() => void setStatus(application.id, "applied_manually", application.notes ?? "")}>Mark applied manually</button>
            </div>
          </article>
        );
      })}
    </section>
  );
}

export function ExtensionPanel() {
  const [message, setMessage] = useState("");
  const extensionsPage = "chrome://extensions";
  async function copyText(text: string, done: string) {
    try {
      await navigator.clipboard.writeText(text);
      setMessage(done);
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Copy failed — select and copy manually.");
    }
  }
  function download() {
    setMessage("");
    window.open("/api/extension/download", "_blank", "noopener");
  }
  return (
    <section>
      <h2>Autofill extension</h2>
      <div className="card max-w-[720px]">
        <p className="text-[13px] text-muted">Fills empty application fields from the resume and preferences stored here, and attaches your saved resume file. Then you submit yourself and press “I submitted — mark applied” in the popup. It skips demographic questions and does not submit.</p>
        <h3>Install (one time)</h3>
        <ol>
          <li>Download the extension below and unzip it anywhere, or use the <code>extension</code> folder in this project.</li>
          <li>Paste <code>{extensionsPage}</code> into Chrome’s address bar (Chrome blocks pages from opening it — copy it with the button) and turn on <strong>Developer mode</strong>.</li>
          <li>Choose <strong>Load unpacked</strong> and select the unzipped folder. Pin the extension so the popup is one click away.</li>
          <li>Keep this agent running, open an employer form, click <strong>Fill from my resume</strong>.</li>
        </ol>
        <div className="flex flex-wrap items-center gap-2.5">
          <button className="btn" onClick={download}>Download extension (.zip)</button>
          <button className="btn secondary" onClick={() => void copyText(extensionsPage, "Copied — paste it into Chrome’s address bar.")}>Copy chrome://extensions</button>
        </div>
        <div className="text-xs text-muted">{message}</div>
      </div>
    </section>
  );
}

export function SettingsPanel({ status }: { status: DiscoveryStatus | null }) {
  const boards = status?.greenhouse_boards ?? [];
  const [cacheMessage, setCacheMessage] = useState("");
  async function clearCache() {
    setCacheMessage("");
    try {
      await api("/api/discovery/cache/clear", { method: "POST" });
      setCacheMessage("Today's search pages were retired. They stay in SQLite until the weekly cleanup. The next discovery fetches new pages.");
    } catch (error) {
      setCacheMessage(error instanceof Error ? error.message : "Could not clear the search cache");
    }
  }
  return (
    <section>
      <h2>Settings & status</h2>
      <div className="card">
        <h3>Scheduler and providers</h3>
        <p>Schedule: every {status?.interval_hours ?? 4} hours</p>
        <p>Providers configured: {status?.provider_count ?? "—"}</p>
        <p>SerpApi (Google Jobs + Glassdoor): {status?.serpapi_configured ? "key saved" : "not configured"}</p>
        <p>JSearch (RapidAPI): {status?.jsearch_configured ? "key saved" : "not configured"}</p>
        <p>Tavily search: {status?.tavily_configured ? "key saved" : "not configured"}</p>
        <p>Brave search: {status?.brave_configured ? "key saved" : "not configured"}</p>
        <p>Greenhouse boards: {boards.length ? boards.map((board) => board.name || board.token).join(", ") : "none saved"}</p>
        <p>ONNX feature compiled: {status?.onnx_enabled ? "yes" : "no"}</p>
        <p className="text-[13px] text-muted">Search pages are cached in SQLite for today, one row per page. SerpApi returns structured postings per query, Tavily and Brave up to 20. Older days are marked deleted, and a weekly cleanup removes rows that have been deleted for 7 days.</p>
        <button className="btn secondary" onClick={() => void clearCache()}>Clear search cache</button>
        {cacheMessage ? <p className="text-[13px] text-muted">{cacheMessage}</p> : null}
        <pre>{JSON.stringify(status?.last_run ?? null, null, 2)}</pre>
      </div>
      <div className="card">
        <h3>ONNX embeddings</h3>
        <p>Build with <code>--features onnx</code> and set <code>EMBEDDING_MODEL</code> and <code>TOKENIZER_JSON</code>. Without these settings, deterministic scoring remains active.</p>
        <h3>LLM reranking</h3>
        <p>Resume extraction can use Ollama (<code>OLLAMA_BASE_URL</code>, <code>OLLAMA_MODEL</code>) or OpenAI (<code>OPENAI_API_KEY</code>, <code>OPENAI_MODEL</code>). Set <code>RESUME_LLM_PROVIDER=both</code> to try Ollama first.</p>
      </div>
    </section>
  );
}

export function CompaniesPanel() {
  const [companies, setCompanies] = useState<CompanyEntry[]>([]);
  const [selected, setSelected] = useState<CompanyEntry | null>(null);
  const [guide, setGuide] = useState<CompanyGuideResult | null>(null);
  const [message, setMessage] = useState("");
  const [notes, setNotes] = useState("");

  async function load() {
    try {
      setCompanies(await api<CompanyEntry[]>("/api/companies"));
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Could not load companies");
    }
  }

  useEffect(() => {
    void load();
  }, []);

  async function openGuide(entry: CompanyEntry, refresh: boolean) {
    setMessage(refresh ? "Regenerating study guide…" : "Building study guide…");
    try {
      const result = await api<CompanyGuideResult>(
        `/api/companies/${encodeURIComponent(entry.key)}/guide`,
        refresh ? { method: "POST", body: JSON.stringify({ refresh: true }) } : undefined,
      );
      setSelected(entry);
      setGuide(result);
      setNotes(result.notes || "");
      setMessage(result.cached ? "Loaded saved guide." : "Guide built from this company's postings.");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Could not build guide");
    }
  }

  async function saveNotes() {
    if (!selected) return;
    try {
      await api(`/api/companies/${encodeURIComponent(selected.key)}/notes`, {
        method: "PUT",
        body: JSON.stringify({ notes }),
      });
      setMessage("Notes saved to the knowledge base.");
      await load();
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "Could not save notes");
    }
  }

  return (
    <section>
      <div className="flex flex-wrap items-center justify-between gap-2.5">
        <h2>Companies</h2>
        <button className="btn secondary" onClick={() => void load()}>Refresh</button>
      </div>
      <p className="text-xs text-muted">Knowledge base built from your saved postings and applications. Guides are inferred from those postings — not insider information. Add your own process intel per company.</p>
      {message ? <div className="text-[13px] text-muted">{message}</div> : null}
      {companies.length === 0 ? <div className="card">No companies yet. Run discovery first.</div> : companies.map((entry) => (
        <article className="card job-hover" key={entry.key}>
          <div className="flex flex-wrap items-center justify-between gap-2.5">
            <div>
              <h3>{entry.name}</h3>
              <div className="text-xs text-muted">
                {entry.jobs} posting{entry.jobs === 1 ? "" : "s"} · {entry.applications} application{entry.applications === 1 ? "" : "s"}
                {entry.top_role ? ` · top: ${entry.top_role} (${Math.round(entry.top_score)}%)` : ""}
                {entry.statuses.length ? ` · ${entry.statuses.join(", ")}` : ""}
                {entry.has_guide ? " · guide saved" : ""}
              </div>
              {entry.notes_preview ? <p className="text-xs text-muted">Notes: {entry.notes_preview}</p> : null}
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2.5">
            <button className="btn" onClick={() => void openGuide(entry, false)}>Study guide</button>
          </div>
        </article>
      ))}
      {guide && selected ? (
        <div className="card draft draft-accent">
          <h3>{guide.company} · DSA + System Design + Resume prep</h3>
          <p className="text-xs text-muted">{guide.guide.integrity_note}</p>
          <p>{guide.guide.roles.join(" · ")} ({guide.guide.seniority}) · {guide.guide.applications} application(s)</p>
          <div>
            {(guide.guide.top_skills ?? []).map((skill) => <span className="tag good" key={`cs-${skill}`}>{skill}</span>)}
          </div>
          <h4>DSA topics</h4>
          <ul>{guide.guide.dsa_topics.map((topic) => <li key={topic.topic}><strong>[{topic.priority}] {topic.topic}</strong> — {topic.why}</li>)}</ul>
          <h4>System design</h4>
          <ul>{guide.guide.system_design.map((item) => <li key={item.name}><strong>{item.name}</strong> — {item.notes}</li>)}</ul>
          <h4>Capacity estimation</h4>
          <p>{guide.guide.capacity_notes}</p>
          <h4>DSA practice lists</h4>
          <ul>{(guide.guide.dsa_lists ?? []).map((link) => (
            <li key={link.url}><a href={link.url} target="_blank" rel="noreferrer">{link.title}</a></li>
          ))}</ul>
          <h4>Low-level design</h4>
          <ul>{(guide.guide.lld_practice ?? []).map((prompt) => <li key={prompt}>{prompt}</li>)}</ul>
          {(guide.guide.lld_links ?? []).length > 0 ? (
            <ul>{(guide.guide.lld_links ?? []).map((link) => (
              <li key={link.url}><a href={link.url} target="_blank" rel="noreferrer">{link.title}</a></li>
            ))}</ul>
          ) : null}
          <h4>Resume tips</h4>
          <ul>{guide.guide.resume_tips.map((tip) => <li key={tip}>{tip}</li>)}</ul>
          {(guide.guide.primer_links ?? []).length > 0 ? (
            <>
              <h4>System design reading (external)</h4>
              <ul>{(guide.guide.primer_links ?? []).map((link) => (
                <li key={link.url}><a href={link.url} target="_blank" rel="noreferrer">{link.title}</a></li>
              ))}</ul>
            </>
          ) : null}
          {(guide.guide.community_qa ?? []).length > 0 ? (
            <>
              <h4>Community Q&amp;A (quoted verbatim)</h4>
              {(guide.guide.community_qa ?? []).map((qa) => (
                <div key={`${qa.topic}-${qa.question}`}>
                  <p><strong>Q: {qa.question}</strong> <span className="text-xs text-muted">[{qa.topic}]</span></p>
                  <p className="text-xs">{qa.answer}</p>
                  {qa.generated ? (
                    <p className="text-xs text-muted"><span className="tag good">Generated for you</span></p>
                  ) : (
                    <p className="text-xs text-muted">
                      Source: <a href={qa.source_url} target="_blank" rel="noreferrer">{qa.source_repo}</a> · {qa.source_license}
                    </p>
                  )}
                </div>
              ))}
            </>
          ) : null}
          <h4>Your notes (process, known questions)</h4>
          <textarea value={notes} onChange={(event) => setNotes(event.target.value)} placeholder="e.g. Round 1: DSA on arrays. Round 2: design a notification service…" />
          <div className="flex flex-wrap items-center gap-2.5">
            <button className="btn" onClick={() => void saveNotes()}>Save notes</button>
            <button className="btn secondary" onClick={() => void openGuide(selected, true)}>Regenerate guide</button>
            <button className="btn secondary" onClick={() => { setSelected(null); setGuide(null); }}>Close</button>
          </div>
        </div>
      ) : null}
    </section>
  );
}

export const TABS: { id: Tab; label: string }[] = [
  { id: "overview", label: "Home" },
  { id: "resume", label: "Resume" },
  { id: "preferences", label: "Weights" },
  { id: "jobs", label: "Jobs" },
  { id: "applications", label: "Applications" },
  { id: "companies", label: "Companies" },
  { id: "extension", label: "Extension" },
  { id: "settings", label: "Settings" },
];
