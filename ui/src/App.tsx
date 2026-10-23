import { useCallback, useEffect, useRef, useState } from "react";
import type { GridSelection, Rectangle } from "@glideapps/glide-data-grid";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api, newId, type ExportFormat } from "./api";
import { ColumnEditor } from "./ColumnEditor";
import { FunctionsPanel } from "./FunctionsPanel";
import { RateCardPanel } from "./RateCardPanel";
import { money, parseTsv, shortDate, toTsv, transpose } from "./format";
import { SheetGrid, emptySelection, opsForBlock, opsForClear, positionBetween, selectedBlock } from "./SheetGrid";
import type { ColumnSpec, Op, RateSource, SheetView, Snapshot } from "./types";
import { SOURCE_LABEL } from "./types";

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

/** Which column the editor is open for: an existing one, or a new one at a position. */
type Editing =
  | { mode: "edit"; index: number; at: { x: number; y: number } }
  | { mode: "new"; position: number; at: { x: number; y: number } };

export default function App() {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [sheetId, setSheetId] = useState<string | null>(null);
  const [selection, setSelection] = useState<GridSelection>(emptySelection);
  const [message, setMessage] = useState<{ text: string; error?: boolean } | null>(null);
  const [rateBusy, setRateBusy] = useState(false);
  const [editing, setEditing] = useState<Editing | null>(null);
  const [panel, setPanel] = useState<"rates" | "functions" | null>(null);
  const dark = usePrefersDark();

  const sheetRef = useRef<string | null>(null);
  sheetRef.current = sheetId;
  const snapRef = useRef<Snapshot | null>(null);
  snapRef.current = snap;
  const selectionRef = useRef(selection);
  selectionRef.current = selection;

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
              ? `Could not update ${failed.map((r) => SOURCE_LABEL[r.source]).join(", ")}: ${failed[0].message}`
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
    const t = setTimeout(() => setMessage(null), 5000);
    return () => clearTimeout(t);
  }, [message]);

  const view = snap?.sheet ?? null;

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

  /**
   * Paste tab-separated text (as Excel and Google Sheets copy it) at the selected
   * cell, or below the last row if nothing is selected. Adds rows as needed; the
   * whole paste is one undo step.
   */
  const paste = useCallback(
    async (transposed: boolean) => {
      const v = snapRef.current?.sheet;
      if (!v) return;
      const sel = selectionRef.current;
      const cell: [number, number] = sel.current
        ? [sel.current.cell[0], sel.current.cell[1]]
        : sel.rows.length
          ? [0, sel.rows.first()!]
          : [0, v.rows.length];
      try {
        const text = await readText();
        if (!text) return;
        let grid = parseTsv(text);
        if (transposed) grid = transpose(grid).map((r) => r.map((c) => c ?? ""));
        const { ops, skippedCells, skippedHeader } = opsForBlock(v, cell, grid);
        if (ops.length) await apply({ op: "batch", ops });
        const notes = [
          skippedHeader ? "skipped the heading row" : "",
          skippedCells ? `left out ${skippedCells} value${skippedCells > 1 ? "s" : ""} that didn't fit the column` : "",
        ].filter(Boolean);
        if (notes.length) setMessage({ text: `Pasted; ${notes.join(", ")}.` });
      } catch (e) {
        fail(`Could not paste: ${e}`);
      }
    },
    [apply, fail],
  );

  /** Rows the selection touches (whole rows, or the rows of the selected range). */
  const selectedRows = useCallback((): number[] => {
    const v = snapRef.current?.sheet;
    const sel = selectionRef.current;
    if (!v) return [];
    let ys = sel.rows.toArray();
    if (!ys.length && sel.current) {
      const r = sel.current.range;
      ys = Array.from({ length: r.height }, (_, i) => r.y + i);
    }
    return ys.filter((y) => y < v.rows.length);
  }, []);

  const insertRowAbove = useCallback(() => {
    const v = snapRef.current?.sheet;
    if (!v) return;
    const ys = selectedRows();
    const y = ys.length ? Math.min(...ys) : v.rows.length;
    const position = positionBetween(v.rows[y - 1]?.position, v.rows[y]?.position);
    void apply({ op: "insert_row", sheet_id: v.sheet_id, row_id: newId(), position });
  }, [apply, selectedRows]);

  /** Turn the selected rows into section headings, or back into items. */
  const toggleSection = useCallback(() => {
    const v = snapRef.current?.sheet;
    if (!v) return;
    const ys = selectedRows();
    if (!ys.length) {
      setMessage({ text: "Select a row first; it becomes the section heading." });
      return;
    }
    const makeSection = v.rows[ys[0]].kind !== "section";
    const ops: Op[] = ys.map((y) => ({
      op: "set_row_kind",
      sheet_id: v.sheet_id,
      row_id: v.rows[y].row_id,
      kind: makeSection ? "section" : "item",
    }));
    void apply(ops.length === 1 ? ops[0] : { op: "batch", ops });
  }, [apply, selectedRows]);

  // Keyboard shortcuts that sit above the grid.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (!mod || isTyping(e) || editing || panel) return;
      const k = e.key.toLowerCase();
      if (k === "z") {
        e.preventDefault();
        void run(e.shiftKey ? api.redo(sheetRef.current) : api.undo(sheetRef.current));
      } else if (k === "y") {
        e.preventDefault();
        void run(api.redo(sheetRef.current));
      } else if (k === "v") {
        e.preventDefault();
        void paste(e.shiftKey);
      } else if (k === "c") {
        e.preventDefault();
        void copy(e.shiftKey, false);
      } else if (k === "x") {
        e.preventDefault();
        void copy(false, true);
      } else if (e.key === "Enter") {
        e.preventDefault();
        insertRowAbove();
      } else if (e.shiftKey && k === "s") {
        e.preventDefault();
        toggleSection();
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
  }, [run, paste, copy, insertRowAbove, toggleSection, editing, panel]);

  const openColumnEditor = useCallback((index: number | "new", at: Rectangle) => {
    const v = snapRef.current?.sheet;
    if (!v) return;
    const where = { x: at.x, y: at.y + at.height + 4 };
    if (index === "new") {
      setEditing({ mode: "new", position: positionBetween(v.columns[v.columns.length - 1]?.position, undefined), at: where });
    } else {
      setEditing({ mode: "edit", index, at: where });
    }
  }, []);

  if (!snap) {
    return <div className="loading">{message?.text ?? "Opening Tally…"}</div>;
  }

  const editedColumn = editing?.mode === "edit" && view ? view.columns[editing.index] ?? null : null;

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
        onExport={async (format: ExportFormat) => {
          if (!view) return;
          const path = await saveDialog({
            defaultPath: `${view.name}.${format}`,
            filters: [{ name: EXPORT_LABEL[format], extensions: [format] }],
          });
          if (!path) return;
          try {
            await api.exportSheet(view.sheet_id, path, format);
            setMessage({ text: `Exported ${path.split(/[\\/]/).pop()}.` });
          } catch (e) {
            fail(e);
          }
        }}
        onUndo={() => run(api.undo(sheetRef.current))}
        onRedo={() => run(api.redo(sheetRef.current))}
        onInsertRow={insertRowAbove}
        onToggleSection={toggleSection}
        onOpenRateCard={() => setPanel("rates")}
        onOpenFunctions={() => setPanel("functions")}
        onAddColumn={(at) => {
          const v = snapRef.current?.sheet;
          if (!v) return;
          // Right of the selected column, or at the end.
          const sel = selectionRef.current;
          const x = sel.columns.length ? sel.columns.last()! : sel.current?.cell[0];
          const cols = v.columns;
          const position =
            x !== undefined && cols[x]
              ? positionBetween(cols[x].position, cols[x + 1]?.position)
              : positionBetween(cols[cols.length - 1]?.position, undefined);
          setEditing({ mode: "new", position, at: { x: at.x, y: at.y + at.height + 4 } });
        }}
      />

      <main className="grid-area">
        {view ? (
          <SheetGrid
            view={view}
            apply={apply}
            selection={selection}
            setSelection={setSelection}
            onEditColumn={openColumnEditor}
            dark={dark}
          />
        ) : (
          <div className="empty">No sheets. Add one below.</div>
        )}
      </main>

      {view && <CellNote view={view} selection={selection} />}
      {view && <Totals view={view} />}

      <SheetTabs
        snap={snap}
        current={sheetId}
        onSelect={(id) => {
          setSelection(emptySelection);
          void run(api.snapshot(id));
        }}
        onApply={apply}
        onAdd={async (template) => {
          setSelection(emptySelection);
          if (template === "file") {
            const path = await openDialog({
              multiple: false,
              filters: [{ name: "CSV or JSON", extensions: ["csv", "tsv", "txt", "json"] }],
            });
            if (typeof path !== "string") return;
            setMessage({ text: `Importing ${path.split(/[\\/]/).pop()}…` });
            try {
              const s = await api.importFile(path);
              show(s);
              const v = s.sheet;
              if (v) setMessage({ text: `Imported ${v.rows.length} rows and ${v.columns.length} columns into “${v.name}”. Double-click a header to rename or retype a column.` });
            } catch (e) {
              fail(e);
            }
            return;
          }
          await run(api.createSheet(template === "spending" ? "Spending" : `Sheet ${snap.sheets.length + 1}`, template));
        }}
      />

      {editing && view && (
        <ColumnEditor
          key={editing.mode === "edit" ? editedColumn?.col_id : `new-${editing.position}`}
          sheetId={view.sheet_id}
          column={editedColumn}
          others={view.columns.filter((c) => c !== editedColumn)}
          baseCurrency={view.rate_card.base}
          functions={snap.functions}
          at={editing.at}
          onClose={() => setEditing(null)}
          onSave={async (spec: ColumnSpec) => {
            const op: Op =
              editing.mode === "edit" && editedColumn
                ? { op: "update_column", sheet_id: view.sheet_id, col_id: editedColumn.col_id, spec }
                : {
                    op: "add_column",
                    sheet_id: view.sheet_id,
                    col_id: "", // the engine assigns the next ID (c1, c2, …)
                    position: editing.mode === "new" ? editing.position : 0,
                    spec,
                  };
            if (await apply(op)) setEditing(null);
          }}
          onInsert={
            editedColumn && editing.mode === "edit"
              ? (side) => {
                  const i = editing.index;
                  const cols = view.columns;
                  const position =
                    side === "left"
                      ? positionBetween(cols[i - 1]?.position, cols[i].position)
                      : positionBetween(cols[i].position, cols[i + 1]?.position);
                  setEditing({ mode: "new", position, at: editing.at });
                }
              : undefined
          }
          onDelete={
            editedColumn
              ? async () => {
                  const ok = await apply({
                    op: "set_column_deleted",
                    sheet_id: view.sheet_id,
                    col_id: editedColumn.col_id,
                    deleted: true,
                  });
                  if (ok) {
                    setEditing(null);
                    setMessage({ text: `Deleted column ${editedColumn.name}. ⌘Z brings it back.` });
                  }
                }
              : undefined
          }
        />
      )}

      {panel === "rates" && view && <RateCardPanel view={view} onApply={apply} onClose={() => setPanel(null)} />}
      {panel === "functions" && (
        <FunctionsPanel
          functions={snap.functions}
          sheetId={sheetId}
          onSaved={show}
          onError={fail}
          onClose={() => setPanel(null)}
        />
      )}

      {(message || snap.notices.length > 0) && (
        <div className={`toast ${message?.error ? "error" : ""}`}>
          <span className="toast-text">{message?.text ?? snap.notices[0]}</span>
          <button className="toast-btn" title="Copy" onClick={() => void writeText(message?.text ?? snap.notices[0])}>
            Copy
          </button>
          <button className="toast-btn" title="Dismiss" onClick={() => setMessage(null)}>
            ×
          </button>
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
  onExport: (format: ExportFormat) => void;
  onUndo: () => void;
  onRedo: () => void;
  onInsertRow: () => void;
  onToggleSection: () => void;
  onAddColumn: (at: Rectangle) => void;
  onOpenRateCard: () => void;
  onOpenFunctions: () => void;
}) {
  const { snap } = props;
  const view = snap.sheet;
  const [newProject, setNewProject] = useState<string | null>(null);

  const locked = view?.rates.mode === "locked" ? view.rates.date : null;
  const firstSource = view?.effective_sources[0] ?? "ecb";

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
        <button onClick={props.onInsertRow} title="Insert a row above the selection (⌘Enter)">
          Insert row
        </button>
        <button onClick={props.onToggleSection} title="Make the selected row a section heading, or back (⇧⌘S)">
          Section
        </button>
        <button
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            props.onAddColumn({ x: r.left, y: r.top, width: r.width, height: r.height });
          }}
          title="Add a column right of the selected one (or at the end). To edit a column, double-click its header."
        >
          + Column
        </button>
      </div>

      {view && (
        <div className="group">
          <button className="rate-card-btn" onClick={props.onOpenRateCard} title="Open the rate card: every rate this sheet uses">
            Rates: {view.rate_card.base} base · {SOURCE_LABEL[firstSource]} ·{" "}
            {locked ? `pegged ${locked}` : "floating"}
            {view.fixed_rates.length > 0 && ` · ${view.fixed_rates.length} pinned`}
          </button>
          <button onClick={props.onOpenFunctions} title="Your own functions, usable in every formula">
            ƒ Functions
          </button>
        </div>
      )}

      <div className="group rates" title="Latest rates held, per source">
        {snap.rate_status.map((r) => (
          <span key={r.source} className={r.latest ? "" : "missing"}>
            {SOURCE_LABEL[r.source]} {r.source === "hmrc_monthly" && r.latest ? r.latest.slice(0, 7) : shortDate(r.latest)}
          </span>
        ))}
        <button onClick={props.onRefreshRates} disabled={props.rateBusy}>
          {props.rateBusy ? "Updating…" : "Update rates"}
        </button>
        <button onClick={props.onImportRates}>Import…</button>
      </div>

      <div className="group right">
        <button onClick={props.onUndo} disabled={!snap.can_undo} title="Undo (⌘Z)">
          Undo
        </button>
        <button onClick={props.onRedo} disabled={!snap.can_redo} title="Redo (⇧⌘Z)">
          Redo
        </button>
        <ExportMenu disabled={!view} onExport={props.onExport} />
      </div>

    </header>
  );
}

