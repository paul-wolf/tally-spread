import { useCallback, useEffect, useRef, useState } from "react";
import type { GridSelection } from "@glideapps/glide-data-grid";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api, newId } from "./api";
import { CostGrid, emptySelection, opsForBlock, opsForClear, selectedBlock } from "./CostGrid";
import { money, parseTsv, shortDate, todayIso, toTsv, transpose } from "./format";
import type { Op, RateSource, Snapshot } from "./types";

const SYNC_EVERY_MS = 5000;

function usePrefersDark(): boolean {
  const q = "(prefers-color-scheme: dark)";
  const [dark, setDark] = useState(() => window.matchMedia(q).matches);
  useEffect(() => {
    const m = window.matchMedia(q);
    const on = () => setDark(m.matches);
    m.addEventListener("change", on);
    return () => m.removeEventListener("change", on);
  }, []);
  return dark;
}

const isTyping = (e: KeyboardEvent) => {
  const t = e.target as HTMLElement | null;
  return !!t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
};

export default function App() {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [sheetId, setSheetId] = useState<string | null>(null);
  const [selection, setSelection] = useState<GridSelection>(emptySelection);
  const [message, setMessage] = useState<{ text: string; error?: boolean } | null>(null);
  const [rateBusy, setRateBusy] = useState(false);
  const dark = usePrefersDark();

  const sheetRef = useRef<string | null>(null);
  sheetRef.current = sheetId;
  const snapRef = useRef<Snapshot | null>(null);
  snapRef.current = snap;

  const show = useCallback((s: Snapshot) => {
    setSnap(s);
    if (s.sheet) setSheetId(s.sheet.sheet_id);
  }, []);

  const fail = useCallback((e: unknown) => setMessage({ text: String(e), error: true }), []);

  const run = useCallback(
    async (p: Promise<Snapshot>): Promise<Snapshot | undefined> => {
      try {
        const s = await p;
        show(s);
        return s;
      } catch (e) {
        fail(e);
        return undefined;
      }
    },
    [show, fail],
  );

  const apply = useCallback((op: Op) => run(api.apply(op, sheetRef.current)), [run]);

  const refreshRates = useCallback(
    async (quiet: boolean) => {
      setRateBusy(true);
      try {
        const reports = await api.refreshRates();
        const failed = reports.filter((r) => !r.ok);
        const added = reports.reduce((n, r) => n + r.added, 0);
        if (!quiet || failed.length) {
          setMessage({
            text: failed.length
              ? `Could not update ${failed.map((r) => r.source.toUpperCase()).join(", ")}: ${failed[0].message}`
              : added
                ? `Rates updated (${added} new).`
                : "Rates are up to date.",
            error: failed.length > 0 && !quiet,
          });
        }
        show(await api.snapshot(sheetRef.current));
      } catch (e) {
        if (!quiet) fail(e);
      } finally {
        setRateBusy(false);
      }
    },
    [show, fail],
  );

  // Load, then fetch today's rates in the background.
  useEffect(() => {
    void run(api.snapshot(null)).then(() => refreshRates(true));
  }, [run, refreshRates]);

  // Pick up changes synced from the other machine.
  useEffect(() => {
    const t = setInterval(async () => {
      try {
        const s = await api.sync(sheetRef.current);
        if (s) show(s);
      } catch {
        /* next tick */
      }
    }, SYNC_EVERY_MS);
    return () => clearInterval(t);
  }, [show]);

  useEffect(() => {
    if (!message || message.error) return;
    const t = setTimeout(() => setMessage(null), 4000);
    return () => clearTimeout(t);
  }, [message]);

  const view = snap?.sheet ?? null;

  const selectionRef = useRef(selection);
  selectionRef.current = selection;

  /** Copy the selection as tab-separated text (Excel and Google Sheets read it). */
  const copy = useCallback(
    async (transposed: boolean, cut: boolean) => {
      const v = snapRef.current?.sheet;
      const block = v ? selectedBlock(v, selectionRef.current) : null;
      if (!v || !block) return;
      const grid = transposed ? transpose(block.values).map((r) => r.map((c) => c ?? "")) : block.values;
      try {
        await writeText(toTsv(grid));
        if (cut) {
          const ops = opsForClear(v, selectionRef.current);
          if (ops.length) await apply({ op: "batch", ops });
        } else if (transposed) {
          setMessage({ text: "Copied, transposed." });
        }
      } catch (e) {
        fail(`Could not write to the clipboard: ${e}`);
      }
    },
    [apply, fail],
  );

  /** Paste tab-separated text at the selected cell, adding rows as needed: one undo step. */
  const paste = useCallback(
    async (transposed: boolean) => {
      const v = snapRef.current?.sheet;
      const sel = selectionRef.current;
      const cell = sel.current?.cell ?? (sel.rows.length ? ([0, sel.rows.first()!] as const) : null);
      if (!v || !cell) return;
      try {
        const text = await readText();
        if (!text) return;
        let grid = parseTsv(text);
        if (transposed) grid = transpose(grid).map((r) => r.map((c) => c ?? ""));
        const ops = opsForBlock(v, [cell[0], cell[1]], grid);
        if (ops.length) await apply({ op: "batch", ops });
      } catch (e) {
        fail(`Could not read the clipboard: ${e}`);
      }
    },
    [apply, fail],
  );

  // Keyboard shortcuts that sit above the grid.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;
      const k = e.key.toLowerCase();
      if (!isTyping(e) && k === "z") {
        e.preventDefault();
        void run(e.shiftKey ? api.redo(sheetRef.current) : api.undo(sheetRef.current));
      } else if (!isTyping(e) && k === "y") {
        e.preventDefault();
        void run(api.redo(sheetRef.current));
      } else if (k === "v" && !isTyping(e)) {
        e.preventDefault();
        void paste(e.shiftKey);
      } else if (k === "c" && !isTyping(e)) {
        e.preventDefault();
        void copy(e.shiftKey, false);
      } else if (k === "x" && !isTyping(e)) {
        e.preventDefault();
        void copy(false, true);
      } else if (e.key === "PageDown" || e.key === "PageUp") {
        const sheets = snapRef.current?.sheets ?? [];
        const i = sheets.findIndex((s) => s.sheet_id === sheetRef.current);
        const next = sheets[(i + (e.key === "PageDown" ? 1 : -1) + sheets.length) % sheets.length];
        if (next) {
          e.preventDefault();
          setSelection(emptySelection);
          void run(api.snapshot(next.sheet_id));
        }
      }
    };
    // Capture phase, so the shortcuts work while the grid has focus.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [run, paste, copy]);

  if (!snap) {
    return <div className="loading">{message?.text ?? "Opening Tally…"}</div>;
  }

  return (
    <div className="app">
      <TopBar
        snap={snap}
        rateBusy={rateBusy}
        onOpenProject={(d) => run(api.openProject(d))}
        onCreateProject={(n) => run(api.createProject(n))}
        onApply={apply}
        onRefreshRates={() => refreshRates(false)}
        onImportRates={async () => {
          const path = await openDialog({
            multiple: false,
            filters: [{ name: "Rates (ECB .xml, NBU .json, HMRC .csv)", extensions: ["xml", "json", "csv"] }],
          });
          if (typeof path !== "string") return;
          const ext = path.split(".").pop()?.toLowerCase();
          const source: RateSource = ext === "xml" ? "ecb" : ext === "json" ? "nbu" : "hmrc_monthly";
          try {
            const n = await api.importRates(source, path);
            setMessage({ text: `Imported ${n} new rates.` });
            show(await api.snapshot(sheetRef.current));
          } catch (e) {
            fail(e);
          }
        }}
        onAddRate={async (base, quote, rate, date) => {
          try {
            await api.addManualRate(base, quote, rate, date);
            setMessage({ text: `Added 1 ${base} = ${rate} ${quote}.` });
            show(await api.snapshot(sheetRef.current));
          } catch (e) {
            fail(e);
          }
        }}
        onExport={async () => {
          if (!view) return;
          const path = await saveDialog({
            defaultPath: `${view.name}.xlsx`,
            filters: [{ name: "Excel workbook", extensions: ["xlsx"] }],
          });
          if (!path) return;
          try {
            await api.exportXlsx(view.sheet_id, path);
            setMessage({ text: `Exported ${path.split(/[\\/]/).pop()}.` });
          } catch (e) {
            fail(e);
          }
        }}
        onUndo={() => run(api.undo(sheetRef.current))}
        onRedo={() => run(api.redo(sheetRef.current))}
      />

      <main className="grid-area">
        {view ? (
          <CostGrid view={view} apply={apply} selection={selection} setSelection={setSelection} dark={dark} />
        ) : (
          <div className="empty">No sheets. Add one below.</div>
        )}
      </main>

      {view && <Totals view={view} />}

      <SheetTabs
        snap={snap}
        current={sheetId}
        onSelect={(id) => {
          setSelection(emptySelection);
          void run(api.snapshot(id));
        }}
        onApply={apply}
        onAdd={async () => {
          const id = newId();
          const n = snap.sheets.length + 1;
          setSelection(emptySelection);
          await run(api.apply({ op: "create_sheet", sheet_id: id, name: `Sheet ${n}` }, id));
        }}
      />

      {(message || snap.notices.length > 0) && (
        <div className={`toast ${message?.error ? "error" : ""}`} onClick={() => setMessage(null)}>
          {message?.text ?? snap.notices[0]}
        </div>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ top bar

function TopBar(props: {
  snap: Snapshot;
  rateBusy: boolean;
  onOpenProject: (dir: string) => void;
  onCreateProject: (name: string) => void;
  onApply: (op: Op) => void;
  onRefreshRates: () => void;
  onImportRates: () => void;
  onAddRate: (base: string, quote: string, rate: string, date: string) => void;
  onExport: () => void;
  onUndo: () => void;
  onRedo: () => void;
}) {
  const { snap } = props;
  const view = snap.sheet;
  const [newProject, setNewProject] = useState<string | null>(null);
  const [currencies, setCurrencies] = useState("");
  const [showRateForm, setShowRateForm] = useState(false);

  useEffect(() => setCurrencies(view ? view.currencies.join(", ") : ""), [view?.sheet_id, view?.currencies.join()]);

  const commitCurrencies = () => {
    if (!view) return;
    const codes = currencies
      .split(/[\s,;]+/)
      .map((c) => c.trim().toUpperCase())
      .filter(Boolean);
    if (codes.join() === view.currencies.join()) return;
    props.onApply({ op: "set_sheet_currencies", sheet_id: view.sheet_id, currencies: codes });
  };

  const locked = view?.rates.mode === "locked" ? view.rates.date : null;

  return (
    <header className="topbar">
      <div className="group">
        {newProject === null ? (
          <select
            className="project"
            value={snap.project}
            onChange={(e) => (e.target.value === "__new" ? setNewProject("") : props.onOpenProject(e.target.value))}
          >
            {snap.projects.map((p) => (
              <option key={p.dir_name} value={p.dir_name}>
                {p.name}
              </option>
            ))}
            <option value="__new">New project…</option>
          </select>
        ) : (
          <input
            autoFocus
            className="project"
            placeholder="Project name"
            value={newProject}
            onChange={(e) => setNewProject(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && newProject.trim()) {
                props.onCreateProject(newProject.trim());
                setNewProject(null);
              } else if (e.key === "Escape") setNewProject(null);
            }}
            onBlur={() => setNewProject(null)}
          />
        )}
      </div>

      {view && (
        <div className="group">
          <label className="field">
            <span>Show in</span>
            <input
              className="currencies"
              value={currencies}
              onChange={(e) => setCurrencies(e.target.value)}
              onBlur={commitCurrencies}
              onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
              title="Display currencies, e.g. GBP, UAH. Leave empty for the project default."
            />
          </label>
          <label className="field">
            <span>Rates</span>
            <select
              value={locked ? "locked" : "live"}
              onChange={(e) =>
                props.onApply({
                  op: "set_sheet_rates",
                  sheet_id: view.sheet_id,
                  rates: e.target.value === "live" ? { mode: "live" } : { mode: "locked", date: todayIso() },
                })
              }
            >
              <option value="live">Live</option>
              <option value="locked">Locked</option>
            </select>
            {locked && (
              <input
                type="date"
                value={locked}
                onChange={(e) =>
                  e.target.value &&
                  props.onApply({
                    op: "set_sheet_rates",
                    sheet_id: view.sheet_id,
                    rates: { mode: "locked", date: e.target.value },
                  })
                }
              />
            )}
          </label>
        </div>
      )}

      <div className="group rates" title="Latest rates held, per source">
        {snap.rate_status.map((r) => (
          <span key={r.source} className={r.latest ? "" : "missing"}>
            {r.label} {r.source === "hmrc_monthly" && r.latest ? r.latest.slice(0, 7) : shortDate(r.latest)}
          </span>
        ))}
        <button onClick={props.onRefreshRates} disabled={props.rateBusy}>
          {props.rateBusy ? "Updating…" : "Update rates"}
        </button>
        <button onClick={() => setShowRateForm((s) => !s)}>Add rate</button>
        <button onClick={props.onImportRates}>Import…</button>
      </div>

      <div className="group right">
        <button onClick={props.onUndo} disabled={!snap.can_undo} title="Undo (⌘Z)">
          Undo
        </button>
        <button onClick={props.onRedo} disabled={!snap.can_redo} title="Redo (⇧⌘Z)">
          Redo
        </button>
        <button className="primary" onClick={props.onExport} disabled={!view}>
          Export XLSX
        </button>
      </div>

      {showRateForm && (
        <RateForm
          onCancel={() => setShowRateForm(false)}
          onSave={(b, q, r, d) => {
            props.onAddRate(b, q, r, d);
            setShowRateForm(false);
          }}
        />
      )}
    </header>
  );
}

function RateForm(props: { onSave: (b: string, q: string, r: string, d: string) => void; onCancel: () => void }) {
  const [base, setBase] = useState("GBP");
  const [quote, setQuote] = useState("UAH");
  const [rate, setRate] = useState("");
  const [date, setDate] = useState(todayIso());
  return (
    <form
      className="rate-form"
      onSubmit={(e) => {
        e.preventDefault();
        if (rate.trim()) props.onSave(base, quote, rate, date);
      }}
    >
      1 <input value={base} onChange={(e) => setBase(e.target.value)} size={4} /> =
      <input autoFocus value={rate} onChange={(e) => setRate(e.target.value)} placeholder="rate" size={10} />
      <input value={quote} onChange={(e) => setQuote(e.target.value)} size={4} /> from
      <input type="date" value={date} onChange={(e) => setDate(e.target.value)} />
      <button type="submit" className="primary">
        Add
      </button>
      <button type="button" onClick={props.onCancel}>
        Cancel
      </button>
    </form>
  );
}

// ------------------------------------------------------------------ totals

function Totals({ view }: { view: NonNullable<Snapshot["sheet"]> }) {
  const mode = view.rates.mode === "live" ? `Live rates, ${shortDate(view.as_of)}` : `Rates locked at ${shortDate(view.as_of)}`;
  return (
    <section className="totals">
      <div className="big">
        {view.totals.map((t) => (
          <div key={t.currency} className={`total ${t.complete ? "" : "incomplete"}`}>
            <span className="label">Total {t.currency}</span>
            <span className="amount">{money(t.amount, t.currency)}</span>
            {!t.complete && <span className="flag">incomplete</span>}
          </div>
        ))}
      </div>
      <div className="detail">
        {view.subtotals.length > 1 && (
          <div>By currency: {view.subtotals.map((s) => money(s.amount, s.currency)).join(" · ")}</div>
        )}
        <div>
          {mode}
          {view.rates_used.length > 0 && ": "}
          {view.rates_used.map((l) => `1 ${l.from} = ${l.rate} ${l.to} (${l.source === "hmrc_monthly" ? "HMRC" : l.source.toUpperCase()} ${l.date})`).join(" · ")}
        </div>
        {view.warnings.map((w) => (
          <div key={w} className="warning">
            {w}
          </div>
        ))}
      </div>
    </section>
  );
}

// ------------------------------------------------------------------ sheet tabs

function SheetTabs(props: {
  snap: Snapshot;
  current: string | null;
  onSelect: (id: string) => void;
  onApply: (op: Op) => void;
  onAdd: () => void;
}) {
  const [renaming, setRenaming] = useState<string | null>(null);
  const [name, setName] = useState("");
  return (
    <nav className="tabs">
      {props.snap.sheets.map((s) => (
        <div
          key={s.sheet_id}
          className={`tab ${s.sheet_id === props.current ? "active" : ""}`}
          onClick={() => s.sheet_id !== props.current && props.onSelect(s.sheet_id)}
          onDoubleClick={() => {
            setRenaming(s.sheet_id);
            setName(s.name);
          }}
          title="Double-click to rename"
        >
          {renaming === s.sheet_id ? (
            <input
              autoFocus
              value={name}
              onChange={(e) => setName(e.target.value)}
              onBlur={() => setRenaming(null)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  if (name.trim() && name.trim() !== s.name) {
                    props.onApply({ op: "rename_sheet", sheet_id: s.sheet_id, name: name.trim() });
                  }
                  setRenaming(null);
                } else if (e.key === "Escape") setRenaming(null);
              }}
            />
          ) : (
            <>
              <span>{s.name}</span>
              {s.sheet_id === props.current && props.snap.sheets.length > 1 && (
                <button
                  className="close"
                  title="Archive sheet (undo with ⌘Z)"
                  onClick={(e) => {
                    e.stopPropagation();
                    props.onApply({ op: "set_sheet_archived", sheet_id: s.sheet_id, archived: true });
                  }}
                >
                  ×
                </button>
              )}
            </>
          )}
        </div>
      ))}
      <button className="add" onClick={props.onAdd} title="New sheet">
        +
      </button>
      <span className="hint">⌘C/⌘X/⌘V copy, cut, paste · ⇧⌘C/⇧⌘V transposed · ⌘D fill down · ⌘-click opens a link</span>
    </nav>
  );
}
