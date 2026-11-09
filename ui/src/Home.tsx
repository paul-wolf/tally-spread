import { useEffect, useMemo, useRef, useState } from "react";
import { fuzzy } from "./ColumnJump";
import { group, money, projectStyle } from "./format";
import type { ProjectSummary, SheetEntry } from "./types";

// Home: every sheet in every project (Unfiled included), seen by recency, on a
// timeline, by project or by kind, with search. Projects are a way to group sheets
// here, not a place you have to enter before you can work.

export type HomeMode = "recent" | "time" | "project" | "kind";

const MODES: [HomeMode, string][] = [
  ["recent", "Recent"],
  ["time", "Timeline"],
  ["project", "Projects"],
  ["kind", "Kind"],
];
const MODE_KEY = "tally.homeMode";

const KIND_LABEL: Record<SheetEntry["kind"], string> = {
  money: "Money: prices, costs and totals",
  log: "Logs: dated rows",
  table: "Tables",
};

/** When a sheet was last touched: changed anywhere, or shown here. */
export const lastUsed = (e: SheetEntry) => Math.max(e.edited ?? 0, e.opened ?? 0);

export function relativeTime(ms: number, now = Date.now()): string {
  if (!ms) return "never";
  const s = Math.max(0, (now - ms) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  const d = new Date(ms);
  const today = new Date(now);
  const sameDay = d.toDateString() === today.toDateString();
  const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  if (sameDay) return `today ${time}`;
  const y = new Date(now);
  y.setDate(y.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return `yesterday ${time}`;
  const opts: Intl.DateTimeFormatOptions = { day: "numeric", month: "short" };
  if (d.getFullYear() !== today.getFullYear()) opts.year = "numeric";
  return d.toLocaleDateString(undefined, opts);
}

/**
 * Timeline buckets, finest for the recent past: the last 5 minutes, the last hour,
 * each hour of today, yesterday, each day of the last week, then months.
 */
export function timeBucket(ms: number, now = Date.now()): { key: string; label: string; order: number } {
  if (!ms) return { key: "never", label: "Never opened or changed", order: -1 };
  const age = now - ms;
  if (age < 5 * 60_000) return { key: "5m", label: "Last 5 minutes", order: now };
  if (age < 3600_000) return { key: "1h", label: "Last hour", order: now - 5 * 60_000 };
  const d = new Date(ms);
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  if (ms >= start.getTime()) {
    const h = new Date(ms);
    h.setMinutes(0, 0, 0);
    const label = `Today, ${h.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}`;
    return { key: `h${h.getTime()}`, label, order: h.getTime() };
  }
  const day = new Date(d);
  day.setHours(0, 0, 0, 0);
  const days = Math.round((start.getTime() - day.getTime()) / 86400_000);
  if (days === 1) return { key: "yesterday", label: "Yesterday", order: day.getTime() };
  if (days < 7) return { key: `d${day.getTime()}`, label: day.toLocaleDateString(undefined, { weekday: "long" }), order: day.getTime() };
  const month = new Date(d.getFullYear(), d.getMonth(), 1);
  return { key: `m${month.getTime()}`, label: month.toLocaleDateString(undefined, { month: "long", year: "numeric" }), order: month.getTime() };
}

interface Group {
  key: string;
  label: string;
  /** Set for project groups: the folder name, for Open all and New sheet. */
  project?: string;
  entries: SheetEntry[];
}

function groupEntries(entries: SheetEntry[], mode: HomeMode, searching: boolean): Group[] {
  const byRecent = [...entries].sort((a, b) => lastUsed(b) - lastUsed(a) || a.name.localeCompare(b.name));
  if (mode === "recent" || searching) return [{ key: "all", label: "", entries: byRecent }];
  const groups = new Map<string, Group & { order: number | string }>();
  const put = (key: string, label: string, order: number | string, e: SheetEntry, project?: string) => {
    let g = groups.get(key);
    if (!g) groups.set(key, (g = { key, label, order, entries: [], project }));
    g.entries.push(e);
  };
  for (const e of byRecent) {
    if (mode === "time") {
      const b = timeBucket(lastUsed(e));
      put(b.key, b.label, b.order, e);
    } else if (mode === "project") {
      // Unfiled first: it's the inbox of sheets not filed yet.
      put(e.project, e.unfiled ? "Unfiled" : e.project_name, e.unfiled ? "" : e.project_name.toLowerCase(), e, e.project);
    } else {
      put(e.kind, KIND_LABEL[e.kind], ["money", "log", "table"].indexOf(e.kind), e);
    }
  }
  const list = [...groups.values()];
  if (mode === "time") list.sort((a, b) => (b.order as number) - (a.order as number));
  else if (mode === "project") list.sort((a, b) => String(a.order).localeCompare(String(b.order)));
  else list.sort((a, b) => (a.order as number) - (b.order as number));
  return list;
}

function matches(e: SheetEntry, q: string): number {
  if (!q) return 0;
  return Math.max(fuzzy(q, e.name), fuzzy(q, e.unfiled ? "unfiled" : e.project_name) - 2);
}

export interface HomeProps {
  /** Changes whenever the data may have changed, so Home reloads its list. */
  version: unknown;
  projects: ProjectSummary[];
  unfiled: string;
  load: (includeArchived: boolean) => Promise<SheetEntry[]>;
  onOpen: (e: SheetEntry) => void;
  onRestore: (e: SheetEntry) => void;
  onOpenProject: (dir: string) => void;
  onNewSheet: (project: string | null) => void;
  onNewFromClipboard: () => void;
  onImport: () => void;
  onNewProject: (name: string) => void;
  onError: (e: unknown) => void;
}

export function Home(props: HomeProps) {
  const [entries, setEntries] = useState<SheetEntry[] | null>(null);
  const [query, setQuery] = useState("");
  const [archived, setArchived] = useState(false);
  const [newProject, setNewProject] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [mode, setMode] = useState<HomeMode>(() => {
    try {
      const m = localStorage.getItem(MODE_KEY) as HomeMode | null;
      if (m && MODES.some(([k]) => k === m)) return m;
    } catch {
      /* default */
    }
    return "recent";
  });
  const searchRef = useRef<HTMLInputElement>(null);
  const { load, onError } = props;

  useEffect(() => {
    let live = true;
    load(archived)
      .then((e) => live && setEntries(e))
      .catch((e) => live && onError(e));
    return () => {
      live = false;
    };
  }, [load, onError, archived, props.version]);

  useEffect(() => searchRef.current?.focus(), []);

  const setModeKept = (m: HomeMode) => {
    setMode(m);
    try {
      localStorage.setItem(MODE_KEY, m);
    } catch {
      /* not kept */
    }
  };

  const q = query.trim().toLowerCase();
  const shown = useMemo(() => {
    // With Archived ticked, only archived sheets (to restore them); otherwise the rest.
    const visible = (entries ?? []).filter((e) => e.archived === archived);
    if (!q) return visible;
    return visible
      .map((e) => ({ e, s: matches(e, q) }))
      .filter((x) => x.s >= 0)
      .sort((a, b) => b.s - a.s)
      .map((x) => x.e);
  }, [entries, archived, q]);

  const groups = useMemo(() => {
    const g = groupEntries(shown, mode, !!q);
    if (q) g[0].entries = shown; // best match first
    // Projects with no sheets still show in the Projects view, so they can be opened.
    if (mode === "project" && !q && !archived) {
      for (const p of props.projects) {
        if (!g.some((x) => x.project === p.dir_name)) g.push({ key: p.dir_name, label: p.name, project: p.dir_name, entries: [] });
      }
      if (!g.some((x) => x.project === props.unfiled)) g.unshift({ key: props.unfiled, label: "Unfiled", project: props.unfiled, entries: [] });
    }
    return g;
  }, [shown, mode, q, archived, props.projects, props.unfiled]);

  const now = Date.now();
  const total = (entries ?? []).filter((e) => !e.archived).length;

  return (
    <section className="home" aria-label="Home">
      <header className="home-head">
        <div className="home-title">
          <h1>Home</h1>
          <span className="home-count">
            {entries === null ? "Loading…" : archived ? `${shown.length} archived` : `${total} sheet${total === 1 ? "" : "s"}`}
          </span>
        </div>
        <input
          ref={searchRef}
          className="home-search"
          type="search"
          placeholder="Search sheets and projects"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && shown[0]) {
              e.preventDefault();
              (archived ? props.onRestore : props.onOpen)(shown[0]);
            } else if (e.key === "Escape" && query) {
              e.stopPropagation();
              setQuery("");
            }
          }}
        />
        <div className="segmented" role="tablist" aria-label="Group by">
          {MODES.map(([k, label]) => (
            <button key={k} className={`seg ${mode === k ? "on" : ""}`} onClick={() => setModeKept(k)}>
              {label}
            </button>
          ))}
        </div>
        <label className="home-archived">
          <input type="checkbox" checked={archived} onChange={(e) => setArchived(e.target.checked)} /> Archived
        </label>
        <div className="home-actions">
          <button onClick={() => props.onNewSheet(null)} title="A blank sheet in Unfiled (⌘N)">
            New sheet
          </button>
          <button onClick={props.onNewFromClipboard} title="Cells copied from Excel or a CSV become a new sheet (⇧⌘N)">
            From clipboard
          </button>
          <button onClick={props.onImport} title="Excel, CSV or JSON into Unfiled">
            Import…
          </button>
          {newProject === null ? (
            <button onClick={() => setNewProject("")}>New project…</button>
          ) : (
            <input
              autoFocus
              className="home-new-project"
              placeholder="Project name, then Enter"
              value={newProject}
              onChange={(e) => setNewProject(e.target.value)}
              onBlur={() => setNewProject(null)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && newProject.trim()) {
                  props.onNewProject(newProject.trim());
                  setNewProject(null);
                } else if (e.key === "Escape") setNewProject(null);
              }}
            />
          )}
        </div>
      </header>

      <div className="home-body">
        {entries !== null && shown.length === 0 && !(mode === "project" && !q && !archived) && (
          <div className="home-empty">
            {q ? "No sheet matches." : archived ? "No archived sheets." : "No sheets yet. ⌘N makes one."}
          </div>
        )}
        {groups.map((g) => {
          const isCollapsed = collapsed.has(g.key);
          return (
            <div key={g.key} className="home-group">
              {g.label && (
                <div className="home-group-head">
                  <button
                    className="home-twisty"
                    onClick={() =>
                      setCollapsed((c) => {
                        const n = new Set(c);
                        if (n.has(g.key)) n.delete(g.key);
                        else n.add(g.key);
                        return n;
                      })
                    }
                  >
                    {isCollapsed ? "▸" : "▾"}{" "}
                    {g.project && g.project !== props.unfiled && <span className="proj-dot" style={projectStyle(props.projects, g.project)} aria-hidden />}
                    {g.label}
                    <span className="home-group-count">{g.entries.length}</span>
                  </button>
                  {g.project && !archived && (
                    <span className="home-group-actions">
                      {g.entries.length > 0 && g.project !== props.unfiled && (
                        <button onClick={() => props.onOpenProject(g.project!)} title="Open all of its sheets as tabs">
                          Open all
                        </button>
                      )}
                      <button onClick={() => props.onNewSheet(g.project === props.unfiled ? null : g.project!)}>+ Sheet</button>
                    </span>
                  )}
                </div>
              )}
              {!isCollapsed &&
                g.entries.map((e) => (
                  <div
                    key={`${e.project}/${e.sheet_id}`}
                    className={`home-row ${e.archived ? "archived" : ""}`}
                    onClick={() => (e.archived ? props.onRestore(e) : props.onOpen(e))}
                    title={e.archived ? "Click to restore" : "Click to open"}
                  >
                    <span className={`home-kind k-${e.kind}`} aria-hidden>
                      {e.kind === "money" ? "¤" : e.kind === "log" ? "◷" : "▦"}
                    </span>
                    <span className="home-name">
                      {e.name}
                      {e.pinned && <span className="home-badge">pinned</span>}
                      {e.open && !e.pinned && <span className="home-badge">open</span>}
                    </span>
                    <span className={`chip ${e.unfiled ? "unfiled" : "proj"}`} style={e.unfiled ? undefined : projectStyle(props.projects, e.project)}>
                      {e.unfiled ? "Unfiled" : e.project_name}
                    </span>
                    <span className="home-size">
                      {group(String(e.rows))} row{e.rows === 1 ? "" : "s"}
                    </span>
                    <span className="home-total">{e.total ? (e.total_currency ? money(e.total, e.total_currency) : group(e.total)) : ""}</span>
                    <span className="home-when" title={e.edited ? `Changed ${new Date(e.edited).toLocaleString()}` : ""}>
                      {e.archived ? "Restore" : relativeTime(lastUsed(e), now)}
                    </span>
                  </div>
                ))}
            </div>
          );
        })}
      </div>
    </section>
  );
}
