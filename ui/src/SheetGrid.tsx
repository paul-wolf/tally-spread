import { useCallback, useMemo, useRef, useState } from "react";
import {
  CompactSelection,
  DataEditor,
  GridCellKind,
  GridColumnIcon,
  type EditableGridCell,
  type GridCell,
  type GridColumn,
  type GridSelection,
  type Item,
  type Rectangle,
  type Theme,
} from "@glideapps/glide-data-grid";
import { openUrl } from "@tauri-apps/plugin-opener";
import { newId } from "./api";
import { group, padMinor } from "./format";
import type { ColumnView, Op, SheetView, Snapshot } from "./types";

/** Empty rows always shown below the data, ready to type or paste into. */
export const BLANK_ROWS = 40;

export const emptySelection: GridSelection = {
  columns: CompactSelection.empty(),
  rows: CompactSelection.empty(),
};

const isInput = (c: ColumnView) => c.type !== "calc";
const isNumeric = (c: ColumnView) => c.type === "number" || c.type === "money" || c.type === "calc";

/** A position between two neighbours (or past the end), for inserting rows or columns. */
export function positionBetween(before: number | undefined, after: number | undefined): number {
  if (before === undefined && after === undefined) return 1;
  if (before === undefined) return after! - 1;
  if (after === undefined) return Math.floor(before) + 1;
  return (before + after) / 2;
}

/** Raw (unformatted) value of a cell, as copied to the clipboard. */
export function rawValue(view: SheetView, col: number, row: number): string {
  const cell = view.rows[row]?.cells[col];
  return cell?.raw ?? cell?.value ?? "";
}

/** The selected block as a grid of raw values: whole rows, whole columns, or a range. */
export function selectedBlock(view: SheetView, sel: GridSelection): { origin: Item; values: string[][] } | null {
  const ncols = view.columns.length;
  const rows = sel.rows.toArray();
  const cols = sel.columns.toArray();
  let xs: number[];
  let ys: number[];
  if (rows.length) {
    ys = rows.filter((y) => y < view.rows.length);
    xs = [...Array(ncols).keys()];
  } else if (cols.length) {
    xs = cols;
    ys = [...Array(view.rows.length).keys()];
  } else if (sel.current) {
    const r = sel.current.range;
    xs = Array.from({ length: r.width }, (_, i) => r.x + i);
    ys = Array.from({ length: r.height }, (_, i) => r.y + i).filter((y) => y < view.rows.length);
  } else {
    return null;
  }
  if (!xs.length || !ys.length) return null;
  return { origin: [xs[0], ys[0]], values: ys.map((y) => xs.map((x) => rawValue(view, x, y))) };
}

/** Operations that clear the editable cells of the selection. */
export function opsForClear(view: SheetView, sel: GridSelection): Op[] {
  const block = selectedBlock(view, sel);
  if (!block) return [];
  const ops: Op[] = [];
  const [x0, y0] = block.origin;
  block.values.forEach((line, i) => {
    const row = view.rows[y0 + i];
    line.forEach((_, j) => {
      const col = view.columns[x0 + j];
      if (row && col && isInput(col) && row.cells[x0 + j]?.raw != null) {
        ops.push({ op: "set_cell", sheet_id: view.sheet_id, row_id: row.row_id, col_id: col.col_id, value: null });
      }
    });
  });
  return ops;
}