// ------------------------------------------------------------------ export

const EXPORT_LABEL: Record<ExportFormat, string> = {
  xlsx: "Excel workbook",
  pdf: "PDF",
  csv: "CSV",
  json: "JSON",
};

/** Export the sheet: Excel, PDF for people, CSV and JSON for other tools. */
function ExportMenu(props: { disabled: boolean; onExport: (f: ExportFormat) => void }) {
  const [open, setOpen] = useState(false);
  const pick = (f: ExportFormat) => {
    setOpen(false);
    props.onExport(f);
  };
  return (
    <div className="export-menu">
      <button className="primary" disabled={props.disabled} onClick={() => setOpen((o) => !o)}>
        Export ▾
      </button>
      {open && (
        <>
          <div className="backdrop clear" onClick={() => setOpen(false)} />
          <div className="menu">
            {(["xlsx", "pdf", "csv", "json"] as ExportFormat[]).map((f) => (
              <button key={f} onClick={() => pick(f)}>
                {EXPORT_LABEL[f]} <span className="muted">.{f}</span>
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ cell note

/** The selected cell's problem, if it has one, in words you can copy. */
function CellNote({ view, selection }: { view: SheetView; selection: GridSelection }) {
  const at = selection.current?.cell;
  const cell = at ? view.rows[at[1]]?.cells[at[0]] : undefined;
  const text = cell?.error ?? cell?.warning;
  if (!at || !text) return null;
  const col = view.columns[at[0]];
  return (
    <div className={`cell-note ${cell?.error ? "error" : ""}`}>
      <span>
        Row {at[1] + 1}, {col?.name}: {text}
      </span>
      <button className="copy-link" onClick={() => void writeText(text)}>
        Copy
      </button>
    </div>
  );
}

// ------------------------------------------------------------------ totals

function Totals({ view }: { view: SheetView }) {
  // (writeText is the clipboard plugin's, imported above.)
  const mode = view.rates.mode === "live" ? `Live rates, ${shortDate(view.as_of)}` : `Rates locked at ${shortDate(view.as_of)}`;
  const totals = view.columns.map((c, i) => ({ c, t: view.totals[i] })).filter((x) => x.t);
  return (
    <section className="totals">
      <div className="big">
        {totals.map(({ c, t }) => (
          <div key={c.col_id} className={`total ${t!.complete ? "" : "incomplete"}`}>
            <span className="label">{c.name}</span>
            <span className="amount">{t!.currency ? money(t!.amount, t!.currency) : t!.amount}</span>
            {!t!.complete && <span className="flag">incomplete</span>}
          </div>
        ))}
        {totals.length === 0 && (
          <div className="hint">No totals yet: click a column's header and tick “Show total”, or add a calculated column with “+ Column”.</div>
        )}
      </div>
      <div className="detail">
        <div>
          {mode}
          {view.rates_used.length > 0 && ": "}
          {view.rates_used.map((l) => `1 ${l.from} = ${l.rate} ${l.to} (${SOURCE_LABEL[l.source]} ${l.date})`).join(" · ")}
        </div>
        {view.warnings.length > 0 && (
          <div className="warnings">
            {view.warnings.map((w) => (
              <div key={w} className="warning">
                {w}
              </div>
            ))}
            <button className="copy-link" onClick={() => void writeText(view.warnings.join("\n"))}>
              Copy {view.warnings.length > 1 ? "all" : ""}
            </button>
          </div>
        )}
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
  onAdd: (template: "blank" | "spending" | "file") => void;
}) {
  const [renaming, setRenaming] = useState<string | null>(null);
  const [name, setName] = useState("");
  // Where the new-sheet menu opens (fixed position: the tab bar scrolls and would clip it).
  const [adding, setAdding] = useState<{ left: number; bottom: number } | null>(null);
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
      <div className="add-wrap">
        <button
          className="add"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            setAdding((a) => (a ? null : { left: r.left, bottom: window.innerHeight - r.top + 4 }));
          }}
          title="New sheet"
        >
          +
        </button>
        {adding && (
          <>
          <div className="backdrop" onClick={() => setAdding(null)} />
          <div className="add-menu" style={adding}>
            <button
              onClick={() => {
                setAdding(null);
                props.onAdd("blank");
              }}
            >
              Blank sheet
              <small>Columns A–E; name and type them as you go</small>
            </button>
            <button
              onClick={() => {
                setAdding(null);
                props.onAdd("spending");
              }}
            >
              Spending list
              <small>Item, Link, Note, UAH, USD, Unit USD, Qty, Total USD</small>
            </button>
            <button
              onClick={() => {
                setAdding(null);
                props.onAdd("file");
              }}
            >
              From a file…
              <small>CSV or JSON; its headings become typed columns</small>
            </button>
          </div>
          </>
        )}
      </div>
      <span className="hint">
        Double-click a column header to edit it · + Column adds one · ⌘C/⌘X/⌘V · ⇧⌘C/⇧⌘V transposed · ⌘D fill down · ⌘-click opens a link
      </span>
    </nav>
  );
}
