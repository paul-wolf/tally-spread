import { useEffect, useState } from "react";
import { todayIso } from "./format";
import type { FixedRate, Op, RateSource, SheetView } from "./types";
import { SOURCE_LABEL } from "./types";

/** Up to 6 decimals for reading; the full rate is in the tooltip and used in sums. */
function shortRate(r: string): string {
  const [i, f = ""] = r.split(".");
  return f.length > 6 ? `${i}.${f.slice(0, 6)}…` : r;
}

/**
 * The sheet's rate card: one rate per currency against the base, with where it
 * came from. Rates float with the latest daily rates, or are pegged to a date or
 * to values typed in (pins).
 */
export function RateCardPanel(props: { view: SheetView; onApply: (op: Op) => void; onClose: () => void }) {
  const { view } = props;
  const card = view.rate_card;
  const [base, setBase] = useState(card.base);
  const [pins, setPins] = useState<Record<string, string>>(() =>
    Object.fromEntries(view.fixed_rates.filter((f) => f.base === card.base).map((f) => [f.quote, f.rate])),
  );
  useEffect(() => setBase(card.base), [card.base]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && props.onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props]);

  const locked = view.rates.mode === "locked" ? view.rates.date : null;
  const id = view.sheet_id;

  const savePins = (next: Record<string, string>) => {
    setPins(next);
    const rates: FixedRate[] = Object.entries(next)
      .filter(([, r]) => r.trim() !== "")
      .map(([quote, rate]) => ({ base: card.base, quote, rate: rate.trim() }));
    props.onApply({ op: "set_sheet_fixed_rates", sheet_id: id, rates });
  };

  return (
    <>
      <div className="backdrop" onClick={props.onClose} />
      <div className="panel rate-card">
        <div className="panel-head">
          <strong>Rate card</strong>
          <span className="muted">
            One rate per currency for this sheet, against {card.base}. Every conversion goes through it.
          </span>
          <button className="close-x" onClick={props.onClose} title="Close (Esc)">
            ×
          </button>
        </div>

        <div className="rc-controls">
          <label>
            Base
            <input
              value={base}
              maxLength={3}
              size={4}
              onChange={(e) => setBase(e.target.value.toUpperCase())}
              onBlur={() => {
                const b = base.trim().toUpperCase();
                if (/^[A-Z]{3}$/.test(b) && b !== card.base) props.onApply({ op: "set_sheet_base", sheet_id: id, base: b });
                else setBase(card.base);
              }}
              onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
            />
            {!card.base_chosen && <span className="muted small">(automatic)</span>}
          </label>
          <label>
            Prefer
            <select
              value={view.effective_sources[0] ?? "ecb"}
              onChange={(e) => props.onApply({ op: "set_sheet_sources", sheet_id: id, sources: [e.target.value as RateSource] })}
            >
              <option value="ecb">ECB</option>
              <option value="nbu">NBU</option>
              <option value="hmrc_monthly">HMRC monthly</option>
            </select>
          </label>
          <label>
            Rates
            <select
              value={locked ? "locked" : "live"}
              onChange={(e) =>
                props.onApply({
                  op: "set_sheet_rates",
                  sheet_id: id,
                  rates: e.target.value === "live" ? { mode: "live" } : { mode: "locked", date: view.as_of ?? todayIso() },
                })
              }
            >
              <option value="live">Floating (latest)</option>
              <option value="locked">Pegged to a date</option>
            </select>
            {locked && (
              <input
                type="date"
                value={locked}
                onChange={(e) =>
                  e.target.value && props.onApply({ op: "set_sheet_rates", sheet_id: id, rates: { mode: "locked", date: e.target.value } })
                }
              />
            )}
          </label>
        </div>

        <table className="rc-table">
          <thead>
            <tr>
              <th>Currency</th>
              <th className="num">1 {card.base} =</th>
              <th>Source</th>
              <th>Rate for</th>
              <th>Fetched</th>
              <th>Pin</th>
            </tr>
          </thead>
          <tbody>
            {card.entries.length === 0 && (
              <tr>
                <td colSpan={6} className="muted">
                  This sheet uses only {card.base}.
                </td>
              </tr>
            )}
            {card.entries.map((e) => (
              <tr key={e.currency} className={e.error ? "bad-row" : ""}>
                <td>
                  <strong>{e.currency}</strong>
                </td>
                <td className="num mono" title={e.rate ?? ""}>
                  {e.rate ? shortRate(e.rate) : "—"}
                </td>
                <td>
                  {e.pinned
                    ? "Pinned by you"
                    : e.error
                      ? e.error
                      : e.legs.map((l) => `${SOURCE_LABEL[l.source]} ${l.from}/${l.to}`).join(" + ")}
                </td>
                <td>{e.pinned ? "—" : (e.rate_date ?? "—")}</td>
                <td className="muted small">{e.fetched_at ? new Date(e.fetched_at).toLocaleString() : "—"}</td>
                <td>
                  <input
                    className="mono pin"
                    placeholder="—"
                    value={pins[e.currency] ?? ""}
                    onChange={(ev) => setPins((p) => ({ ...p, [e.currency]: ev.target.value }))}
                    onBlur={(ev) => {
                      const v = ev.target.value.trim();
                      const was = view.fixed_rates.find((f) => f.base === card.base && f.quote === e.currency)?.rate ?? "";
                      if (v !== was) savePins({ ...pins, [e.currency]: v });
                    }}
                    onKeyDown={(ev) => ev.key === "Enter" && (ev.target as HTMLInputElement).blur()}
                    title={`Type a rate to peg 1 ${card.base} = ? ${e.currency} for this sheet; clear it to float again`}
                  />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        <div className="muted small">
          {locked ? `Pegged to rates for ${locked}.` : `Floating: the latest daily rates on or before ${card.as_of}.`} ECB does not
          publish hryvnia, so UAH comes from NBU. A pin overrides any source.
        </div>
      </div>
    </>
  );
}
