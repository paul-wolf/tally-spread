import { useCallback, useMemo, useRef } from "react";
import {
  CompactSelection,
  DataEditor,
  GridCellKind,
  type EditableGridCell,
  type GridCell,
  type GridColumn,
  type GridSelection,
  type Item,
  type Theme,
} from "@glideapps/glide-data-grid";
import { openUrl } from "@tauri-apps/plugin-opener";
import { newId } from "./api";
import { group, padMinor } from "./format";
import type { Column, CostListView, Op, Snapshot } from "./types";

type ColDef = GridColumn & { id: string; edit?: Column; numeric?: boolean };

/** Empty rows always shown below the data, ready to type or paste into. */
export const BLANK_ROWS = 40;

const BASE_COLUMNS: ColDef[] = [
  { id: "item", title: "Item", width: 280, edit: "item" },
  { id: "url", title: "Link", width: 220, edit: "url" },
  { id: "qty", title: "Qty", width: 64, edit: "qty", numeric: true },
  { id: "unit_price", title: "Unit price", width: 116, edit: "unit_price", numeric: true },
  { id: "currency", title: "Cur", width: 58, edit: "currency" },
  { id: "line_total", title: "Line total", width: 120, numeric: true },
];

interface Props {
  view: CostListView;
  apply: (op: Op) => Promise<Snapshot | undefined>;
  selection: GridSelection;
  setSelection: (s: GridSelection) => void;
  dark: boolean;
}

export const emptySelection: GridSelection = {
  columns: CompactSelection.empty(),
  rows: CompactSelection.empty(),
};

export function columnsFor(view: CostListView): ColDef[] {
  return [
    ...BASE_COLUMNS,
    ...view.currencies.map((c, i) => ({ id: `conv_${i}`, title: `Total ${c}`, width: 124, numeric: true })),
  ];
}

/** Raw (unformatted) value of a cell, as copied to the clipboard. */
export function rawValue(view: CostListView, col: number, row: number): string {
  const r = view.rows[row];
  if (!r) return "";
  const def = columnsFor(view)[col];
  if (!def) return "";
  if (def.edit) return r[def.edit] ?? "";
  if (def.id === "line_total") return r.line_total ?? "";
  const i = Number(def.id.slice(5));
  return r.converted[i] ?? "";
}

