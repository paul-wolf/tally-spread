import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { api } from "./api";
import type { ColumnSpec, ColumnView, UserFunction } from "./types";

/** Where a column's values come from. */
type Source = "typed" | "calc";
/** What a column holds. Calculated columns are "any" (whatever the formula gives) or money/number. */
type Type = "text" | "number" | "money" | "link" | "any";

const TYPED: { t: Type; label: string }[] = [
  { t: "text", label: "Text" },
  { t: "number", label: "Number" },
  { t: "money", label: "Money" },
  { t: "link", label: "Link" },
];
const CALC: { t: Type; label: string }[] = [
  { t: "money", label: "Money" },
  { t: "any", label: "As the formula gives" },
];

/** Built-in functions, offered while typing a formula. */
const BUILTINS: [string, string][] = [
  ["coalesce", "first value that is filled in"],
  ["to", "to(amount, USD): convert"],
  ["round", "round(x, 2)"],
  ["if", "if(cond, a, b)"],
  ["min", "smallest"],
  ["max", "largest"],
  ["abs", "without the sign"],
];
const COMMON_CURRENCIES = ["USD", "UAH", "GBP", "EUR"];

type Suggestion = { text: string; insert: string; hint: string; kind: string };

/** What could complete the name being typed just before the cursor. */
function suggestionsFor(
  before: string,
  cols: ColumnView[],
  fns: UserFunction[],
): { prefix: string; items: Suggestion[] } | null {
  const m = /(^|[^A-Za-z0-9_\[])([A-Za-z_][A-Za-z0-9_]*)$/.exec(before);
  if (!m) return null;
  const prefix = m[2];
  const p = prefix.toLowerCase();
  const all: Suggestion[] = [
    ...cols.map((c) => ({ text: c.var, insert: c.var, hint: `${c.col_id} · ${c.name}`, kind: "column" })),
    ...BUILTINS.map(([f, h]) => ({ text: `${f}()`, insert: `${f}(`, hint: h, kind: "function" })),
    ...fns.map((f) => ({ text: `${f.name}()`, insert: `${f.name}(`, hint: f.description || `${f.name}(${f.params.join(", ")})`, kind: "yours" })),
  ];
  const curs = [...new Set([...COMMON_CURRENCIES, ...cols.flatMap((c) => (c.type === "money" ? [c.currency] : []))])];
  if (/^[A-Z]+$/.test(prefix)) all.push(...curs.map((c) => ({ text: c, insert: c, hint: "currency", kind: "currency" })));
  const name = (s: Suggestion) => s.insert.replace(/\($/, "").toLowerCase();
  const starts = all.filter((s) => name(s).startsWith(p));
  const contains = all.filter((s) => !name(s).startsWith(p) && name(s).includes(p) && p.length >= 2);
  const items = [...starts, ...contains].filter((s) => name(s) !== p).slice(0, 8);
  return items.length ? { prefix, items } : null;
}

/** Examples written with this sheet's own column names where possible. */
function examples(cols: ColumnView[]): [string, string][] {
  const money = cols.filter((c) => c.type === "money" || c.type === "calc").map((c) => c.var);
  const num = cols.find((c) => c.type === "number")?.var ?? "qty";
  const a = money[0] ?? "price_uah";
  const b = money[1] ?? "price_usd";
  return [
    [`coalesce(${a}, ${b})`, `${a} if it's filled in, otherwise ${b}`],
    [`${a} * ${num}`, "multiply two columns on the same row"],
    [`to(${a}, USD)`, "convert to dollars at the sheet's rates"],
    [`round(${a} * 1.2, 2)`, "round to 2 decimals"],
  ];
}

/** "Price (UAH)" -> price_uah, as the engine derives it. */
export function varFromName(name: string): string {
  let v = name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "");
  if (!v) v = "col";
  if (/^\d/.test(v) || /^[cr]\d+$/.test(v)) v = `col_${v}`;
  return v;
}

const VAR_OK = /^[a-z][a-z0-9_]*$/;

export interface ColumnEditorProps {
  sheetId: string;
  /** The column being edited, or null for a new one. */
  column: ColumnView | null;
  /** The sheet's other columns, offered for formulas. */
  others: ColumnView[];
  /** The sheet's base currency: the default for calculated money columns. */
  baseCurrency: string;
  /** Your own functions, offered while typing a formula. */
  functions?: UserFunction[];
  at: { x: number; y: number };
  onSave: (spec: ColumnSpec) => void;
  onInsert?: (side: "left" | "right") => void;
  onDelete?: () => void;
  onClose: () => void;
}

function initial(column: ColumnView | null, base: string): { source: Source; type: Type; currency: string } {
  if (!column) return { source: "typed", type: "text", currency: base };
  if (column.type === "calc") {
    const cur = (column as { currency?: string | null }).currency;
    return { source: "calc", type: cur ? "money" : "any", currency: cur ?? base };
  }
  return { source: "typed", type: column.type, currency: column.type === "money" ? column.currency : base };
}

export function ColumnEditor({ sheetId, column, others, baseCurrency, functions = [], at, onSave, onInsert, onDelete, onClose }: ColumnEditorProps) {
  const init = initial(column, baseCurrency);
  const [name, setName] = useState(column?.name ?? "");
  // The variable name follows the display name until it is edited by hand.
  const [varName, setVarName] = useState(column?.var ?? "");
  // An existing column's variable name follows renames when it still matches its
  // display name (e.g. "E" -> e); one chosen by hand stays put.
  const [varTouched, setVarTouched] = useState(!!column && column.var !== varFromName(column.name));
  const [drag, setDrag] = useState<{ dx: number; dy: number }>({ dx: 0, dy: 0 });
  const [source, setSource] = useState<Source>(init.source);
  const [type, setType] = useState<Type>(init.type);
  const [currency, setCurrency] = useState(init.currency);
  const [formula, setFormula] = useState(column?.formula_display ?? (column?.type === "calc" ? column.formula : ""));
  const [total, setTotal] = useState(column?.total ?? false);
  const [bold, setBold] = useState(column?.style?.bold ?? false);
  const [preview, setPreview] = useState<{ ok: boolean; lines: string[] } | null>(null);
  const formulaRef = useRef<HTMLInputElement>(null);
  // Autocomplete for the name being typed in the formula.
  const [caret, setCaret] = useState<number | null>(null);
  const [acIndex, setAcIndex] = useState(0);
  const [acOff, setAcOff] = useState(false);
  const ac = caret === null || acOff ? null : suggestionsFor(formula.slice(0, caret), others, functions);

  const currencyOk = /^[A-Z]{3}$/.test(currency.trim());
  // A name suggested from what the column holds, accepted with Tab or a click.
  const suggestion =
    type === "money" && currencyOk ? (source === "calc" ? `Total (${currency})` : `Price (${currency})`) : type === "number" ? "Qty" : "";

  useEffect(() => {
    if (!varTouched) setVarName(name ? varFromName(name) : "");
  }, [name, varTouched]);

  // Try the formula as it is typed.
  useEffect(() => {
    if (source !== "calc" || !formula.trim()) {
      setPreview(null);
      return;
    }
    let live = true;
    const cur = type === "money" && currencyOk ? currency : null;
    const t = setTimeout(() => {
      api
        .previewFormula(sheetId, column?.col_id ?? null, formula, cur)
        .then((lines) => live && setPreview({ ok: true, lines }))
        .catch((e) => live && setPreview({ ok: false, lines: [String(e).replace(/^invalid value: /, "")] }));
    }, 200);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [source, type, currency, currencyOk, formula, sheetId, column?.col_id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const accept = (sg: Suggestion) => {
    if (!ac || caret === null) return;
    const start = caret - ac.prefix.length;
    // Replace the whole word under the cursor, not just the part before it.
    const rest = formula.slice(caret).replace(/^[A-Za-z0-9_]*/, "");
    const next = formula.slice(0, start) + sg.insert + rest;
    const pos = start + sg.insert.length;
    setFormula(next);
    setCaret(pos);
    setAcIndex(0);
    requestAnimationFrame(() => {
      formulaRef.current?.focus();
      formulaRef.current?.setSelectionRange(pos, pos);
    });
  };

  const insert = (text: string) => {
    const el = formulaRef.current;
    if (!el) return setFormula((f) => f + text);
    const start = el.selectionStart ?? formula.length;
    const end = el.selectionEnd ?? formula.length;
    setFormula(formula.slice(0, start) + text + formula.slice(end));
    requestAnimationFrame(() => {
      el.focus();
      el.setSelectionRange(start + text.length, start + text.length);
    });
  };

  const finalName = name.trim() || suggestion;
  const finalVar = varTouched ? varName : finalName ? varFromName(finalName) : "";
  const varOk = VAR_OK.test(finalVar) && !/^[cr]\d+$/.test(finalVar);
  const canSave =
    finalName !== "" &&
    varOk &&
    (type !== "money" || currencyOk) &&
    (source !== "calc" || (formula.trim() !== "" && preview?.ok !== false));

  const save = () => {
    if (!canSave) return;
    const base = { name: finalName, var: finalVar, total, style: bold ? { bold: true } : {} };
    let spec: ColumnSpec;
    if (source === "calc") {
      spec = { ...base, type: "calc", formula: formula.trim(), ...(type === "money" ? { currency } : {}) } as ColumnSpec;
    } else if (type === "money") {
      spec = { ...base, type: "money", currency };
    } else {
      spec = { ...base, type: type as "text" | "number" | "link" };
    }
    onSave(spec);
  };

  const chooseSource = (s: Source) => {
    setSource(s);
    if (s === "calc") {
      setType("money");
      setTotal(true);
    } else if (type === "any") {
      setType("text");
    }
  };

  const left = Math.max(8, Math.min(at.x, window.innerWidth - 430)) + drag.dx;
  const top = Math.max(8, Math.min(at.y, window.innerHeight - 640)) + drag.dy;
  // Drag the dialog by its title bar to see the sheet underneath.
  const startDrag = (e: ReactPointerEvent) => {
    if ((e.target as HTMLElement).closest("button,input")) return;
    e.preventDefault();
    const x0 = e.clientX - drag.dx;
    const y0 = e.clientY - drag.dy;
    const move = (ev: PointerEvent) => setDrag({ dx: ev.clientX - x0, dy: ev.clientY - y0 });
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  // A variable name left over from an old display name (Qty still called e).
  const derived = finalName ? varFromName(finalName) : "";
  const staleVar = !!column && varTouched && finalVar === column.var && derived !== finalVar && derived !== "col";

  return (
    <>
      <div className="backdrop" onClick={onClose} />
      <form
        className="column-editor"
        style={{ left, top, maxHeight: Math.max(240, window.innerHeight - top - 8) }}
        onSubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <div className="ce-head" onPointerDown={startDrag} title="Drag to move">
          <span className="ce-id">{column ? column.col_id : "new"}</span>
          <span className="ce-title">{column ? "Edit column" : "New column"}</span>
        </div>

        <div className="ce-row">
          <span className="ce-label">Values</span>
          <div className="kinds">
            <button type="button" className={source === "typed" ? "on" : ""} onClick={() => chooseSource("typed")}>
              Typed in
            </button>
            <button type="button" className={source === "calc" ? "on" : ""} onClick={() => chooseSource("calc")}>
              Calculated
            </button>
          </div>
        </div>

        <div className="ce-row">
          <span className="ce-label">Type</span>
          <div className="kinds">
            {(source === "typed" ? TYPED : CALC).map((k) => (
              <button type="button" key={k.t} className={k.t === type ? "on" : ""} onClick={() => setType(k.t)}>
                {k.label}
              </button>
            ))}
            {type === "money" && (
              <input
                value={currency}
                onChange={(e) => setCurrency(e.target.value.toUpperCase())}
                maxLength={3}
                size={4}
                className={currencyOk ? "currency" : "currency bad"}
                title="Currency"
              />
            )}
          </div>
        </div>
        {source === "calc" && type === "money" && (
          <div className="muted small">Results are in {currencyOk ? currency : "this currency"}: amounts in other currencies are converted at the sheet's rates.</div>
        )}

        <div className="ce-names-row">
          <label>
            Display name
            <input
              autoFocus
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={suggestion || "Column name"}
              onKeyDown={(e) => {
                if (e.key === "Tab" && !e.shiftKey && !name && suggestion) {
                  e.preventDefault();
                  setName(suggestion);
                }
              }}
            />
          </label>
          <label>
            Variable name
            <input
              value={varTouched ? varName : finalVar}
              onChange={(e) => {
                setVarTouched(true);
                setVarName(e.target.value);
              }}
              className={varOk ? "mono" : "mono bad"}
              spellCheck={false}
            />
          </label>
        </div>
        {staleVar && (
          <div className="small">
            <span className="muted">The variable name doesn't match the display name.</span>{" "}
            <button type="button" className="suggest" onClick={() => setVarName(derived)}>
              Use {derived}
            </button>{" "}
            <span className="muted">(formulas update themselves)</span>
          </div>
        )}
        {!name && suggestion && (
          <div className="small">
            <button type="button" className="suggest" onClick={() => setName(suggestion)}>
              Use “{suggestion}”
            </button>{" "}
            <span className="muted">or press Tab</span>
          </div>
        )}

        {source === "calc" && (
          <>
            <div className="ac-wrap">
              <input
                ref={formulaRef}
                value={formula}
                onChange={(e) => {
                  setFormula(e.target.value);
                  setCaret(e.target.selectionStart);
                  setAcIndex(0);
                  setAcOff(false);
                }}
                onSelect={(e) => setCaret((e.target as HTMLInputElement).selectionStart)}
                onBlur={() => setTimeout(() => setCaret(null), 150)}
                onKeyDown={(e) => {
                  if (!ac) return;
                  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                    e.preventDefault();
                    const n = ac.items.length;
                    setAcIndex((i) => (i + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
                  } else if (e.key === "Tab" || e.key === "Enter") {
                    e.preventDefault();
                    accept(ac.items[Math.min(acIndex, ac.items.length - 1)]);
                  } else if (e.key === "Escape") {
                    e.preventDefault();
                    e.stopPropagation();
                    e.nativeEvent.stopImmediatePropagation();
                    setAcOff(true);
                  }
                }}
                placeholder="Formula, e.g. coalesce(price_uah, price_usd)"
                className={preview?.ok === false ? "mono formula bad" : "mono formula"}
                spellCheck={false}
                autoComplete="off"
              />
              {ac && (
                <div className="ac-list">
                  {ac.items.map((sg, i) => (
                    <button
                      type="button"
                      key={sg.kind + sg.text}
                      className={i === Math.min(acIndex, ac.items.length - 1) ? "on" : ""}
                      onMouseDown={(e) => {
                        e.preventDefault();
                        accept(sg);
                      }}
                    >
                      <span className="mono">{sg.text}</span>
                      <span className="muted">{sg.hint}</span>
                      <span className="muted ac-kind">{sg.kind}</span>
                    </button>
                  ))}
                </div>
              )}
            </div>
            {preview && (
              <div className={preview.ok ? "ce-preview" : "ce-error"}>
                {preview.ok ? (preview.lines.length ? preview.lines.join(" · ") : "No rows with values yet") : preview.lines[0]}
              </div>
            )}
            <div className="ce-section">Columns you can use</div>
            <div className="ce-cols">
              {others.map((c) => (
                <button type="button" key={c.col_id} onClick={() => insert(c.var)} title={`Insert ${c.var}`}>
                  <span className="ce-id">{c.col_id}</span> <span className="mono">{c.var}</span> <span className="muted">{c.name}</span>
                </button>
              ))}
            </div>
            <div className="ce-section">Examples</div>
            <table className="ce-examples">
              <tbody>
                {examples(others).map(([f, what]) => (
                  <tr key={f}>
                    <td className="mono">{f}</td>
                    <td>{what}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </>
        )}

        <label className="check">
          <input type="checkbox" checked={total} onChange={(e) => setTotal(e.target.checked)} />
          Show total (in the Total row and on section headings)
        </label>
        <label className="check">
          <input type="checkbox" checked={bold} onChange={(e) => setBold(e.target.checked)} />
          <strong>Bold</strong>
        </label>

        <div className="ce-actions">
          {onInsert && (
            <>
              <button type="button" onClick={() => onInsert("left")}>
                Insert left
              </button>
              <button type="button" onClick={() => onInsert("right")}>
                Insert right
              </button>
            </>
          )}
          {onDelete && (
            <button type="button" className="danger" onClick={onDelete}>
              Delete
            </button>
          )}
          <span className="spacer" />
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={!canSave}>
            {column ? "Save" : "Add"}
          </button>
        </div>
      </form>
    </>
  );
}