/** Roughly what the engine accepts as an amount: "1 234,56", "£12.50", "$ 71.50", "19,90 EUR", "(15)". */
export function looksLikeAmount(v: string): boolean {
  const t = v
    .replace(/[\s  '’]/g, "")
    .replace(/^\(|\)$/g, "")
    .replace(/^-|-$/g, "")
    .replace(/^(грн\.?|[₴£€$¥]|[A-Za-z]{3})/i, "")
    .replace(/(грн\.?|[₴£€$¥]|zł|[A-Za-z]{3})$/i, "")
    .replace(/^-/, "");
  return /^(\d[\d.,]*|[.,]\d+)$/.test(t);
}

const SYMBOL_CURRENCY: Record<string, string> = { "₴": "UAH", "£": "GBP", "€": "EUR", $: "USD", "¥": "JPY" };

/** The currency a typed amount names, if any ("$25" -> USD, "3 250 UAH" -> UAH). */
function namedCurrency(v: string): string | null {
  const t = v.trim();
  for (const [sym, code] of Object.entries(SYMBOL_CURRENCY)) if (t.includes(sym)) return code;
  if (/грн/i.test(t)) return "UAH";
  const m = t.match(/^([A-Za-z]{3})|([A-Za-z]{3})$/);
  return m ? (m[1] ?? m[2]).toUpperCase() : null;
}

/** Is this value acceptable for the column? Empty is always fine (it clears). */
export function acceptable(col: ColumnView, v: string): boolean {
  if (v.trim() === "") return true;
  if (col.type === "number") return looksLikeAmount(v) && namedCurrency(v) === null;
  if (col.type === "money") {
    const named = namedCurrency(v);
    return looksLikeAmount(v) && (named === null || named === col.currency);
  }
  return col.type !== "calc";
}

/**
 * Operations that write a block of values starting at a cell, adding rows as
 * needed. Calculated columns are stepped over (their pasted values are dropped),
 * a first line that looks like column headings is skipped, and values a column
 * cannot take are left out, so one stray cell does not stop the whole paste.
 */
export function opsForBlock(
  view: SheetView,
  target: Item,
  values: readonly (readonly string[])[],
): { ops: Op[]; skippedCells: number; skippedHeader: boolean } {
  const ops: Op[] = [];
  let nextPos = Math.max(0, ...view.rows.map((r) => r.position)) + 1;
  const [col0, row0] = target;
  let skippedCells = 0;
  const colFor = (j: number) => view.columns[col0 + j];

  const first = values[0] ?? [];
  const skippedHeader =
    values.length > 1 &&
    first.some((v, j) => {
      const c = colFor(j);
      return !!c && isInput(c) && isNumeric(c) && v.trim() !== "" && !acceptable(c, v);
    });
  const lines = skippedHeader ? values.slice(1) : values;

  let y = row0;
  for (const line of lines) {
    if (line.every((v) => v.trim() === "")) {
      y++;
      continue;
    }
    const existing = view.rows[y];
    let rowId = existing?.row_id;
    if (!rowId) {
      rowId = newId();
      ops.push({ op: "insert_row", sheet_id: view.sheet_id, row_id: rowId, position: nextPos++ });
    }
    line.forEach((value, j) => {
      const col = colFor(j);
      if (!col || !isInput(col)) return;
      if (!existing && value.trim() === "") return;
      if (existing?.kind !== "section" && !acceptable(col, value)) {
        skippedCells++;
        return;
      }
      ops.push({
        op: "set_cell",
        sheet_id: view.sheet_id,
        row_id: rowId!,
        col_id: col.col_id,
        value: value.trim() === "" ? null : value,
      });
    });
    y++;
  }
  return { ops, skippedCells, skippedHeader };
}

const darkTheme: Partial<Theme> = {
  accentColor: "#5b9cf5",
  accentLight: "rgba(91, 156, 245, 0.18)",
  textDark: "#e6e6e6",
  textMedium: "#a8a8a8",
  textLight: "#7a7a7a",
  textBubble: "#e6e6e6",
  bgIconHeader: "#a8a8a8",
  fgIconHeader: "#1c1c1e",
  textHeader: "#c8c8c8",
  textGroupHeader: "#8a8a90",
  textHeaderSelected: "#1c1c1e",
  bgCell: "#1c1c1e",
  bgCellMedium: "#232326",
  bgHeader: "#232326",
  bgHeaderHasFocus: "#2c2c30",
  bgHeaderHovered: "#2c2c30",
  bgBubble: "#2c2c30",
  bgBubbleSelected: "#3a3a40",
  bgSearchResult: "#4a4020",
  borderColor: "rgba(255, 255, 255, 0.10)",
  drilldownBorder: "rgba(255, 255, 255, 0.2)",
  linkColor: "#7fb2ff",
};

const baseTheme: Partial<Theme> = {
  fontFamily: "-apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', sans-serif",
  baseFontStyle: "13px",
  headerFontStyle: "600 12px",
  editorFontSize: "13px",
  textGroupHeader: "#8a8a8a",
};

const ICON: Record<ColumnView["type"], GridColumnIcon> = {
  text: GridColumnIcon.HeaderString,
  link: GridColumnIcon.HeaderUri,
  number: GridColumnIcon.HeaderNumber,
  money: GridColumnIcon.HeaderNumber,
  calc: GridColumnIcon.HeaderMath,
};

function defaultWidth(c: ColumnView, index: number): number {
  if (index === 0 && c.type === "text") return 260;
  switch (c.type) {
    case "text":
      return 160;
    case "link":
      return 200;
    case "number":
      return 70;
    default:
      return 116;
  }
}

function loadWidths(): Record<string, number> {
  try {
    return JSON.parse(localStorage.getItem("tally.widths") ?? "{}");
  } catch {
    return {};
  }
}

interface Props {
  view: SheetView;
  apply: (op: Op) => Promise<Snapshot | undefined>;
  selection: GridSelection;
  setSelection: (s: GridSelection) => void;
  /** Open the column editor for column `index`, or for a new column at the end. */
  onEditColumn: (index: number | "new", at: Rectangle) => void;
  dark: boolean;
}

export function SheetGrid({ view, apply, selection, setSelection, onEditColumn, dark }: Props) {
  const [widths, setWidths] = useState<Record<string, number>>(loadWidths);
  // The latest view and selection, readable from callbacks that run after an await.
  const viewRef = useRef(view);
  viewRef.current = view;
  const selectionRef = useRef(selection);
  selectionRef.current = selection;

  const columns: GridColumn[] = useMemo(
    () =>
      view.columns.map((c, i) => ({
        id: c.col_id,
        title: c.name,
        group: c.var ? `${c.col_id} · ${c.var}` : c.col_id,
        width: widths[c.col_id] ?? defaultWidth(c, i),
        icon: ICON[c.type],
        hasMenu: true,
        themeOverride: c.type === "calc" ? { bgHeader: dark ? "#26262c" : "#eef1f7" } : undefined,
      })),
    [view, widths, dark],
  );

  // The last row is the pinned Total row.
  const totalRow = view.rows.length + BLANK_ROWS;

  const getCellContent = useCallback(
    ([col, row]: Item): GridCell => {
      const c = view.columns[col];
      const r = view.rows[row];
      const empty: GridCell = { kind: GridCellKind.Text, data: "", displayData: "", allowOverlay: false, readonly: true };
      if (!c) return empty;
      if (row === totalRow) {
        // The Total row, pinned to the bottom: sheet totals under each column.
        const t = view.totals[col];
        if (t) {
          return {
            kind: GridCellKind.Text,
            data: t.amount,
            displayData: t.complete ? group(t.amount) : `${group(t.amount)} ⚠`,
            allowOverlay: false,
            readonly: true,
            contentAlign: "right",
            themeOverride: { baseFontStyle: "700 13px", ...(t.complete ? {} : { textDark: "#b25c00" }) },
          };
        }
        return col === 0
          ? { kind: GridCellKind.Text, data: "Total", displayData: "Total", allowOverlay: false, readonly: true, themeOverride: { baseFontStyle: "700 13px" } }
          : empty;
      }
      if (!r) {
        // A blank row below the data: editable, becomes a real row when typed into.
        return isInput(c) ? { kind: GridCellKind.Text, data: "", displayData: "", allowOverlay: true } : empty;
      }
      const cell = r.cells[col];
      const warnBg = dark ? "#4a3a12" : "#fff1c7";
      const bold = { baseFontStyle: "600 13px" };

      if (r.kind === "section") {
        if (cell.value != null) {
          return {
            kind: GridCellKind.Text,
            data: cell.value,
            displayData: group(cell.value),
            allowOverlay: false,
            readonly: true,
            contentAlign: "right",
            themeOverride: bold,
          };
        }
        if (!isInput(c)) return empty;
        return { kind: GridCellKind.Text, data: cell.raw ?? "", displayData: cell.raw ?? "", allowOverlay: true, themeOverride: bold };
      }

      if (isInput(c)) {
        const raw = cell.raw ?? "";
        let display = raw;
        if (c.type === "money") display = group(padMinor(raw, c.currency));
        else if (c.type === "number") display = group(raw);
        else if (c.type === "link") display = raw.replace(/^https?:\/\/(www\.)?/, "");
        return {
          kind: GridCellKind.Text,
          data: raw,
          displayData: display,
          allowOverlay: true,
          contentAlign: isNumeric(c) ? "right" : "left",
          themeOverride: {
            ...(c.type === "link" ? { textDark: dark ? "#7fb2ff" : "#2f5fc4" } : {}),
            ...(cell.warning ? { bgCell: warnBg } : {}),
          },
        };
      }

      // Calculated.
      return {
        kind: GridCellKind.Text,
        data: cell.value ?? "",
        displayData: cell.error ? "!" : group(cell.value),
        allowOverlay: false,
        readonly: true,
        contentAlign: "right",
        themeOverride: cell.error
          ? { textDark: "#d9534f" }
          : { textDark: dark ? "#c8c8c8" : "#333", ...(cell.warning ? { bgCell: warnBg } : {}) },
      };
    },
    [view, dark, totalRow],
  );

  const getRowThemeOverride = useCallback(
    (row: number) =>
      row === totalRow
        ? { bgCell: dark ? "#2a2a2e" : "#ecece6", bgCellMedium: dark ? "#2a2a2e" : "#ecece6" }
        : view.rows[row]?.kind === "section" ? { bgCell: dark ? "#26262a" : "#f1f1ec", bgCellMedium: dark ? "#26262a" : "#f1f1ec" } : undefined,
    [view, dark, totalRow],
  );

  const onCellsEdited = useCallback(
    (edits: readonly { location: Item; value: EditableGridCell }[]) => {
      const v = viewRef.current;
      const ops: Op[] = [];
      // Blank rows typed into become new rows, in the order they appear.
      const newRows = new Map<number, string>();
      let nextPos = Math.max(0, ...v.rows.map((r) => r.position)) + 1;
      const sorted = [...edits].sort((a, b) => a.location[1] - b.location[1]);
      for (const { location, value } of sorted) {
        if (location[1] >= v.rows.length + BLANK_ROWS) continue; // the Total row
        const col = v.columns[location[0]];
        if (!col || !isInput(col)) continue;
        const data = "data" in value && typeof value.data === "string" ? value.data : "";
        const row = v.rows[location[1]];
        let rowId: string | undefined = row?.row_id;
        if (!row) {
          if (data.trim() === "") continue;
          rowId = newRows.get(location[1]);
          if (!rowId) {
            rowId = newId();
            newRows.set(location[1], rowId);
            ops.push({ op: "insert_row", sheet_id: v.sheet_id, row_id: rowId, position: nextPos++ });
          }
        } else if (data === (row.cells[location[0]]?.raw ?? "")) {
          continue;
        }
        ops.push({
          op: "set_cell",
          sheet_id: v.sheet_id,
          row_id: rowId!,
          col_id: col.col_id,
          value: data.trim() === "" ? null : data,
        });
      }
      if (!ops.length) return true;
      void apply(ops.length === 1 ? ops[0] : { op: "batch", ops }).then((snap) => {
        // New rows go straight after the data. If a row was started further down,
        // move the cursor up to it so Tab carries on along the same row.
        const firstNew = Math.min(...newRows.keys());
        if (!snap?.sheet || !Number.isFinite(firstNew)) return;
        const landed = v.rows.length;
        const cur = selectionRef.current.current;
        if (cur && cur.cell[1] === firstNew && firstNew !== landed) {
          const x = cur.cell[0];
          setSelection({
            ...emptySelection,
            current: { cell: [x, landed], range: { x, y: landed, width: 1, height: 1 }, rangeStack: [] },
          });
        }
      });
      return true;
    },
    [apply, setSelection],
  );

  const onDelete = useCallback(
    (sel: GridSelection) => {
      const v = viewRef.current;
      const rows = sel.rows.toArray().filter((i) => v.rows[i]);
      if (rows.length) {
        const ops: Op[] = rows.map((i) => ({
          op: "set_row_deleted",
          sheet_id: v.sheet_id,
          row_id: v.rows[i].row_id,
          deleted: true,
        }));
        void apply({ op: "batch", ops });
        setSelection(emptySelection);
        return false;
      }
      const ops = opsForClear(v, sel);
      if (ops.length) void apply(ops.length === 1 ? ops[0] : { op: "batch", ops });
      return false;
    },
    [apply, setSelection],
  );

  const onCellClicked = useCallback((cell: Item, e: { metaKey: boolean; ctrlKey: boolean }) => {
    const v = viewRef.current;
    const col = v.columns[cell[0]];
    const url = col?.type === "link" ? v.rows[cell[1]]?.cells[cell[0]]?.raw : null;
    if (url && (e.metaKey || e.ctrlKey)) void openUrl(/^[a-z]+:\/\//i.test(url) ? url : `https://${url}`);
  }, []);

  const onColumnMoved = useCallback(
    (from: number, to: number) => {
      const v = viewRef.current;
      const col = v.columns[from];
      if (!col || from === to) return;
      const others = v.columns.filter((_, i) => i !== from);
      const position = positionBetween(others[to - 1]?.position, others[to]?.position);
      void apply({ op: "move_column", sheet_id: v.sheet_id, col_id: col.col_id, position });
    },
    [apply],
  );

  const onColumnResize = useCallback((column: GridColumn, size: number) => {
    setWidths((w) => {
      const next = { ...w, [column.id!]: size };
      try {
        localStorage.setItem("tally.widths", JSON.stringify(next));
      } catch {
        /* widths are a convenience */
      }
      return next;
    });
  }, []);

  return (
    <DataEditor
      columns={columns}
      rows={view.rows.length + BLANK_ROWS + 1}
      freezeTrailingRows={1}
      getCellContent={getCellContent}
      getCellsForSelection={true}
      getRowThemeOverride={getRowThemeOverride}
      onCellsEdited={onCellsEdited}
      onPaste={false}
      onDelete={onDelete}
      onCellClicked={onCellClicked}
      onHeaderMenuClick={(col, bounds) => onEditColumn(col, bounds)}
      onHeaderClicked={(col, e) => {
        // Double-click a header to edit the column; a single click selects it.
        if (e.isDoubleClick) onEditColumn(col, e.bounds);
      }}
      onHeaderContextMenu={(col, e) => {
        e.preventDefault();
        onEditColumn(col, e.bounds);
      }}
      onColumnMoved={onColumnMoved}
      onColumnResize={onColumnResize}
      rowMarkers="both"
      smoothScrollX
      smoothScrollY
      gridSelection={selection}
      onGridSelectionChange={setSelection}
      // Copy, cut and paste go through the app (see App.tsx), which reads and
      // writes the system clipboard directly and turns pastes into one undoable step.
      keybindings={{ downFill: true, rightFill: true, search: true, copy: false, paste: false, cut: false }}
      theme={dark ? { ...baseTheme, ...darkTheme } : baseTheme}
      width="100%"
      height="100%"
      rowHeight={30}
      headerHeight={32}
      groupHeaderHeight={20}
    />
  );
}
