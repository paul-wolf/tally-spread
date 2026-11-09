import { useCallback, useEffect, useRef, useState } from "react";
import { CompactSelection, type DataEditorRef, type GridSelection, type Rectangle } from "@glideapps/glide-data-grid";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { api, newId, type ExportFormat } from "./api";
import { ColumnEditor } from "./ColumnEditor";
import { AnalysisPanel } from "./AnalysisPanel";
import { FunctionsPanel } from "./FunctionsPanel";
import { RateCardPanel } from "./RateCardPanel";
import { ColumnJump } from "./ColumnJump";
import { RowDrawer } from "./RowDrawer";
import { Home } from "./Home";
import { GoToSheet } from "./GoToSheet";
import { money, parseTsv, projectStyle, shortDate, toTsv, transpose } from "./format";
import { SheetGrid, emptySelection, opsForBlock, opsForClear, positionBetween, selectedBlock } from "./SheetGrid";
import type { ColumnSpec, Op, RateSource, SheetView, Snapshot, TabInfo } from "./types";
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

/** macOS has an app menu (with ⌘W as Close Tab); Linux has none. */
const IS_MAC = /Mac/i.test(navigator.platform || navigator.userAgent);

export default function App() {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [sheetId, setSheetId] = useState<string | null>(null);
  const [selection, setSelection] = useState<GridSelection>(emptySelection);
  const [message, setMessage] = useState<{ text: string; error?: boolean } | null>(null);
  const [rateBusy, setRateBusy] = useState(false);
  const [editing, setEditing] = useState<Editing | null>(null);
  const [panel, setPanel] = useState<"rates" | "functions" | null>(null);
  // The analysis drawer and the columns it opened with.
  const [analyse, setAnalyse] = useState<string[] | null>(null);
  // ⌘J column finder, and ⌘I drawer showing the current row transposed.
  const [jump, setJump] = useState(false);
  // Home (every sheet in every project) in place of the grid, and ⌘K Go to Sheet.
  const [home, setHome] = useState(false);
  const [goto, setGoto] = useState(false);
  // Notices belong to one snapshot; × hides them until the next one brings new ones.
  const [noticesHidden, setNoticesHidden] = useState<Snapshot | null>(null);
  const homeRef = useRef(home);
  homeRef.current = home;
  const [rowDrawer, setRowDrawer] = useState(false);
  const gridRef = useRef<DataEditorRef>(null);
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

  /** Show a sheet (leaving Home); with no sheet left to show, Home takes its place. */
  const goTo = useCallback(
    async (p: Promise<Snapshot>) => {
      setSelection(emptySelection);
      const s = await run(p);
      if (s) setHome(!s.sheet);
    },
    [run],
  );

  const loadIndex = useCallback((archived: boolean) => api.sheetIndex(archived), []);
  const loadOpenable = useCallback(() => api.sheetIndex(false), []);

  /** ⇧⌘N: cells copied from Excel (or CSV text) become a new Unfiled sheet. */
  const newFromClipboard = useCallback(async () => {
    try {
      const text = await readText();
      await goTo(api.newSheetFromText(text ?? ""));
    } catch (e) {
      fail(e);
    }
  }, [goTo, fail]);

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
    void run(api.snapshot(null)).then((s) => {
      if (s && !s.sheet) setHome(true);
      return refreshRates(true);
    });
  }, [run, refreshRates]);

  // Pick up changes synced from the other machine.
  useEffect(() => {
    const t = setInterval(async () => {
      try {
        const s = await api.sync(sheetRef.current);
        if (s) show(s);
        // Sheets imported with `tally import` are shown even over Home.
        if (s?.focus && s.sheet) {
          setSelection(emptySelection);
          setHome(false);
        }
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

  /** Column IDs the selection touches: whole columns, a range, or the current cell. */
  const selectedColumns = useCallback((): string[] => {
    const v = snapRef.current?.sheet;
    if (!v) return [];
    const sel = selectionRef.current;
    let idx = sel.columns.toArray();
    if (!idx.length && sel.current) {
      const r = sel.current.range;
      idx = Array.from({ length: r.width }, (_, i) => r.x + i);
    }
    return idx.map((i) => v.columns[i]?.col_id).filter((x): x is string => !!x);
  }, []);
  const openAnalysis = useCallback(() => {
    setRowDrawer(false);
    setAnalyse(selectedColumns());
  }, [selectedColumns]);

  /** Put the cursor on one cell and scroll it into view. */
  const moveTo = useCallback((col: number, row: number, focus = true) => {
    setSelection({
      columns: CompactSelection.empty(),
      rows: CompactSelection.empty(),
      current: { cell: [col, row], range: { x: col, y: row, width: 1, height: 1 }, rangeStack: [] },
    });
    gridRef.current?.scrollTo(col, row, "both", 40, 40);
    if (focus) gridRef.current?.focus();
  }, []);

  /**
   * ⌘W: close whatever is in front. An open panel, editor or finder closes first (like
   * Escape); otherwise the shown sheet's tab closes and the sheet stays in its project.
   * Home is always there and does not close; the window never closes from ⌘W.
   */
  const closeFront = () => {
    if (editing || panel || jump || goto || analyse || rowDrawer) {
      setEditing(null);
      setPanel(null);
      setJump(false);
      setGoto(false);
      setAnalyse(null);
      setRowDrawer(false);
      return;
    }
    const s = snapRef.current;
    if (homeRef.current || !s?.sheet) return;
    const t = s.tabs.find((t) => t.sheet_id === sheetRef.current && t.project === s.project);
    if (t) void goTo(api.closeTab(t.project, t.sheet_id, sheetRef.current));
  };
  const closeFrontRef = useRef(closeFront);
  closeFrontRef.current = closeFront;

  // On macOS ⌘W is the File menu's Close Tab (set up in src-tauri), which sends this event.
  useEffect(() => {
    const off = listen("close-tab", () => closeFrontRef.current());
    return () => void off.then((f) => f());
  }, []);

  // Keyboard shortcuts that sit above the grid.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      // Elsewhere there is no app menu, so Ctrl+W is handled here.
      if (!IS_MAC && e.ctrlKey && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "w") {
        e.preventDefault();
        closeFrontRef.current();
        return;
      }
      if (!mod || editing || panel || jump || goto) return;
      const k = e.key.toLowerCase();
      // These work everywhere, Home and its search box included.
      if (k === "n" && !e.shiftKey) {
        // A new sheet in Unfiled, no questions; file it later from its tab.
        e.preventDefault();
        void goTo(api.newSheet("blank"));
        return;
      } else if (k === "n" && e.shiftKey) {
        e.preventDefault();
        void newFromClipboard();
        return;
      } else if (k === "k") {
        e.preventDefault();
        setGoto(true);
        return;
      } else if (k === "h" && e.shiftKey) {
        e.preventDefault();
        if (homeRef.current && snapRef.current?.sheet) setHome(false);
        else setHome(true);
        return;
      }
      // The rest act on the grid, which Home hides.
      if (isTyping(e) || homeRef.current) return;
      if (k === "j") {
        e.preventDefault();
        setJump(true);
      } else if (k === "i") {
        e.preventDefault();
        setAnalyse(null);
        setRowDrawer((o) => !o);
      } else if (k === "z") {
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
      } else if (e.shiftKey && k === "a") {
        e.preventDefault();
        openAnalysis();
      } else if (e.key === "PageDown" || e.key === "PageUp") {
        const tabs = snapRef.current?.tabs ?? [];
        const i = tabs.findIndex((t) => t.sheet_id === sheetRef.current);
        const next = tabs[(i + (e.key === "PageDown" ? 1 : -1) + tabs.length) % tabs.length];
        if (next && tabs.length > 1) {
          e.preventDefault();
          void goTo(api.openSheet(next.project, next.sheet_id));
        }
      }
    };
    // Capture phase, so the shortcuts work while the grid has focus.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [run, goTo, newFromClipboard, paste, copy, insertRowAbove, toggleSection, openAnalysis, editing, panel, jump, goto]);

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

  /** Excel, CSV or JSON as new sheets in Unfiled (one per worksheet). */
  const importFile = async () => {
    const path = await openDialog({
      multiple: false,
      filters: [{ name: "Excel, CSV or JSON", extensions: ["xlsx", "xlsm", "xlsb", "xls", "ods", "csv", "tsv", "txt", "json"] }],
    });
    if (typeof path !== "string") return;
    setMessage({ text: `Importing ${path.split(/[\\/]/).pop()}…` });
    try {
      const before = snap.tabs.length;
      setSelection(emptySelection);
      const s = await api.importFile(path);
      show(s);
      setHome(false);
      const v = s.sheet;
      const added = s.tabs.length - before;
      const file = "It is in Unfiled; right-click its tab to move it to a project.";
      if (v && added > 1) setMessage({ text: `Imported ${added} worksheets as sheets; showing “${v.name}”. ${file}` });
      else if (v) setMessage({ text: `Imported ${v.rows.length} rows and ${v.columns.length} columns into “${v.name}”. ${file}` });
    } catch (e) {
      fail(e);
    }
  };

  const editedColumn = editing?.mode === "edit" && view ? view.columns[editing.index] ?? null : null;

  return (
    <div className="app">
      <TopBar
        // On Home no sheet is shown: the sheet's tools, undo and export stand aside.
        snap={home ? { ...snap, sheet: null } : snap}
        rateBusy={rateBusy}
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
        onAnalyse={openAnalysis}
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
        {home || !view ? (
          <Home
            version={snap}
            projects={snap.projects}
            unfiled={snap.unfiled}
            load={loadIndex}
            onOpen={(e) => void goTo(api.openSheet(e.project, e.sheet_id))}
            onRestore={(e) => void goTo(api.restoreSheet(e.project, e.sheet_id))}
            onOpenProject={(d) => void goTo(api.openProject(d))}
            onNewSheet={(project) => void goTo(api.newSheet("blank", project))}
            onNewFromClipboard={() => void newFromClipboard()}
            onImport={() => void importFile()}
            onNewProject={(n) => void goTo(api.createProject(n))}
            onError={fail}
          />
        ) : (
          <SheetGrid
            view={view}
            apply={apply}
            selection={selection}
            setSelection={setSelection}
            onEditColumn={openColumnEditor}
            dark={dark}
            gridRef={gridRef}
          />
        )}
      </main>

      {view && !home && <CellNote view={view} selection={selection} />}
      {view && !home && <Totals view={view} />}

      <SheetTabs
        snap={snap}
        current={view && !home ? sheetId : null}
        home={home || !view}
        onHome={() => setHome(true)}
        onSelect={(t) => void goTo(api.openSheet(t.project, t.sheet_id))}
        onClose={(t) => void goTo(api.closeTab(t.project, t.sheet_id, sheetRef.current)).then(() => home && setHome(true))}
        onMove={(t, to, newProject) => void goTo(api.moveSheet(t.project, t.sheet_id, to, newProject))}
        onPin={(t, pinned) => void run(api.pinTab(t.project, t.sheet_id, pinned, sheetRef.current))}
        onApply={async (t, op) => {
          // Rename and archive act on the tab's own project, which may not be the shown one.
          if (t.project !== snapRef.current?.project) await run(api.openSheet(t.project, t.sheet_id));
          await apply(op);
        }}
        onAdd={async (template, inProject) => {
          if (template === "file") await importFile();
          else await goTo(api.newSheet(template, inProject));
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

      {analyse && view && (
        <AnalysisPanel
          key={view.sheet_id}
          view={view}
          initialCols={analyse}
          onClose={() => setAnalyse(null)}
          onSaved={(s) => {
            show(s);
            setAnalyse(null);
            setMessage({ text: `Saved the summary as the sheet “${s.sheet?.name ?? ""}”.` });
          }}
          onError={fail}
        />
      )}

      {goto && (
        <GoToSheet
          load={loadOpenable}
          onPick={(e) => {
            setGoto(false);
            void goTo(api.openSheet(e.project, e.sheet_id));
          }}
          onClose={() => setGoto(false)}
          onError={fail}
        />
      )}

      {jump && view && (
        <ColumnJump
          view={view}
          current={selection.current?.cell[0] ?? null}
          onPick={(col) => {
            setJump(false);
            // Same row as before; the first row if nothing was selected.
            moveTo(col, selection.current?.cell[1] ?? 0);
          }}
          onClose={() => {
            setJump(false);
            gridRef.current?.focus();
          }}
        />
      )}

      {rowDrawer && view && (
        <RowDrawer
          view={view}
          row={selection.current?.cell[1] ?? 0}
          col={selection.current?.cell[0] ?? null}
          apply={apply}
          onMove={(col, row) => moveTo(col, row, false)}
          onClose={() => {
            setRowDrawer(false);
            gridRef.current?.focus();
          }}
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

      {(message || (snap.notices.length > 0 && noticesHidden !== snap)) && (
        <div className={`toast ${message?.error ? "error" : ""}`}>
          <span className="toast-text">{message?.text ?? snap.notices[0]}</span>
          <button className="toast-btn" title="Copy" onClick={() => void writeText(message?.text ?? snap.notices[0])}>
            Copy
          </button>
          <button
            className="toast-btn"
            title="Dismiss"
            onClick={() => {
              setMessage(null);
              setNoticesHidden(snap);
            }}
          >
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
  onAnalyse: () => void;
}) {
  const { snap } = props;
  const view = snap.sheet;

  const locked = view?.rates.mode === "locked" ? view.rates.date : null;
  const firstSource = view?.effective_sources[0] ?? "ecb";

  return (
    <header className="topbar">
      {view && (
        <div className="group">
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
      )}

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
          <button onClick={props.onAnalyse} title="Charts and figures for the selected columns (⇧⌘A)">
            Analyse
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

      {view && (
        <div className="group right">
          <button onClick={props.onUndo} disabled={!snap.can_undo} title="Undo (⌘Z)">
            Undo
          </button>
          <button onClick={props.onRedo} disabled={!snap.can_redo} title="Redo (⇧⌘Z)">
            Redo
          </button>
          <ExportMenu disabled={!view} onExport={props.onExport} />
        </div>
      )}

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

/** Where a tab menu opens: fixed position above the tab bar (which scrolls and would clip it). */
type MenuAt = { left: number; bottom: number };

/** Tab menus are 300px wide (.add-menu). */
const MENU_WIDTH = 300;

/** Above `el`, starting at its left edge but always fully inside the window. */
const menuAt = (el: Element): MenuAt => {
  const r = el.getBoundingClientRect();
  const left = Math.max(8, Math.min(r.left, window.innerWidth - MENU_WIDTH - 8));
  return { left, bottom: window.innerHeight - r.top + 4 };
};

/**
 * Open sheets from any project. A tab in a project shows the project's name after the
 * sheet's, in muted text; Unfiled sheets show nothing extra. Move to project… on the
 * right-click menu files the sheet elsewhere. × (or ⌘W) closes the tab only: the
 * sheet stays in its project.
 *
 * Home and + stay put; only the sheet tabs between them scroll, so + is always in reach.
 */
function SheetTabs(props: {
  snap: Snapshot;
  current: string | null;
  /** Home is shown in place of a sheet. */
  home: boolean;
  onHome: () => void;
  onSelect: (t: TabInfo) => void;
  onClose: (t: TabInfo) => void;
  onMove: (t: TabInfo, to: string | null, newProject: string | null) => void;
  onPin: (t: TabInfo, pinned: boolean) => void;
  onApply: (t: TabInfo, op: Op) => void;
  onAdd: (template: "blank" | "spending" | "file", inProject: string | null) => void;
}) {
  const { snap } = props;
  const [renaming, setRenaming] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [adding, setAdding] = useState<MenuAt | null>(null);
  const [menu, setMenu] = useState<{ kind: "tab" | "move"; tab: TabInfo; at: MenuAt } | null>(null);
  const [newProject, setNewProject] = useState<string | null>(null);
  const key = (t: TabInfo) => `${t.project}/${t.sheet_id}`;
  const shown = snap.tabs.find((t) => t.sheet_id === props.current && t.project === snap.project) ?? null;
  const close = () => {
    setMenu(null);
    setAdding(null);
    setNewProject(null);
  };
  const rename = (t: TabInfo) => {
    close();
    setRenaming(key(t));
    setName(t.name);
  };

  // The strip of sheet tabs scrolls sideways: a mouse wheel scrolls it too, the shown
  // tab is kept in view, and faded edges say there are more tabs off to that side.
  const strip = useRef<HTMLDivElement>(null);
  const [more, setMore] = useState({ left: false, right: false });
  const measure = useCallback(() => {
    const el = strip.current;
    if (!el) return;
    const left = el.scrollLeft > 1;
    const right = el.scrollLeft + el.clientWidth < el.scrollWidth - 1;
    setMore((m) => (m.left === left && m.right === right ? m : { left, right }));
  }, []);
  useEffect(() => {
    const el = strip.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (Math.abs(e.deltaY) > Math.abs(e.deltaX) && el.scrollWidth > el.clientWidth) {
        e.preventDefault();
        el.scrollLeft += e.deltaY;
      }
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => {
      el.removeEventListener("wheel", onWheel);
      ro.disconnect();
    };
  }, [measure]);
  const shownKey = !props.home && shown ? key(shown) : null;
  useEffect(() => {
    strip.current?.querySelector(".tab.active")?.scrollIntoView({ block: "nearest", inline: "nearest" });
    measure();
  }, [shownKey, snap.tabs.length, measure]);

  return (
    <nav className="tabs">
      <div
        className={`tab home-tab ${props.home ? "active" : ""}`}
        onClick={props.onHome}
        title="Home: every sheet in every project (⇧⌘H) · ⌘K to go to a sheet by name"
      >
        <span aria-hidden>⌂</span>
        <span>Home</span>
      </div>
      <div
        ref={strip}
        className={`tab-strip ${more.left ? "more-left" : ""} ${more.right ? "more-right" : ""}`}
        onScroll={measure}
      >
      {snap.tabs.map((t) => {
        const active = !props.home && shown !== null && key(t) === key(shown);
        return (
          <div
            key={key(t)}
            className={`tab ${active ? "active" : ""} ${t.unfiled ? "" : "in-project"}`}
            style={t.unfiled ? undefined : projectStyle(snap.projects, t.project)}
            onClick={() => !active && props.onSelect(t)}
            onDoubleClick={() => rename(t)}
            onContextMenu={(e) => {
              e.preventDefault();
              setMenu({ kind: "tab", tab: t, at: menuAt(e.currentTarget) });
            }}
            onAuxClick={(e) => e.button === 1 && props.onClose(t)}
            title={`${t.name} · ${t.unfiled ? "Unfiled (not in a project)" : `in ${t.project_name}`}\nDouble-click to rename · right-click to move to a project, pin or archive`}
          >
            {renaming === key(t) ? (
              <input
                autoFocus
                value={name}
                onChange={(e) => setName(e.target.value)}
                onBlur={() => setRenaming(null)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    if (name.trim() && name.trim() !== t.name) {
                      props.onApply(t, { op: "rename_sheet", sheet_id: t.sheet_id, name: name.trim() });
                    }
                    setRenaming(null);
                  } else if (e.key === "Escape") setRenaming(null);
                }}
              />
            ) : (
              <>
                {t.pinned && (
                  <span className="pin" title="Pinned: never closed to make room">
                    ◆
                  </span>
                )}
                <span>{t.name}</span>
                {!t.unfiled && <span className="tab-project">{t.project_name}</span>}
                <button
                  className="close"
                  title="Close tab, ⌘W (the sheet stays in its project)"
                  onClick={(e) => {
                    e.stopPropagation();
                    props.onClose(t);
                  }}
                >
                  ×
                </button>
              </>
            )}
          </div>
        );
      })}
      </div>

      {menu?.kind === "tab" && (
        <>
          <div className="backdrop" onClick={close} />
          <div className="add-menu" style={menu.at}>
            <button onClick={() => rename(menu.tab)}>Rename</button>
            <button onClick={() => setMenu({ ...menu, kind: "move" })}>
              Move to project…
              <small>Now in {menu.tab.unfiled ? "Unfiled" : menu.tab.project_name}</small>
            </button>
            <button
              onClick={() => {
                close();
                props.onPin(menu.tab, !menu.tab.pinned);
              }}
            >
              {menu.tab.pinned ? "Unpin" : "Pin"}
              <small>{menu.tab.pinned ? "Can close again when tabs run over" : "Never closed to make room for new tabs"}</small>
            </button>
            <button
              onClick={() => {
                close();
                props.onApply(menu.tab, { op: "set_sheet_archived", sheet_id: menu.tab.sheet_id, archived: true });
              }}
            >
              Archive
              <small>Hidden but kept; ⌘Z brings it back</small>
            </button>
            <button
              onClick={() => {
                close();
                props.onClose(menu.tab);
              }}
            >
              <span>
                Close tab <kbd>⌘W</kbd>
              </span>
            </button>
          </div>
        </>
      )}

      {menu?.kind === "move" && (
        <>
          <div className="backdrop" onClick={close} />
          <div className="add-menu move-menu" style={menu.at}>
            <div className="menu-title">Move “{menu.tab.name}” to</div>
            {snap.projects
              .filter((p) => p.dir_name !== menu.tab.project)
              .map((p) => (
                <button
                  key={p.dir_name}
                  onClick={() => {
                    close();
                    props.onMove(menu.tab, p.dir_name, null);
                  }}
                >
                  {p.name}
                </button>
              ))}
            {!menu.tab.unfiled && (
              <button
                onClick={() => {
                  close();
                  props.onMove(menu.tab, snap.unfiled, null);
                }}
              >
                Unfiled
                <small>Not in any project</small>
              </button>
            )}
            {newProject === null ? (
              <button onClick={() => setNewProject("")}>
                New project…
                <small>Creates it and moves the sheet in</small>
              </button>
            ) : (
              <input
                autoFocus
                className="menu-input"
                placeholder="Project name, then Enter"
                value={newProject}
                onChange={(e) => setNewProject(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && newProject.trim()) {
                    const t = menu.tab;
                    const n = newProject.trim();
                    close();
                    props.onMove(t, null, n);
                  } else if (e.key === "Escape") close();
                }}
              />
            )}
          </div>
        </>
      )}

      <div className="add-wrap">
        <button
          className={`add ${adding ? "open" : ""}`}
          onClick={(e) => {
            const at = menuAt(e.currentTarget);
            setAdding((a) => (a ? null : at));
          }}
          title="New sheet (⌘N for a blank one in Unfiled)"
        >
          +
        </button>
        {adding && (
          <>
            <div className="backdrop" onClick={close} />
            <div className="add-menu" style={adding}>
              <button
                onClick={() => {
                  close();
                  props.onAdd("blank", null);
                }}
              >
                <span>
                  Blank sheet <kbd>⌘N</kbd>
                </span>
                <small>Columns A–E, in Unfiled; name and type them as you go</small>
              </button>
              <button
                onClick={() => {
                  close();
                  props.onAdd("spending", null);
                }}
              >
                Spending list
                <small>Item, Link, Note, UAH, USD, Unit USD, Qty, Total USD, in Unfiled</small>
              </button>
              <button
                onClick={() => {
                  close();
                  props.onAdd("file", null);
                }}
              >
                From a file…
                <small>Excel, CSV or JSON, into Unfiled; headings become typed columns</small>
              </button>
              {shown && !shown.unfiled && (
                <button
                  onClick={() => {
                    close();
                    props.onAdd("blank", shown.project);
                  }}
                >
                  Blank sheet in {shown.project_name}
                  <small>Uses {shown.project_name}'s settings</small>
                </button>
              )}
            </div>
          </>
        )}
      </div>
      <span className="hint">
        ⌘N new sheet · ⌘W close tab · ⌘K go to sheet · right-click a tab to move or pin it · Double-click a column header to edit it · ⌘C/⌘X/⌘V · ⇧⌘C/⇧⌘V transposed · ⌘D fill down · ⌘-click opens a link
      </span>
    </nav>
  );
}
