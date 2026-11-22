import type { Tab } from "./types";
import { TABS } from "./panels";

export type Route = {
  tab: Tab;
  page: number;
  perPage: number;
  query: string;
  similarId: string | null;
};

const VALID_TABS = new Set<string>(TABS.map((item) => item.id));

function clampInt(value: string | null, fallback: number, min: number, max: number): number {
  const parsed = value == null ? NaN : Number.parseInt(value, 10);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.min(max, Math.max(min, parsed));
}

export function parseRoute(pathname: string, search: string): Route {
  const segment = pathname.replace(/^\/+|\/+$/g, "");
  const tab: Tab = (segment === "" ? "overview" : segment) as Tab;
  const params = new URLSearchParams(search);
  return {
    tab: VALID_TABS.has(tab) ? tab : "overview",
    page: clampInt(params.get("page"), 1, 1, 10_000),
    perPage: clampInt(params.get("per_page"), 15, 5, 50),
    query: (params.get("q") ?? "").slice(0, 200),
    similarId: params.get("similar")?.slice(0, 120) || null,
  };
}

export function routePath(route: Route): string {
  const base = route.tab === "overview" ? "/" : `/${route.tab}`;
  if (route.tab !== "jobs") return base;
  const params = new URLSearchParams();
  if (route.similarId) {
    params.set("similar", route.similarId);
  } else {
    if (route.query.trim()) params.set("q", route.query.trim());
    if (route.page > 1) params.set("page", String(route.page));
  }
  if (route.perPage !== 15) params.set("per_page", String(route.perPage));
  const query = params.toString();
  return query ? `${base}?${query}` : base;
}
