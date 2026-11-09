import { useEffect, useMemo, useRef, useState } from "react";
import { fuzzy } from "./ColumnJump";
import { lastUsed, relativeTime } from "./Home";
import type { SheetEntry } from "./types";

interface Props {
  load: () => Promise<SheetEntry[]>;
  onPick: (e: SheetEntry) => void;
  onClose: () => void;
  onError: (e: unknown) => void;
}

/**
 * ⌘K: type a few letters of any sheet, in any project, and press Enter.
 * With no query, open tabs come first, then the most recently used.
 */
export function GoToSheet({ load, onPick, onClose, onError }: Props) {
  const [entries, setEntries] = useState<SheetEntry[]>([]);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLUListElement>(null);

  useEffect(() => {
    load().then(setEntries).catch(onError);
  }, [load, onError]);

  const hits = useMemo(() => {
    const q = query.trim().toLowerCase();
    const list = entries.filter((e) => !e.archived);
    if (!q) return list.sort((a, b) => Number(b.open) - Number(a.open) || lastUsed(b) - lastUsed(a)).slice(0, 50);
    return list
      .map((e) => ({ e, s: Math.max(fuzzy(q, e.name), fuzzy(q, `${e.unfiled ? "unfiled" : e.project_name} ${e.name}`) - 1) }))
      .filter((x) => x.s >= 0)
      .sort((a, b) => b.s - a.s || lastUsed(b.e) - lastUsed(a.e))
      .slice(0, 50)
      .map((x) => x.e);
  }, [entries, query]);

  useEffect(() => setActive(0), [query]);
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
      if (h) onPick(h);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  const now = Date.now();
  return (
    <div className="jump-backdrop" onMouseDown={onClose}>
      <div className="jump goto" onMouseDown={(e) => e.stopPropagation()}>
        <input autoFocus placeholder="Go to sheet: name or project…" value={query} onChange={(e) => setQuery(e.target.value)} onKeyDown={onKeyDown} />
        <ul ref={listRef}>
          {hits.map((h, i) => (
            <li
              key={`${h.project}/${h.sheet_id}`}
              className={i === active ? "on" : ""}
              onMouseEnter={() => setActive(i)}
              onMouseDown={(e) => {
                e.preventDefault();
                onPick(h);
              }}
            >
              <span className="jump-name">{h.name}</span>
              <span className="jump-var">{h.unfiled ? "Unfiled" : h.project_name}</span>
              <span className="jump-kind">{h.open ? "open" : relativeTime(lastUsed(h), now)}</span>
            </li>
          ))}
          {!hits.length && <li className="jump-none">{entries.length ? "No sheet matches." : "Loading…"}</li>}
        </ul>
        <div className="jump-foot">↑↓ choose · Enter open · Esc close · ⇧⌘H Home</div>
      </div>
    </div>
  );
}
