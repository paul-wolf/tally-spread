import { useEffect, useState } from "react";
import { api } from "./api";
import type { Snapshot, UserFunction } from "./types";

const BLANK: UserFunction = { name: "", params: ["x"], body: "", description: "" };

/** Your own functions, e.g. vat(x) = x * 0.2: usable in every sheet and project. */
export function FunctionsPanel(props: {
  functions: UserFunction[];
  sheetId: string | null;
  onSaved: (s: Snapshot) => void;
  onError: (e: unknown) => void;
  onClose: () => void;
}) {
  const [editing, setEditing] = useState<{ fn: UserFunction; oldName: string | null } | null>(
    props.functions.length ? null : { fn: BLANK, oldName: null },
  );
  const [params, setParams] = useState(props.functions.length ? "" : BLANK.params.join(", "));
  const [trial, setTrial] = useState("");
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && props.onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props]);

  const draft: UserFunction | null = editing
    ? { ...editing.fn, params: params.split(",").map((p) => p.trim()).filter(Boolean) }
    : null;

  // Try the draft as it is typed.
  useEffect(() => {
    if (!draft || !draft.name || !draft.body.trim() || !trial.trim()) {
      setResult(null);
      return;
    }
    let live = true;
    const t = setTimeout(() => {
      api
        .tryFunction(draft, trial)
        .then((text) => live && setResult({ ok: true, text }))
        .catch((e) => live && setResult({ ok: false, text: String(e) }));
    }, 200);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [draft?.name, draft?.body, params, trial]); // eslint-disable-line react-hooks/exhaustive-deps

  const start = (fn: UserFunction, oldName: string | null) => {
    setEditing({ fn, oldName });
    setParams(fn.params.join(", "));
    setTrial(fn.name ? `${fn.name}(${fn.params.map(() => "100").join(", ")})` : "");
  };

  const set = (patch: Partial<UserFunction>) => editing && setEditing({ ...editing, fn: { ...editing.fn, ...patch } });

  return (
    <>
      <div className="backdrop" onClick={props.onClose} />
      <div className="panel functions">
        <div className="panel-head">
          <strong>Functions</strong>
          <span className="muted">Your own functions, usable in any formula in every sheet and project.</span>
          <button className="close-x" onClick={props.onClose} title="Close (Esc)">
            ×
          </button>
        </div>

        {!editing && (
          <>
            <table className="fn-table">
              <tbody>
                {props.functions.map((f) => (
                  <tr key={f.name} onClick={() => start(f, f.name)} title="Click to edit">
                    <td className="mono">
                      {f.name}({f.params.join(", ")}) = {f.body}
                    </td>
                    <td className="muted">{f.description}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            <div className="ce-actions">
              <button className="primary" onClick={() => start(BLANK, null)}>
                New function
              </button>
            </div>
          </>
        )}

        {editing && draft && (
          <form
            className="fn-form"
            onSubmit={async (e) => {
              e.preventDefault();
              try {
                props.onSaved(await api.saveFunction(draft, editing.oldName, props.sheetId));
                setEditing(null);
              } catch (err) {
                props.onError(err);
              }
            }}
          >
            <div className="fn-def mono">
              <input autoFocus placeholder="vat" value={editing.fn.name} onChange={(e) => set({ name: e.target.value })} size={12} />
              (
              <input placeholder="x" value={params} onChange={(e) => setParams(e.target.value)} size={12} />) =
              <input className="fn-body" placeholder="x * 0.2" value={editing.fn.body} onChange={(e) => set({ body: e.target.value })} />
            </div>
            <input placeholder="What it does (optional)" value={editing.fn.description} onChange={(e) => set({ description: e.target.value })} />
            <div className="fn-try">
              <span className="muted">Try:</span>
              <input className="mono" placeholder="vat(100)" value={trial} onChange={(e) => setTrial(e.target.value)} />
              {result && <span className={result.ok ? "ok" : "ce-error"}>{result.ok ? `= ${result.text}` : result.text}</span>}
            </div>
            <div className="muted small">
              The formula can use its parameters, numbers, currencies (<code>USD</code>), built-in functions and your other functions.
              Columns are passed in when you use it: <code>vat(total_usd)</code>.
            </div>
            <div className="ce-actions">
              {editing.oldName && (
                <button
                  type="button"
                  className="danger"
                  onClick={async () => {
                    try {
                      props.onSaved(await api.deleteFunction(editing.oldName!, props.sheetId));
                      setEditing(null);
                    } catch (err) {
                      props.onError(err);
                    }
                  }}
                >
                  Delete
                </button>
              )}
              <span className="spacer" />
              <button type="button" onClick={() => (props.functions.length ? setEditing(null) : props.onClose())}>
                Cancel
              </button>
              <button type="submit" className="primary" disabled={!editing.fn.name || !editing.fn.body.trim()}>
                Save
              </button>
            </div>
          </form>
        )}
      </div>
    </>
  );
}
