import { useEffect, useMemo, useRef, useState } from "react";
import type { ColumnView, SheetView } from "./types";

/** Spreadsheet letter for a 0-based column index: 0 → A, 25 → Z, 26 → AA. */
export function columnLetter(i: number): string {
  let s = "";
  for (let n = i + 1; n > 0; n = Math.floor((n - 1) / 26)) s = String.fromCharCode(65 + ((n - 1) % 26)) + s;
  return s;
}

/** What a column is, in words the finder can match: "money USD", "calc", "date". */
function kindLabel(c: ColumnView): string {
  if (c.type === "money") return `money ${c.currency}`;
  if (c.type === "calc") return c.currency ? `calc ${c.currency}` : "calc";
  return c.type;
}

/**
 * Fuzzy score of `q` against `text`: every query letter must appear in order.
 * Consecutive letters and word starts score higher. -1 means no match.
 */
export function fuzzy(q: string, text: string): number {
  const t = text.toLowerCase();
  let score = 0;
  let ti = 0;
  let run = 0;
  for (const ch of q) {
    const at = t.indexOf(ch, ti);
    if (at < 0) return -1;
    const wordStart = at === 0 || /[\s_()·-]/.test(t[at - 1]);
    run = at === ti ? run + 1 : 0;
    score += 1 + run * 2 + (wordStart ? 3 : 0);
    ti = at + 1;
  }
  return score - (t.length - q.length) * 0.01;
}

interface Hit {
  index: number;
  col: ColumnView;
  letter: string;
  score: number;
}

function search(view: SheetView, query: string): Hit[] {
  const q = query.trim().toLowerCase();
  const hits: Hit[] = [];
  view.columns.forEach((col, index) => {
    const letter = columnLetter(index);
    if (!q) return hits.push({ index, col, letter, score: 0 });
    // Exact letter or ID wins outright: "AF", "c14".
    if (q === letter.toLowerCase() || q === col.col_id.toLowerCase()) return hits.push({ index, col, letter, score: 1000 });
    const best = Math.max(
      fuzzy(q, col.name),
      fuzzy(q, col.var) - 0.5,
      fuzzy(q, kindLabel(col)) - 1, // "usd" finds USD money columns, "calc" the calculated ones
      col.formula_display ? fuzzy(q, col.formula_display) - 4 : -1, // weakest: "where is price_uah used?"
    );
    if (best >= 0) hits.push({ index, col, letter, score: best });
  });
  return q ? hits.sort((a, b) => b.score - a.score) : hits;
}

interface Props {
  view: SheetView;
  /** The column the cursor is in now, shown first when the query is empty. */
  current: number | null;
  onPick: (index: number) => void;
  onClose: () => void;
}

/** ⌘J: type a few letters of a column, Enter jumps there on the same row. */
export function ColumnJump({ view, current, onPick, onClose }: Props) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLUListElement>(null);
  const hits = useMemo(() => search(view, query), [view, query]);

  useEffect(() => setActive(!query && current != null ? Math.min(current, hits.length - 1) : 0), [query, current, hits.length]);
  useEffect(() => {
    listRef.current?.children[active]?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) {
      e.preventDefault();
      setActive((a) => Math.min(a + 1, hits.length - 1));
    } else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) {
      e.preventDefault();
      setActive((a) => Math.max(a - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const h = hits[active];
      if (h) onPick(h.index);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  return (
    <div className="jump-backdrop" onMouseDown={onClose}>
      <div className="jump" onMouseDown={(e) => e.stopPropagation()}>
        <input
          autoFocus
          placeholder="Go to column: name, variable, letter, ID, currency…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKeyDown}
        />
        <ul ref={listRef}>
          {hits.map((h, i) => (
            <li
              key={h.col.col_id}
              className={i === active ? "on" : ""}
              onMouseEnter={() => setActive(i)}
              onMouseDown={(e) => {
                e.preventDefault();
                onPick(h.index);
              }}
            >
              <span className="jump-letter">{h.letter}</span>
              <span className="jump-name">{h.col.name}</span>
              <span className="jump-var">{h.col.var}</span>
              <span className="jump-kind">{kindLabel(h.col)}</span>
              {h.col.formula_display && <span className="jump-formula">= {h.col.formula_display}</span>}
            </li>
          ))}
          {!hits.length && <li className="jump-none">No column matches.</li>}
        </ul>
        <div className="jump-foot">↑↓ choose · Enter go · Esc close</div>
      </div>
    </div>
  );
}