/** The selected block as a grid of raw values: whole rows, whole columns, or a range. */
export function selectedBlock(view: CostListView, sel: GridSelection): { origin: Item; values: string[][] } | null {
  const ncols = columnsFor(view).length;
  const rows = sel.rows.toArray();
  const cols = sel.columns.toArray();
  let xs: number[];
  let ys: number[];
  if (rows.length) {
    ys = rows;
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
export function opsForClear(view: CostListView, sel: GridSelection): Op[] {
  const cols = columnsFor(view);
  const block = selectedBlock(view, sel);
  if (!block) return [];
  const ops: Op[] = [];
  const [x0, y0] = block.origin;
  block.values.forEach((line, i) => {
    const row = view.rows[y0 + i];
    line.forEach((_, j) => {
      const def = cols[x0 + j];
      if (row && def?.edit && row[def.edit] != null) {
        ops.push({ op: "set_cell", sheet_id: view.sheet_id, row_id: row.row_id, column: def.edit, value: null });
      }
    });
  });
  return ops;
}

/** Roughly what the engine accepts as an amount: "1 234,56", "£12.50", "19,90 EUR", "(15)". */
export function looksLikeAmount(v: string): boolean {
  const t = v
    .replace(/[\s\u00a0\u202f'’]/g, "")
    .replace(/^\(|\)$/g, "")
    .replace(/^-|-$/g, "")
    .replace(/^(грн\.?|[₴£€$¥]|[A-Za-z]{3})/i, "")
    .replace(/(грн\.?|[₴£€$¥]|zł|[A-Za-z]{3})$/i, "")
    .replace(/^-/, "");
  return /^(\d[\d.,]*|[.,]\d+)$/.test(t);
}

const looksLikeCurrency = (v: string) => /^\s*[A-Za-z]{3}\s*$/.test(v);

/** Is this value acceptable for the column? Empty is always fine (it clears). */
function acceptable(column: Column, v: string): boolean {
  if (v.trim() === "") return true;
  if (column === "qty" || column === "unit_price") return looksLikeAmount(v);
  if (column === "currency") return looksLikeCurrency(v);
  return true;
}

/**
 * Operations that write a block of values starting at a cell, adding rows as
 * needed. A first line that looks like column headings is skipped, and values a
 * column cannot take (text in Qty, say) are left out, so one stray cell does not
 * stop the whole paste.
 */
export function opsForBlock(
  view: CostListView,
  target: Item,
  values: readonly (readonly string[])[],
): { ops: Op[]; skippedCells: number; skippedHeader: boolean } {
  const cols = columnsFor(view);
  const ops: Op[] = [];
  let nextPos = Math.max(0, ...view.rows.map((r) => r.position)) + 1;
  const [col0, row0] = target;
  let skippedCells = 0;

  const colFor = (j: number) => cols[col0 + j];
  const first = values[0] ?? [];
  const skippedHeader =
    values.length > 1 &&
    first.some((v, j) => {
      const c = colFor(j)?.edit;
      return !!c && v.trim() !== "" && !acceptable(c, v);
    });
  const lines = skippedHeader ? values.slice(1) : values;

  let y = row0;
  for (const line of lines) {
    if (line.every((v) => v.trim() === "")) {
      y++;
      continue;
    }
    let rowId = view.rows[y]?.row_id;
    if (!rowId) {
      rowId = newId();
      ops.push({ op: "insert_row", sheet_id: view.sheet_id, row_id: rowId, position: nextPos++ });
    }
    const isNew = !view.rows[y];
    line.forEach((value, j) => {
      const column = colFor(j)?.edit;
      if (!column) return;
      // Empty cells clear existing values, but are simply skipped on new rows (so an
      // empty currency cell does not undo the currency in "$25").
      if (isNew && value.trim() === "") return;
      if (!acceptable(column, value)) {
        skippedCells++;
        return;
      }
      ops.push({
        op: "set_cell",
        sheet_id: view.sheet_id,
        row_id: rowId!,
        column,
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
  textHeader: "#a8a8a8",
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
};

export function CostGrid({ view, apply, selection, setSelection, dark }: Props) {
  const columns = useMemo(() => columnsFor(view), [view]);
  // The latest rows, readable from callbacks that run after an await.
  const viewRef = useRef(view);
  viewRef.current = view;
  const selectionRef = useRef(selection);
  selectionRef.current = selection;

  const getCellContent = useCallback(
    ([col, row]: Item): GridCell => {
      const r = view.rows[row];
      const def = columns[col];
      if (!def) return { kind: GridCellKind.Text, data: "", displayData: "", allowOverlay: false };
      if (!r) {
        // A blank row below the data: editable, becomes a real row when typed into.
        return def.edit
          ? { kind: GridCellKind.Text, data: "", displayData: "", allowOverlay: true }
          : { kind: GridCellKind.Text, data: "", displayData: "", allowOverlay: false, readonly: true };
      }

      if (def.id === "url") {
        // A text cell (so typing into it keeps the first key), drawn as a link.
        const url = r.url ?? "";
        return {
          kind: GridCellKind.Text,
          data: url,
          displayData: url.replace(/^https?:\/\/(www\.)?/, ""),
          allowOverlay: true,
          themeOverride: { textDark: dark ? "#7fb2ff" : "#2f5fc4" },
        };
      }

      if (def.edit) {
        const value = r[def.edit] ?? "";
        return {
          kind: GridCellKind.Text,
          data: value,
          displayData:
            def.id === "unit_price" ? group(padMinor(value, r.currency)) : def.numeric ? group(value) : value,
          allowOverlay: true,
          contentAlign: def.numeric ? "right" : "left",
        };
      }

      const value = def.id === "line_total" ? r.line_total : r.converted[Number(def.id.slice(5))];
      const missing = r.unit_price != null && value == null;
      return {
        kind: GridCellKind.Text,
        data: value ?? "",
        displayData: missing ? "?" : group(value),
        allowOverlay: false,
        readonly: true,
        contentAlign: "right",
        themeOverride: missing ? { textDark: "#d9534f" } : { textDark: dark ? "#c8c8c8" : "#333" },
      };
    },
    [view, columns, dark],
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
        const def = columns[location[0]];
        if (!def?.edit) continue;
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
        } else if (data === (row[def.edit] ?? "")) {
          continue;
        }
        ops.push({
          op: "set_cell",
          sheet_id: v.sheet_id,
          row_id: rowId!,
          column: def.edit,
          value: data.trim() === "" ? null : data,
        });
      }
      if (!ops.length) return true;
      void apply(ops.length === 1 ? ops[0] : { op: "batch", ops }).then((snap) => {
        // New rows are added straight after the data. If a row was started further
        // down, move the cursor up to it so Tab carries on along the same row.
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
    [apply, columns, setSelection],
  );

  const onDelete = useCallback(
    (sel: GridSelection) => {
      const v = viewRef.current;
      const rows = sel.rows.toArray();
      if (rows.length) {
        const ops: Op[] = rows
          .filter((i) => v.rows[i])
          .map((i) => ({ op: "set_row_deleted", sheet_id: v.sheet_id, row_id: v.rows[i].row_id, deleted: true }));
        if (ops.length) void apply({ op: "batch", ops });
        setSelection(emptySelection);
        return false;
      }
      const range = sel.current?.range;
      if (!range) return false;
      const ops: Op[] = [];
      for (let y = range.y; y < range.y + range.height; y++) {
        const row = v.rows[y];
        if (!row) continue;
        for (let x = range.x; x < range.x + range.width; x++) {
          const def = columns[x];
          if (def?.edit && row[def.edit] != null) {
            ops.push({ op: "set_cell", sheet_id: v.sheet_id, row_id: row.row_id, column: def.edit, value: null });
          }
        }
      }
      if (ops.length) void apply(ops.length === 1 ? ops[0] : { op: "batch", ops });
      return false;
    },
    [apply, columns, setSelection],
  );

  const onCellClicked = useCallback(
    ([col, row]: Item, e: { metaKey: boolean; ctrlKey: boolean }) => {
      const url = columns[col]?.id === "url" ? viewRef.current.rows[row]?.url : null;
      if (url && (e.metaKey || e.ctrlKey)) void openUrl(/^[a-z]+:\/\//i.test(url) ? url : `https://${url}`);
    },
    [columns],
  );

  return (
    <DataEditor
      onCellClicked={onCellClicked}
      columns={columns}
      rows={view.rows.length + BLANK_ROWS}
      getCellContent={getCellContent}
      getCellsForSelection={true}
      onCellsEdited={onCellsEdited}
      onPaste={false}
      onDelete={onDelete}
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
    />
  );
}
