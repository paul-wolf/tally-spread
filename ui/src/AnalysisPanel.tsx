import { useEffect, useMemo, useRef, useState } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api } from "./api";
import type { ColumnView, Distribution, Groups, Series, SheetView, Snapshot } from "./types";

type Tab = "time" | "dist" | "groups";

const NUMERIC = new Set(["number", "money", "calc"]);
const isTimeCol = (c: ColumnView) => c.type === "date" || c.type === "datetime";
const isNum = (c: ColumnView) => NUMERIC.has(c.type);
/** Number columns, leaving out calculated ones that give dates or times. */
const numberColumns = (view: SheetView) =>
  view.columns.filter((c, j) => {
    if (!isNum(c)) return false;
    if (c.type !== "calc") return true;
    const first = view.rows.find((r) => r.cells[j]?.value)?.cells[j]?.value ?? "";
    return !/^\d{4}-\d\d-\d\d/.test(first);
  });

/** Up to 4 significant decimals, grouped: 1,234.5678 / 0.0123. */
export function fmt(v: number | null | undefined): string {
  if (v == null || !isFinite(v)) return "—";
  const a = Math.abs(v);
  const digits = a >= 1000 ? 1 : a >= 1 ? 3 : 4;
  return v.toLocaleString(undefined, { maximumFractionDigits: digits });
}

