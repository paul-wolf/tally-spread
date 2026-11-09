import { useEffect, useState } from "react";
import type { CellView, ColumnView, Op, SheetView, Snapshot } from "./types";
import { columnLetter } from "./ColumnJump";
import { group, money } from "./format";

interface Props {
  view: SheetView;
  /** Grid row the cursor is on (follows the selection). */
  row: number;
  /** Grid column the cursor is on, highlighted in the list. */
  col: number | null;
  apply: (op: Op) => Promise<Snapshot | undefined>;
  /** Move the grid cursor: another row (↑↓ in the drawer) or a column (click a field name). */
  onMove: (col: number, row: number) => void;
  onClose: () => void;
}

function shown(c: ColumnView, cell: CellView | undefined): string {
  if (!cell?.value) return "";
  return cell.currency ? money(cell.value, cell.currency) : c.type === "number" || c.type === "calc" ? group(cell.value) : cell.value;
}

/** The current row, transposed: one line per column, field names down the left. */
export function RowDrawer({ view, row, col, apply, onMove, onClose }: Props) {
  const [filter, setFilter] = useState("");
  const r = view.rows[row];
  const f = filter.trim().toLowerCase();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !(e.target as HTMLElement)?.closest?.(".row-drawer input")) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const commit = (c: ColumnView, value: string) => {
    if (!r || value === (r.cells[view.columns.indexOf(c)]?.raw ?? "")) return;
    void apply({ op: "set_cell", sheet_id: view.sheet_id, row_id: r.row_id, col_id: c.col_id, value: value.trim() === "" ? null : value });
  };

  return (
    <aside className="row-drawer">
      <div className="panel-head">
        <strong>Row {row + 1}</strong>
        {r?.kind === "section" && <span className="muted small">section</span>}
        <button onClick={() => onMove(col ?? 0, Math.max(0, row - 1))} disabled={row === 0} title="Previous row">
          ↑
        </button>
        <button onClick={() => onMove(col ?? 0, Math.min(view.rows.length - 1, row + 1))} disabled={row >= view.rows.length - 1} title="Next row">
          ↓
        </button>
        <button className="close-x" onClick={onClose} title="Close (Esc)">
          ×
        </button>
      </div>
      <input className="row-filter" placeholder="Filter fields…" value={filter} onChange={(e) => setFilter(e.target.value)} />
      {!r ? (
        <div className="muted small pad">This row is empty. Type in the grid to start it.</div>
      ) : (
        <dl className="row-fields">
          {view.columns.map((c, j) => {
            if (f && !`${c.name} ${c.var} ${columnLetter(j)}`.toLowerCase().includes(f)) return null;
            const cell = r.cells[j];
            return (
              <div key={c.col_id} className={`row-field ${j === col ? "on" : ""}`}>
                <dt onClick={() => onMove(j, row)} title={`${columnLetter(j)} · ${c.col_id} · ${c.var} — click to go there in the grid`}>
                  <span className="rf-letter">{columnLetter(j)}</span>
                  {c.name}
                </dt>
                <dd>
                  {c.type === "calc" ? (
                    <span className="rf-calc" title={`= ${c.formula_display ?? ""}`}>
                      {shown(c, cell)}
                    </span>
                  ) : (
                    <FieldInput key={`${r.row_id}/${c.col_id}`} initial={cell?.raw ?? ""} onCommit={(v) => commit(c, v)} />
                  )}
                  {(cell?.error || cell?.warning) && <span className={`rf-note ${cell.error ? "error" : ""}`}>{cell.error ?? cell.warning}</span>}
                </dd>
              </div>
            );
          })}
        </dl>
      )}
    </aside>
  );
}

/** Commits on Enter or leaving the field; Esc puts the old value back. */
function FieldInput({ initial, onCommit }: { initial: string; onCommit: (v: string) => void }) {
  const [v, setV] = useState(initial);
  useEffect(() => setV(initial), [initial]);
  return (
    <input
      value={v}
      onChange={(e) => setV(e.target.value)}
      onBlur={() => onCommit(v)}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        if (e.key === "Escape") {
          setV(initial);
          e.stopPropagation();
        }
      }}
    />
  );
}
