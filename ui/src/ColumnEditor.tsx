import { useEffect, useState } from "react";
import { api } from "./api";
import type { ColumnSpec, ColumnView } from "./types";

type Kind = ColumnSpec["type"];

const KINDS: { kind: Kind; label: string; hint: string }[] = [
  { kind: "text", label: "Text", hint: "Names, notes" },
  { kind: "number", label: "Number", hint: "Quantities" },
  { kind: "money", label: "Money", hint: "Amounts in one currency" },
  { kind: "calc", label: "Calculated", hint: "A formula for every row" },
  { kind: "link", label: "Link", hint: "Web addresses" },
];

export interface ColumnEditorProps {
  /** The column being edited, or null for a new one. */
  column: ColumnView | null;
  /** Names of the sheet's other columns, offered for formulas. */
  otherNames: string[];
  at: { x: number; y: number };
  onSave: (spec: ColumnSpec) => void;
  onInsert?: (side: "left" | "right") => void;
  onDelete?: () => void;
  onClose: () => void;
}

const ref = (name: string) => (/^[\p{L}_][\p{L}\p{N}_]*$/u.test(name) ? name : `[${name}]`);

export function ColumnEditor({ column, otherNames, at, onSave, onInsert, onDelete, onClose }: ColumnEditorProps) {
  const [name, setName] = useState(column?.name ?? "");
  const [kind, setKind] = useState<Kind>(column?.type ?? "text");
  const [currency, setCurrency] = useState(column?.type === "money" ? column.currency : "USD");
  const [formula, setFormula] = useState(column?.type === "calc" ? column.formula : "");
  const [total, setTotal] = useState(column?.total ?? false);
  const [formulaError, setFormulaError] = useState<string | null>(null);

  // Check the formula as it is typed.
  useEffect(() => {
    if (kind !== "calc" || !formula.trim()) {
      setFormulaError(null);
      return;
    }
    let live = true;
    const t = setTimeout(() => {
      api.checkFormula(formula).then((e) => live && setFormulaError(e));
    }, 150);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [kind, formula]);

  // Close on Escape.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const currencyOk = /^[A-Za-z]{3}$/.test(currency.trim());
  const canSave =
    name.trim() !== "" && (kind !== "money" || currencyOk) && (kind !== "calc" || (formula.trim() !== "" && !formulaError));

  const save = () => {
    if (!canSave) return;
    const base = { name: name.trim(), total };
    const spec: ColumnSpec =
      kind === "money"
        ? { ...base, type: "money", currency: currency.trim().toUpperCase() }
        : kind === "calc"
          ? { ...base, type: "calc", formula: formula.trim() }
          : { ...base, type: kind };
    onSave(spec);
  };

  const left = Math.min(at.x, window.innerWidth - 380);
  const top = Math.min(at.y, window.innerHeight - 420);

  return (
    <>
      <div className="backdrop" onClick={onClose} />
      <form
        className="column-editor"
        style={{ left: Math.max(8, left), top: Math.max(8, top) }}
        onSubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <div className="ce-title">{column ? `Column ${column.letter}` : "New column"}</div>
        <label>
          Name
          <input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. UAH, Qty, Total USD" />
        </label>

        <div className="kinds">
          {KINDS.map((k) => (
            <button
              type="button"
              key={k.kind}
              className={k.kind === kind ? "on" : ""}
              onClick={() => {
                setKind(k.kind);
                if (k.kind === "money" || k.kind === "calc") setTotal((t) => t || k.kind === "calc");
              }}
              title={k.hint}
            >
              {k.label}
            </button>
          ))}
        </div>

        {kind === "money" && (
          <label>
            Currency
            <input
              value={currency}
              onChange={(e) => setCurrency(e.target.value.toUpperCase())}
              maxLength={3}
              className={currencyOk ? "" : "bad"}
              placeholder="UAH"
            />
          </label>
        )}

        {kind === "calc" && (
          <>
            <label>
              Formula
              <input
                value={formula}
                onChange={(e) => setFormula(e.target.value)}
                placeholder="e.g. [Unit USD] * Qty"
                className={formulaError ? "bad" : ""}
                spellCheck={false}
              />
            </label>
            {formulaError && <div className="ce-error">{formulaError}</div>}
            {otherNames.length > 0 && (
              <div className="ce-names">
                {otherNames.map((n) => (
                  <button type="button" key={n} onClick={() => setFormula((f) => (f ? `${f} ${ref(n)}` : ref(n)))}>
                    {n}
                  </button>
                ))}
              </div>
            )}
            <div className="ce-help">
              <code>to(UAH, USD)</code> converts at the sheet's rates · <code>coalesce(a, b)</code> first filled in ·{" "}
              <code>round(x)</code> · <code>+ − × ÷</code>
            </div>
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
