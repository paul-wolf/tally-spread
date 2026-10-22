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

/** Operations that write a block of values starting at a cell, adding rows as needed. */
export function opsForBlock(view: CostListView, target: Item, values: readonly (readonly string[])[]): Op[] {
  const cols = columnsFor(view);
  const ops: Op[] = [];
  let nextPos = Math.max(0, ...view.rows.map((r) => r.position)) + 1;
  const [col0, row0] = target;
  values.forEach((line, i) => {
    let rowId = view.rows[row0 + i]?.row_id;
    if (!rowId) {
      if (line.every((v) => v.trim() === "")) return;
      rowId = newId();
      ops.push({ op: "insert_row", sheet_id: view.sheet_id, row_id: rowId, position: nextPos++ });
    }
    line.forEach((value, j) => {
      const def = cols[col0 + j];
      if (!def?.edit) return;
      ops.push({
        op: "set_cell",
        sheet_id: view.sheet_id,
        row_id: rowId!,
        column: def.edit,
        value: value.trim() === "" ? null : value,
      });
    });
  });
  return ops;
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

  const getCellContent = useCallback(
    ([col, row]: Item): GridCell => {
      const r = view.rows[row];
      const def = columns[col];
      if (!r || !def) return { kind: GridCellKind.Text, data: "", displayData: "", allowOverlay: false };

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
      for (const { location, value } of edits) {
        const def = columns[location[0]];
        const row = v.rows[location[1]];
        if (!def?.edit || !row) continue;
        const data = "data" in value && typeof value.data === "string" ? value.data : "";
        const old = row[def.edit] ?? "";
        if (data === old) continue;
        ops.push({
          op: "set_cell",
          sheet_id: v.sheet_id,
          row_id: row.row_id,
          column: def.edit,
          value: data.trim() === "" ? null : data,
        });
      }
      if (ops.length) void apply(ops.length === 1 ? ops[0] : { op: "batch", ops });
      return true;
    },
    [apply, columns],
  );

  const onRowAppended = useCallback(async () => {
    const v = viewRef.current;
    const position = Math.max(0, ...v.rows.map((r) => r.position)) + 1;
    const snap = await apply({ op: "insert_row", sheet_id: v.sheet_id, row_id: newId(), position });
    // The grid edits the new row right away, before React re-renders with it.
    if (snap?.sheet) viewRef.current = snap.sheet;
    return "bottom" as const;
  }, [apply]);

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
      rows={view.rows.length}
      getCellContent={getCellContent}
      getCellsForSelection={true}
      onCellsEdited={onCellsEdited}
      onPaste={false}
      onDelete={onDelete}
      onRowAppended={onRowAppended}
      trailingRowOptions={{ hint: "New item", sticky: true, tint: true, targetColumn: 0 }}
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
