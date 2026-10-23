import { useEffect, useRef, useState } from "react";
import { api } from "./api";
import type { ColumnSpec, ColumnView } from "./types";

type Kind = ColumnSpec["type"];

const KINDS: { kind: Kind; label: string }[] = [
  { kind: "text", label: "Text" },
  { kind: "number", label: "Number" },
  { kind: "money", label: "Money" },
  { kind: "calc", label: "Calculated" },
  { kind: "link", label: "Link" },
];

/** Examples written with this sheet's own column names where possible. */
function examples(cols: ColumnView[]): [string, string][] {
  const money = cols.filter((c) => c.type === "money" || c.type === "calc").map((c) => c.var);
  const num = cols.find((c) => c.type === "number")?.var ?? "qty";
  const a = money[0] ?? "price_uah";
  const b = money[1] ?? "price_usd";
  return [
    [`${a} * ${num}`, "multiply two columns on the same row"],
    [`to(${a}, USD)`, "convert to dollars at the sheet's rates"],
    [`coalesce(${a}, ${b})`, `${a} if it's filled in, otherwise ${b}`],
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
  at: { x: number; y: number };
  onSave: (spec: ColumnSpec) => void;
  onInsert?: (side: "left" | "right") => void;
  onDelete?: () => void;
  onClose: () => void;
}

export function ColumnEditor({ sheetId, column, others, at, onSave, onInsert, onDelete, onClose }: ColumnEditorProps) {
  const [name, setName] = useState(column?.name ?? "");
  // The variable name follows the display name until it is edited by hand.
  const [varName, setVarName] = useState(column?.var ?? "");
  const [varTouched, setVarTouched] = useState(!!column);
  const [kind, setKind] = useState<Kind>(column?.type ?? "text");
  const [currency, setCurrency] = useState(column?.type === "money" ? column.currency : "USD");
  const [formula, setFormula] = useState(column?.formula_display ?? (column?.type === "calc" ? column.formula : ""));
  const [total, setTotal] = useState(column?.total ?? false);
  const [preview, setPreview] = useState<{ ok: boolean; lines: string[] } | null>(null);
  const formulaRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!varTouched) setVarName(varFromName(name));
  }, [name, varTouched]);

  // Try the formula as it is typed.
  useEffect(() => {
    if (kind !== "calc" || !formula.trim()) {
      setPreview(null);
      return;
    }
    let live = true;
    const t = setTimeout(() => {
      api
        .previewFormula(sheetId, column?.col_id ?? null, formula)
        .then((lines) => live && setPreview({ ok: true, lines }))
        .catch((e) => live && setPreview({ ok: false, lines: [String(e).replace(/^invalid value: /, "")] }));
    }, 200);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [kind, formula, sheetId, column?.col_id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const insert = (text: string) => {
    const el = formulaRef.current;
    if (!el) return setFormula((f) => f + text);
    const start = el.selectionStart ?? formula.length;
    const end = el.selectionEnd ?? formula.length;
    const next = formula.slice(0, start) + text + formula.slice(end);
    setFormula(next);
    requestAnimationFrame(() => {
      el.focus();
      el.setSelectionRange(start + text.length, start + text.length);
    });
  };

  const currencyOk = /^[A-Za-z]{3}$/.test(currency.trim());
  const varOk = VAR_OK.test(varName) && !/^[cr]\d+$/.test(varName);
  const canSave =
    name.trim() !== "" &&
    varOk &&
    (kind !== "money" || currencyOk) &&
    (kind !== "calc" || (formula.trim() !== "" && preview?.ok !== false));

  const save = () => {
    if (!canSave) return;
    const base = { name: name.trim(), var: varName, total };
    const spec: ColumnSpec =
      kind === "money"
        ? { ...base, type: "money", currency: currency.trim().toUpperCase() }
        : kind === "calc"
          ? { ...base, type: "calc", formula: formula.trim() }
          : { ...base, type: kind };
    onSave(spec);
  };

  const left = Math.max(8, Math.min(at.x, window.innerWidth - 420));
  const top = Math.max(8, Math.min(at.y, window.innerHeight - 520));

  return (
    <>
      <div className="backdrop" onClick={onClose} />
      <form
        className="column-editor"
        style={{ left, top }}
        onSubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <div className="ce-head">
          <span className="ce-id">{column ? column.col_id : "new"}</span>
          <span className="ce-title">{column ? "Edit column" : "New column"}</span>
        </div>

        <div className="ce-names-row">
          <label>
            Display name
            <input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="Price (UAH)" />
          </label>
          <label>
            Variable name
            <input
              value={varName}
              onChange={(e) => {
                setVarTouched(true);
                setVarName(e.target.value);
              }}
              placeholder="price_uah"
              className={varOk ? "mono" : "mono bad"}
              spellCheck={false}
            />
          </label>
        </div>

        <div className="kinds">
          {KINDS.map((k) => (
            <button
              type="button"
              key={k.kind}
              className={k.kind === kind ? "on" : ""}
              onClick={() => {
                setKind(k.kind);
                if (k.kind === "calc") setTotal(true);
              }}
            >
              {k.label}
            </button>
          ))}
          {kind === "money" && (
            <input
              value={currency}
              onChange={(e) => setCurrency(e.target.value.toUpperCase())}
              maxLength={3}
              size={4}
              className={currencyOk ? "currency" : "currency bad"}
              placeholder="UAH"
              title="Currency"
            />
          )}
        </div>

        {kind === "calc" && (
          <>
            <input
              ref={formulaRef}
              value={formula}
              onChange={(e) => setFormula(e.target.value)}
              placeholder="unit_usd * qty"
              className={preview?.ok === false ? "mono formula bad" : "mono formula"}
              spellCheck={false}
            />
            {preview && (
              <div className={preview.ok ? "ce-preview" : "ce-error"}>
                {preview.ok ? (preview.lines.length ? preview.lines.join(" · ") : "No rows with values yet") : preview.lines[0]}
              </div>
            )}
            <div className="ce-section">Columns you can use</div>
            <div className="ce-cols">
              {others.map((c) => (
                <button type="button" key={c.col_id} onClick={() => insert(c.var)} title={`Insert ${c.var}`}>
                  <span className="ce-id">{c.col_id}</span> <span className="mono">{c.var}</span>{" "}
                  <span className="muted">{c.name}</span>
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
          Add up (section subtotals and sheet total)
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