function fmtTime(secs: number): string {
  const d = new Date(secs * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function duration(secs: number): string {
  if (secs < 1) return `${fmt(secs)} s`;
  if (secs < 60) return `${fmt(secs)} s`;
  if (secs < 3600) return `${fmt(secs / 60)} min`;
  if (secs < 86400) return `${fmt(secs / 3600)} h`;
  return `${fmt(secs / 86400)} days`;
}

const PALETTE = ["#2f6fdb", "#d9534f", "#2a9d5c", "#b25c00", "#7b4fd6", "#0f8b8d"];

function cssVar(name: string, fallback: string): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

/** Tick labels for a time axis, 24-hour, as much date as the span needs. */
function timeTicks(_u: uPlot, vals: number[]): string[] {
  const span = vals.length > 1 ? vals[vals.length - 1] - vals[0] : 0;
  const p = (n: number) => String(n).padStart(2, "0");
  return vals.map((v, i) => {
    const d = new Date(v * 1000);
    const hm = `${p(d.getHours())}:${p(d.getMinutes())}`;
    const day = `${d.getDate()} ${d.toLocaleString(undefined, { month: "short" })}`;
    if (span > 3 * 86400) return day;
    if (span < 600) return `${hm}:${p(d.getSeconds())}`;
    return i === 0 || new Date(vals[i - 1] * 1000).getDate() !== d.getDate() ? `${hm}\n${day}` : hm;
  });
}

function axes(xTime: boolean, xLabel?: string): uPlot.Axis[] {
  const stroke = cssVar("--muted", "#777");
  const grid = { stroke: cssVar("--line", "#e3e3e3"), width: 1 };
  return [
    { stroke, grid, ticks: grid, label: xLabel, labelSize: xLabel ? 18 : 0, size: xTime ? 44 : 30, values: xTime ? timeTicks : (_u, vals) => vals.map((v) => fmt(v)) },
    { stroke, grid, ticks: grid, size: 64, values: (_u, vals) => vals.map((v) => fmt(v)) },
  ];
}

/** A uPlot chart that follows its container's width. */
function Chart(props: { opts: Omit<uPlot.Options, "width">; data: uPlot.AlignedData; onReady?: (u: uPlot | null) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const u = new uPlot({ ...props.opts, width: el.clientWidth } as uPlot.Options, props.data, el);
    props.onReady?.(u);
    const ro = new ResizeObserver(() => u.setSize({ width: el.clientWidth, height: props.opts.height }));
    ro.observe(el);
    return () => {
      ro.disconnect();
      props.onReady?.(null);
      u.destroy();
    };
  }, [props.opts, props.data]); // eslint-disable-line react-hooks/exhaustive-deps
  return <div ref={ref} className="an-chart" />;
}

/** Save the given charts, one above the other, as a PNG. */
async function savePng(charts: (uPlot | null)[], title: string, onError: (e: unknown) => void) {
  const live = charts.filter((c): c is uPlot => !!c);
  if (!live.length) return;
  const scale = window.devicePixelRatio || 1;
  const w = Math.max(...live.map((u) => u.ctx.canvas.width));
  const head = 34 * scale;
  const h = head + live.reduce((s, u) => s + u.ctx.canvas.height, 0);
  const out = document.createElement("canvas");
  out.width = w;
  out.height = h;
  const ctx = out.getContext("2d")!;
  ctx.fillStyle = cssVar("--panel", "#fff");
  ctx.fillRect(0, 0, w, h);
  ctx.fillStyle = cssVar("--text", "#111");
  ctx.font = `${600} ${14 * scale}px system-ui, sans-serif`;
  ctx.fillText(title, 12 * scale, 22 * scale);
  let y = head;
  for (const u of live) {
    ctx.drawImage(u.ctx.canvas, 0, y);
    y += u.ctx.canvas.height;
  }
  const path = await saveDialog({ defaultPath: `${title.replace(/[\\/:*?"<>|]+/g, "-")}.png`, filters: [{ name: "PNG image", extensions: ["png"] }] });
  if (!path) return;
  const blob: Blob = await new Promise((res) => out.toBlob((b) => res(b!), "image/png"));
  try {
    await api.writeFile(path, Array.from(new Uint8Array(await blob.arrayBuffer())));
  } catch (e) {
    onError(e);
  }
}

// ------------------------------------------------------------------ over time

function OverTime(props: { view: SheetView; cols: string[]; onError: (e: unknown) => void; onSaved: (m: string) => void }) {
  const [series, setSeries] = useState<Series | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const charts = useRef<(uPlot | null)[]>([]);
  const sync = useMemo(() => uPlot.sync(`tally-${Math.random()}`), []);

  useEffect(() => {
    if (!props.cols.length) return setSeries(null);
    let live = true;
    api
      .analyzeSeries(props.view.sheet_id, null, props.cols, 700)
      .then((s) => live && (setSeries(s), setProblem(null)))
      .catch((e) => live && setProblem(String(e)));
    return () => {
      live = false;
    };
  }, [props.view, props.cols]);

  const plots = useMemo(() => {
    if (!series || !series.x.length) return [];
    const x = series.x;
    // Unbucketed: a missing value is joined over (undefined), unless the time
    // gap is unusually long (null, a break in the line).
    const dx = x.slice(1).map((v, i) => v - x[i]).sort((a, b) => a - b);
    const typical = dx.length ? dx[Math.floor(dx.length / 2)] : 0;
    const breaks = new Set<number>();
    if (!series.bucket && typical > 0) x.forEach((v, i) => i > 0 && v - x[i - 1] > 5 * typical && breaks.add(i));
    const prep = (arr: (number | null)[]) =>
      arr.map((v, i) => (v == null ? (series.bucket || breaks.has(i) ? null : undefined) : v)) as (number | null | undefined)[];
    return series.lines.map((l, k) => {
      const color = PALETTE[k % PALETTE.length];
      const data: (number | null | undefined)[][] = [x, prep(l.avg)];
      const s: uPlot.Series[] = [{ label: series.x_name, value: (_u, v) => (v == null ? "—" : series.x_is_time ? fmtTime(v) : fmt(v)) }, { label: series.bucket ? `${l.name} (avg)` : l.name, stroke: color, width: 1.5, spanGaps: false, value: (_u, v) => fmt(v) }];
      const bands: uPlot.Band[] = [];
      if (l.min && l.max) {
        data.push(prep(l.max), prep(l.min));
        s.push({ label: "max", stroke: color + "44", width: 1, value: (_u, v) => fmt(v) }, { label: "min", stroke: color + "44", width: 1, value: (_u, v) => fmt(v) });
        bands.push({ series: [2, 3], fill: color + "22" });
      }
      const opts: Omit<uPlot.Options, "width"> = {
        height: series.lines.length > 2 ? 170 : 230,
        scales: { x: { time: series.x_is_time } },
        series: s,
        bands,
        axes: axes(series.x_is_time),
        cursor: { sync: { key: sync.key }, drag: { x: true, y: false } },
        legend: { show: true },
      };
      return { opts, data: data as uPlot.AlignedData, name: l.name + (l.currency ? ` (${l.currency})` : "") };
    });
  }, [series, sync]);

  if (!props.cols.length) return <div className="muted an-empty">Pick one or more number columns above.</div>;
  if (problem) return <div className="ce-error">{problem}</div>;
  if (!series) return <div className="muted an-empty">Working…</div>;
  return (
    <>
      <div className="an-note muted small">
        {series.points.toLocaleString()} rows against {series.x_is_time ? series.x_name : "row order"}
        {series.bucket ? ` · grouped into ${series.x_is_time ? duration(series.bucket) : `${fmt(series.bucket)}-row`} buckets: line = average, band = lowest to highest` : ""}
        {" · drag across a chart to zoom, double-click to reset"}
      </div>
      {plots.map((p, k) => (
        <div key={k} className="an-plot">
          <div className="an-plot-title">{p.name}</div>
          <Chart opts={p.opts} data={p.data} onReady={(u) => (charts.current[k] = u)} />
        </div>
      ))}
      <div className="ce-actions">
        <span className="spacer" />
        <button onClick={() => savePng(charts.current.slice(0, plots.length), `${props.view.name} – ${plots.map((p) => p.name).join(", ")}`, props.onError)}>
          Save as PNG…
        </button>
      </div>
    </>
  );
}

// ------------------------------------------------------------------ distribution

/** A box plot drawn on the same horizontal scale as the histogram above it. */
function BoxPlot({ d, area }: { d: Distribution; area: { left: number; width: number } | null }) {
  const s = d.stats!;
  const lo = d.edges[0] ?? s.min;
  const hi = d.edges[d.edges.length - 1] ?? s.max;
  const span = hi - lo || 1;
  const W = area?.width ?? 500;
  const X = (v: number) => ((v - lo) / span) * W;
  const whiskLo = Math.max(lo, s.low_fence);
  const whiskHi = Math.min(hi, s.high_fence);
  return (
    <svg viewBox={`0 0 ${W} 44`} className="an-box" style={{ marginLeft: area?.left ?? 0, width: W }} role="img" aria-label="Box plot">
      <line x1={X(whiskLo)} x2={X(s.q1)} y1={20} y2={20} className="whisker" />
      <line x1={X(s.q3)} x2={X(whiskHi)} y1={20} y2={20} className="whisker" />
      <line x1={X(whiskLo)} x2={X(whiskLo)} y1={13} y2={27} className="whisker" />
      <line x1={X(whiskHi)} x2={X(whiskHi)} y1={13} y2={27} className="whisker" />
      <rect x={X(s.q1)} y={8} width={Math.max(1, X(s.q3) - X(s.q1))} height={24} className="box" />
      <line x1={X(s.median)} x2={X(s.median)} y1={8} y2={32} className="median" />
      <circle cx={X(s.mean)} cy={20} r={3} className="mean" />
      {X(s.median) - X(s.q1) > 34 && X(s.q3) - X(s.median) > 34 && (
        <>
          <text x={X(s.q1)} y={42} textAnchor="middle">Q1</text>
          <text x={X(s.median)} y={42} textAnchor="middle">median</text>
          <text x={X(s.q3)} y={42} textAnchor="middle">Q3</text>
        </>
      )}
    </svg>
  );
}

function DistributionView(props: { view: SheetView; col: string | null; onError: (e: unknown) => void }) {
  const [d, setD] = useState<Distribution | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const chart = useRef<uPlot | null>(null);
  const [area, setArea] = useState<{ left: number; width: number } | null>(null);
  useEffect(() => {
    if (!props.col) return setD(null);
    let live = true;
    api
      .analyzeDistribution(props.view.sheet_id, props.col)
      .then((r) => live && (setD(r), setProblem(null)))
      .catch((e) => live && setProblem(String(e)));
    return () => {
      live = false;
    };
  }, [props.view, props.col]);

  const plot = useMemo(() => {
    if (!d || !d.counts.length) return null;
    const width = d.edges[1] - d.edges[0];
    const mids = d.counts.map((_, i) => d.edges[i] + width / 2);
    const color = PALETTE[0];
    const bars = uPlot.paths.bars!({ size: [0.92, Infinity], align: 0 });
    const s: uPlot.Series[] = [
      {},
      { label: "Count", stroke: color, fill: color + "55", paths: bars, points: { show: false }, value: (_u, v) => (v == null ? "—" : String(v)) },
    ];
    const data: (number | null)[][] = [mids, d.counts];
    if (d.normal.length) {
      s.push({ label: "Normal fit", stroke: PALETTE[1], width: 1.5, points: { show: false }, value: (_u, v) => fmt(v) });
      data.push(d.normal);
    }
    const opts: Omit<uPlot.Options, "width"> = {
      height: 220,
      scales: { x: { time: d.is_time, range: [d.edges[0], d.edges[d.edges.length - 1]] } },
      series: s,
      axes: axes(d.is_time),
      cursor: { drag: { x: false, y: false } },
    };
    return { opts, data: data as uPlot.AlignedData };
  }, [d]);

  if (!props.col) return <div className="muted an-empty">Pick a column above.</div>;
  if (problem) return <div className="ce-error">{problem}</div>;
  if (!d) return <div className="muted an-empty">Working…</div>;

  if (d.categories.length) {
    const total = d.categories.reduce((s, c) => s + c[1], 0);
    const top = d.categories[0][1];
    return (
      <div className="an-cats">
        <div className="muted small">
          {d.categories.length} different values in {total} rows
        </div>
        {d.categories.slice(0, 30).map(([v, n]) => (
          <div key={v} className="an-cat">
            <span className="an-cat-name" title={v}>
              {v}
            </span>
            <span className="an-cat-bar" style={{ width: `${(n / top) * 100}%` }} />
            <span className="an-cat-n">
              {n} <span className="muted">({fmt((n / total) * 100)}%)</span>
            </span>
          </div>
        ))}
        {d.categories.length > 30 && <div className="muted small">…and {d.categories.length - 30} more</div>}
      </div>
    );
  }
  const s = d.stats;
  if (!s) return <div className="muted an-empty">No values in this column.</div>;
  const v = (x: number) => (d.is_time ? fmtTime(x) : fmt(x));
  const rows: [string, string][] = [
    ["Values", `${s.count.toLocaleString()}${s.empty ? ` (${s.empty} empty)` : ""}`],
    ["Mean", v(s.mean)],
    ["Standard deviation", d.is_time ? duration(s.sd) : fmt(s.sd)],
    ["Min", v(s.min)],
    ["Q1 (25%)", v(s.q1)],
    ["Median", v(s.median)],
    ["Q3 (75%)", v(s.q3)],
    ["Max", v(s.max)],
    ["Interquartile range", d.is_time ? duration(s.q3 - s.q1) : fmt(s.q3 - s.q1)],
    ["Outliers (beyond 1.5 × IQR)", `${s.outliers}${s.outliers ? ` — below ${v(s.low_fence)} or above ${v(s.high_fence)}` : ""}`],
  ];
  if (!d.is_time) rows.push(["Sum", fmt(s.sum)], ["Skewness", fmt(s.skewness)], ["Excess kurtosis", fmt(s.kurtosis)]);
  return (
    <>
      {d.shape && <div className="an-shape">{d.shape}</div>}
      {plot && (
        <div className="an-plot">
          <div className="an-plot-title">
            {d.name}
            {d.currency ? ` (${d.currency})` : ""} · {d.counts.length} bins
          </div>
          <Chart
            opts={plot.opts}
            data={plot.data}
            onReady={(u) => {
              chart.current = u;
              const r = window.devicePixelRatio || 1;
              if (u) setArea({ left: u.bbox.left / r, width: u.bbox.width / r });
            }}
          />
        </div>
      )}
      <BoxPlot d={d} area={area} />
      <table className="an-stats">
        <tbody>
          {rows.map(([k, val]) => (
            <tr key={k}>
              <td>{k}</td>
              <td className="num">{val}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <div className="ce-actions">
        <button onClick={() => void writeText(rows.map((r) => r.join("\t")).join("\n"))}>Copy figures</button>
        <span className="spacer" />
        <button onClick={() => savePng([chart.current], `${props.view.name} – ${d.name} distribution`, props.onError)}>Save as PNG…</button>
      </div>
    </>
  );
}

// ------------------------------------------------------------------ groups

const BUCKETS: [string, number | null][] = [
  ["Automatic", null],
  ["1 second", 1],
  ["10 seconds", 10],
  ["1 minute", 60],
  ["5 minutes", 300],
  ["15 minutes", 900],
  ["1 hour", 3600],
  ["1 day", 86400],
  ["1 week", 604800],
];

function GroupsView(props: {
  view: SheetView;
  cols: string[];
  onSaved: (s: Snapshot) => void;
  onError: (e: unknown) => void;
}) {
  const { view } = props;
  // Anything but amounts and measurements: text, dates and times (typed or calculated), whole numbers.
  const measures = new Set(numberColumns(view).filter((c) => c.type !== "number").map((c) => c.col_id));
  const candidates = view.columns.filter((c) => !measures.has(c.col_id));
  const [by, setBy] = useState<string>(() => {
    const t = view.columns.find((c) => c.type === "text" && !props.cols.includes(c.col_id));
    return (t ?? view.columns.find(isTimeCol) ?? view.columns[0])?.col_id ?? "";
  });
  const [bucket, setBucket] = useState<number | null>(null);
  const [g, setG] = useState<Groups | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const byCol = view.columns.find((c) => c.col_id === by);
  const valueCols = props.cols.filter((c) => c !== by);

  useEffect(() => {
    if (!by) return;
    let live = true;
    api
      .analyzeGroups(view.sheet_id, by, valueCols, byCol && isTimeCol(byCol) ? bucket : null)
      .then((r) => live && (setG(r), setProblem(null)))
      .catch((e) => live && setProblem(String(e)));
    return () => {
      live = false;
    };
  }, [view, by, bucket, props.cols.join(",")]); // eslint-disable-line react-hooks/exhaustive-deps

  const label = (key: string, start: number | null) => (start != null ? fmtTime(start) : key);
  const tsv = () => {
    if (!g) return "";
    const head = [g.by, "Rows", ...g.columns.flatMap((c) => ["mean", "min", "median", "max"].map((a) => `${c} ${a}`))];
    const lines = g.groups.map((gr) => [label(gr.key, gr.start), String(gr.rows), ...gr.values.flatMap((a) => [a.mean, a.min, a.median, a.max].map((v) => (v == null ? "" : String(v))))]);
    return [head, ...lines].map((r) => r.join("\t")).join("\n");
  };

  return (
    <>
      <div className="an-controls">
        <label>
          Group by
          <select value={by} onChange={(e) => setBy(e.target.value)}>
            {candidates.map((c) => (
              <option key={c.col_id} value={c.col_id}>
                {c.name}
              </option>
            ))}
          </select>
        </label>
        {byCol && isTimeCol(byCol) && (
          <label>
            Every
            <select value={bucket ?? ""} onChange={(e) => setBucket(e.target.value ? Number(e.target.value) : null)}>
              {BUCKETS.map(([l, v]) => (
                <option key={l} value={v ?? ""}>
                  {l}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>
      {problem && <div className="ce-error">{problem}</div>}
      {g && (
        <>
          <div className="muted small">
            {g.groups.length} {g.groups.length === 1 ? "group" : "groups"}{g.bucket ? ` of ${duration(g.bucket)}` : ""}
            {!valueCols.length && " · pick number columns above to see their averages, ranges and medians"}
          </div>
          <div className="an-table-wrap">
            <table className="an-groups">
              <thead>
                <tr>
                  <th rowSpan={2}>{g.by}</th>
                  <th rowSpan={2} className="num">
                    Rows
                  </th>
                  {g.columns.map((c) => (
                    <th key={c} colSpan={4} className="an-colgroup">
                      {c}
                    </th>
                  ))}
                </tr>
                <tr>
                  {g.columns.flatMap((c) =>
                    ["mean", "min", "median", "max"].map((a) => (
                      <th key={c + a} className="num sub">
                        {a}
                      </th>
                    )),
                  )}
                </tr>
              </thead>
              <tbody>
                {g.groups.map((gr) => (
                  <tr key={gr.key}>
                    <td>{label(gr.key, gr.start)}</td>
                    <td className="num">{gr.rows}</td>
                    {gr.values.flatMap((a, i) =>
                      [a.mean, a.min, a.median, a.max].map((v, j) => (
                        <td key={`${i}-${j}`} className="num">
                          {fmt(v)}
                        </td>
                      )),
                    )}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="ce-actions">
            <button onClick={() => void writeText(tsv())}>Copy table</button>
            <span className="spacer" />
            <button
              className="primary"
              onClick={async () => {
                try {
                  props.onSaved(
                    await api.saveGroupsSheet(view.sheet_id, by, valueCols, byCol && isTimeCol(byCol) ? bucket : null, `${view.name} by ${g.by}`),
                  );
                } catch (e) {
                  props.onError(e);
                }
              }}
            >
              Save as a sheet
            </button>
          </div>
        </>
      )}
    </>
  );
}

// ------------------------------------------------------------------ the drawer

/** Charts and figures for the chosen columns, beside the grid. Nothing in the sheet changes. */
export function AnalysisPanel(props: {
  view: SheetView;
  initialCols: string[];
  onClose: () => void;
  onSaved: (s: Snapshot) => void;
  onError: (e: unknown) => void;
}) {
  const { view } = props;
  const hasTime = view.columns.some(isTimeCol);
  const known = (ids: string[]) => ids.filter((id) => view.columns.some((c) => c.col_id === id));
  const [cols, setCols] = useState<string[]>(() => known(props.initialCols));
  const [tab, setTab] = useState<Tab>(() => {
    const picked = view.columns.filter((c) => props.initialCols.includes(c.col_id));
    if (picked.length === 1 && !isNum(picked[0])) return "dist";
    return hasTime ? "time" : "dist";
  });
  useEffect(() => setCols(known(props.initialCols)), [props.initialCols.join(",")]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !(e.target as HTMLElement)?.closest?.("select") && props.onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props]);

  const numCols = numberColumns(view);
  const single = tab === "dist";
  const choices = tab === "dist" ? view.columns : numCols;
  const chosen = single ? cols.slice(0, 1) : cols.filter((id) => numCols.some((c) => c.col_id === id));
  const toggle = (id: string) => {
    if (single) setCols([id, ...cols.filter((c) => c !== id)]);
    else setCols(cols.includes(id) ? cols.filter((c) => c !== id) : [...cols, id]);
  };

  return (
    <aside className="analysis">
      <div className="panel-head">
        <strong>Analyse</strong>
        <div className="kinds">
          {(
            [
              ["time", "Over time"],
              ["dist", "Distribution"],
              ["groups", "Groups"],
            ] as [Tab, string][]
          ).map(([t, l]) => (
            <button key={t} className={tab === t ? "on" : ""} onClick={() => setTab(t)}>
              {l}
            </button>
          ))}
        </div>
        <button className="close-x" onClick={props.onClose} title="Close (Esc)">
          ×
        </button>
      </div>
      <div className="an-pick">
        {choices.map((c) => (
          <button key={c.col_id} className={chosen.includes(c.col_id) ? "chip on" : "chip"} onClick={() => toggle(c.col_id)} title={`${c.col_id} · ${c.var}`}>
            {c.name}
          </button>
        ))}
        {!choices.length && <span className="muted small">This sheet has no number columns.</span>}
      </div>
      <div className="an-body">
        {tab === "time" && <OverTime view={view} cols={chosen} onError={props.onError} onSaved={() => {}} />}
        {tab === "dist" && <DistributionView view={view} col={chosen[0] ?? null} onError={props.onError} />}
        {tab === "groups" && <GroupsView view={view} cols={chosen} onSaved={props.onSaved} onError={props.onError} />}
      </div>
    </aside>
  );
}
