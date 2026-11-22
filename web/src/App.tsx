import { useCallback, useEffect, useState } from "react";
import { ThemeButton } from "./theme";
import { api } from "./api";
import { parseRoute, routePath, type Route } from "./router";
import {
  ApplicationsPanel,
  CompaniesPanel,
  ExtensionPanel,
  JobsPanel,
  Overview,
  PreferencesPanel,
  ResumePanel,
  SettingsPanel,
  TABS,
} from "./panels";
import type {
  Application,
  DiscoveryStatus,
  JobResult,
  PagedJobs,
  Preferences,
  ResumeProfile,
  Source,
  Tab,
} from "./types";

function currentPosition(): Promise<{ latitude: number; longitude: number } | null> {
  if (!navigator.geolocation) return Promise.resolve(null);
  return new Promise((resolve) => {
    const timer = window.setTimeout(() => resolve(null), 5000);
    navigator.geolocation.getCurrentPosition(
      (position) => {
        window.clearTimeout(timer);
        resolve({
          latitude: position.coords.latitude,
          longitude: position.coords.longitude,
        });
      },
      () => {
        window.clearTimeout(timer);
        resolve(null);
      },
      { enableHighAccuracy: false, timeout: 4500, maximumAge: 600_000 },
    );
  });
}

export function App() {
  const [initialRoute] = useState<Route>(() =>
    typeof window === "undefined"
      ? { tab: "overview", page: 1, perPage: 15, query: "", similarId: null }
      : parseRoute(window.location.pathname, window.location.search),
  );
  const [tab, setTab] = useState<Tab>(initialRoute.tab);
  const [health, setHealth] = useState("Connecting…");
  const [jobs, setJobs] = useState<JobResult[]>([]);
  const [jobsTotal, setJobsTotal] = useState(0);
  const [jobsPage, setJobsPage] = useState(initialRoute.page);
  const [jobsPerPage, setJobsPerPage] = useState(initialRoute.perPage);
  const [jobsQuery, setJobsQuery] = useState(initialRoute.query);
  const [similarId, setSimilarId] = useState<string | null>(initialRoute.similarId);
  const [similarTitle, setSimilarTitle] = useState<string | null>(null);
  const [applications, setApplications] = useState<Application[]>([]);
  const [status, setStatus] = useState<DiscoveryStatus | null>(null);
  const [resume, setResume] = useState<ResumeProfile | null>(null);
  const [preferences, setPreferences] = useState<Preferences | null>(null);
  const [sources, setSources] = useState<Source[]>([]);
  const [discovering, setDiscovering] = useState(false);
  const [applying, setApplying] = useState(false);
  const [notice, setNotice] = useState("");
  const [editingResume, setEditingResume] = useState(false);

  async function loadJobs(page: number, perPage: number, query: string, similar: string | null) {
    const params = new URLSearchParams({ page: String(page), per_page: String(perPage) });
    const path = similar
      ? `/api/jobs/search?${params}&similar_to=${encodeURIComponent(similar)}`
      : query.trim()
        ? `/api/jobs/search?${params}&q=${encodeURIComponent(query.trim())}`
        : `/api/jobs?${params}`;
    try {
      const result = await api<PagedJobs | JobResult[]>(path);
      const paged: PagedJobs = Array.isArray(result)
        ? { total: result.length, page: 1, per_page: result.length, jobs: result }
        : result;
      setJobs(paged.jobs ?? []);
      setJobsTotal(paged.total ?? 0);
      setJobsPage(paged.page ?? 1);
    } catch {
      setJobs([]);
    }
  }

  const refresh = useCallback(async () => {
    await Promise.allSettled([
      api<{ ok: boolean }>("/api/health").then(
        () => setHealth("API online"),
        () => setHealth("API offline"),
      ),
      api<ResumeProfile>("/api/resume").then(setResume).catch(() => undefined),
      api<Preferences>("/api/preferences").then(setPreferences).catch(() => undefined),
      api<Application[]>("/api/applications").then(setApplications).catch(() => undefined),
      api<DiscoveryStatus>("/api/discovery/status").then(setStatus).catch(() => undefined),
      api<{ sources: Source[] }>("/api/discovery/sources").then((result) => setSources(result.sources ?? [])).catch(() => undefined),
    ]);
    const route = parseRoute(window.location.pathname, window.location.search);
    await loadJobs(route.page, route.perPage, route.query, route.similarId);
  }, []);

  // Sync state from the URL: tab switches, pager, search, and browser
  // back/forward all flow through the address bar.
  const applyRoute = useCallback(async (route: Route, replace = false) => {
    const url = routePath(route);
    if (window.location.pathname + window.location.search !== url) {
      if (replace) window.history.replaceState(null, "", url);
      else window.history.pushState(null, "", url);
    }
    setTab(route.tab);
    setJobsPage(route.page);
    setJobsPerPage(route.perPage);
    setJobsQuery(route.query);
    setSimilarId(route.similarId);
    if (!route.similarId) setSimilarTitle(null);
    await Promise.allSettled([
      api<{ ok: boolean }>("/api/health").then(
        () => setHealth("API online"),
        () => setHealth("API offline"),
      ),
      api<ResumeProfile>("/api/resume").then(setResume).catch(() => undefined),
      api<Preferences>("/api/preferences").then(setPreferences).catch(() => undefined),
      api<Application[]>("/api/applications").then(setApplications).catch(() => undefined),
      api<DiscoveryStatus>("/api/discovery/status").then(setStatus).catch(() => undefined),
      api<{ sources: Source[] }>("/api/discovery/sources").then((result) => setSources(result.sources ?? [])).catch(() => undefined),
    ]);
    if (route.tab === "jobs" || route.tab === "applications") {
      await loadJobs(route.page, route.perPage, route.query, route.similarId);
    }
  }, []);

  function goTab(next: Tab) {
    const route = parseRoute(window.location.pathname, window.location.search);
    void applyRoute({ ...route, tab: next, page: 1 });
  }

  useEffect(() => {
    const onPopState = () => {
      void applyRoute(parseRoute(window.location.pathname, window.location.search), true);
    };
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, [applyRoute]);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => {
      void api<DiscoveryStatus>("/api/discovery/status").then(setStatus).catch(() => undefined);
    }, 30_000);
    return () => window.clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function runDiscovery() {
    setDiscovering(true);
    setNotice("");
    try {
      const position = await currentPosition();
      const result = await api<{ discovered: number; location?: string; nearby?: string[]; location_source?: string }>(
        "/api/discovery/run",
        {
          method: "POST",
          body: JSON.stringify(position ?? {}),
        },
      );
      const near = result.nearby?.length ? `, then ${result.nearby.join(", ")}` : "";
      const where = result.location ? ` Started near ${result.location}${near}.` : "";
      setNotice(`Discovery finished: ${result.discovered} jobs processed.${where}`);
      await refresh();
    } catch (error) {
      setNotice(error instanceof Error ? error.message : "Discovery failed");
    } finally {
      setDiscovering(false);
    }
  }

  async function setAutoApply(enabled: boolean) {
    if (!preferences) {
      setNotice("Open Matching preferences and save them before turning auto-apply on.");
      return;
    }
    const next = { ...preferences, auto_apply: enabled };
    setPreferences(next);
    try {
      await api("/api/preferences", { method: "PUT", body: JSON.stringify(next) });
      setNotice(enabled
        ? "Auto-apply is on. The next discovery run applies to shortlisted jobs."
        : "Auto-apply is off.");
    } catch (error) {
      setNotice(error instanceof Error ? error.message : "Could not save auto-apply");
      await refresh();
    }
  }

  async function applyNow() {
    if (!window.confirm("Apply to every shortlisted job? Greenhouse, Lever, and Ashby jobs are submitted when your email and resume are saved. Every other site becomes a draft.")) {
      return;
    }
    setApplying(true);
    setNotice("");
    try {
      const result = await api<{ drafted: number; submitted: number; needs_manual: number; skipped: number }>(
        "/api/applications/auto",
        { method: "POST" },
      );
      setNotice(`Auto-apply finished. Drafted ${result.drafted}. Submitted ${result.submitted}. ${result.needs_manual} still need you. Skipped ${result.skipped}.`);
      goTab("applications");
      await refresh();
    } catch (error) {
      setNotice(error instanceof Error ? error.message : "Auto-apply failed");
    } finally {
      setApplying(false);
    }
  }

  function jobsRoute(patch: Partial<{ page: number; perPage: number; query: string; similarId: string | null; title: string | null }>, replace = true) {
    const route = parseRoute(window.location.pathname, window.location.search);
    void applyRoute({
      tab: "jobs",
      page: patch.page ?? route.page,
      perPage: patch.perPage ?? route.perPage,
      query: patch.query ?? route.query,
      similarId: patch.similarId !== undefined ? patch.similarId : route.similarId,
    }, replace);
    if (patch.title !== undefined) setSimilarTitle(patch.title);
  }

  const shortlisted = jobs.filter((job) => job.status === "shortlisted" || job.score >= 65).length;
  const pending = applications.filter((application) => application.status === "pending_review").length;

  return (
    <>
      <header className={`${editingResume ? "hidden" : "flex"} sticky top-0 z-20 flex-wrap items-center justify-between gap-3.5 border-b border-edge bg-ink-950/90 px-7 py-4 backdrop-blur-xl max-md:px-4`}>
        <div>
          <h1 className="flex items-center gap-2.5 text-[19px]"><span className="brand-mark">◈</span> Job agent</h1>
          <div className="text-xs text-muted">Search, rank, and write from your resume — on this machine</div>
        </div>
        <div className="flex flex-wrap items-center gap-2.5">
          <ThemeButton />
          <span className={`status ${health === "API online" ? "ok" : health === "API offline" ? "bad" : ""}`}>{health}</span>
          <label className="m-0 flex cursor-pointer items-center gap-2 text-[13px] normal-case tracking-normal">
            <input
              type="checkbox"
              checked={Boolean(preferences?.auto_apply)}
              onChange={(event) => void setAutoApply(event.target.checked)}
            />
            <span>Auto-apply</span>
          </label>
          <button className="btn" disabled={applying} onClick={() => void applyNow()}>
            {applying ? "Auto-applying…" : "Auto-apply now"}
          </button>
          <button className="btn secondary" disabled={discovering} onClick={() => void runDiscovery()}>
            {discovering ? "Running discovery…" : "Run discovery now"}
          </button>
        </div>
      </header>
      <div className={editingResume ? "min-h-screen" : "grid min-h-[calc(100vh-69px)] grid-cols-1 lg:grid-cols-[224px_1fr]"}>
        <nav className={`${editingResume ? "hidden" : "flex"} gap-1 self-start overflow-x-auto border-b border-edge p-2.5 lg:sticky lg:top-[69px] lg:h-[calc(100vh-69px)] lg:flex-col lg:overflow-auto lg:border-b-0 lg:border-r lg:p-3`}>
          {TABS.map((item) => (
            <button key={item.id} className={`nav-btn max-lg:whitespace-nowrap ${tab === item.id ? "active" : ""}`} onClick={() => goTab(item.id)}>
              {item.label}
            </button>
          ))}
        </nav>
        <main className={tab === "resume" ? "w-full max-w-none" : "mx-auto w-full max-w-[1400px] p-7 max-md:p-4"}>
          {notice ? <p className="mb-3.5 rounded-[10px] border border-line bg-ink-900 px-3.5 py-2.5">{notice}</p> : null}
          {tab === "overview" ? <Overview jobs={jobs.length} shortlisted={shortlisted} pendingApps={pending} status={status} /> : null}
          {tab === "resume" ? <ResumePanel profile={resume} onUploaded={refresh} onEditing={setEditingResume} /> : null}
          {tab === "preferences" ? <PreferencesPanel initial={preferences} onSaved={refresh} /> : null}
          {tab === "jobs" ? (
            <JobsPanel
              jobs={jobs}
              total={jobsTotal}
              page={jobsPage}
              perPage={jobsPerPage}
              query={similarId ? "" : jobsQuery}
              similarId={similarId}
              similarTitle={similarTitle}
              status={status}
              sources={sources}
              onRefresh={refresh}
              onSearch={(next) => jobsRoute({ query: next, similarId: null, title: null, page: 1 })}
              onClearSearch={() => jobsRoute({ query: "", similarId: null, title: null, page: 1 })}
              onSimilar={(id, title) => jobsRoute({ similarId: id, title, page: 1 })}
              onPage={(next) => jobsRoute({ page: next })}
              onPerPage={(next) => jobsRoute({ perPage: next, page: 1 })}
              onDrafted={async () => { goTab("applications"); await refresh(); }}
            />
          ) : null}
          {tab === "applications" ? <ApplicationsPanel applications={applications} onChanged={refresh} /> : null}
          {tab === "companies" ? <CompaniesPanel /> : null}
          {tab === "extension" ? <ExtensionPanel /> : null}
          {tab === "settings" ? <SettingsPanel status={status} /> : null}
        </main>
      </div>
    </>
  );
}
