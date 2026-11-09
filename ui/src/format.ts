// Display formatting that never turns amounts into floats.

/** "12499.50" -> "12,499.50" */
export function group(amount: string | null | undefined): string {
  if (amount == null || amount === "") return "";
  const neg = amount.startsWith("-");
  const body = neg ? amount.slice(1) : amount;
  const [int, frac] = body.split(".");
  const grouped = int.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return (neg ? "−" : "") + grouped + (frac !== undefined ? "." + frac : "");
}

const ZERO_DECIMALS = new Set(["JPY", "KRW", "VND", "ISK", "CLP", "PYG", "UGX", "XAF", "XOF"]);
const THREE_DECIMALS = new Set(["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"]);

/** Pad "12499.5" to the currency's decimals ("12499.50"); never removes digits. */
export function padMinor(amount: string, currency: string | null): string {
  if (!amount || !currency) return amount;
  const want = ZERO_DECIMALS.has(currency) ? 0 : THREE_DECIMALS.has(currency) ? 3 : 2;
  const [int, frac = ""] = amount.split(".");
  if (frac.length >= want) return amount;
  return want === 0 ? int : `${int}.${frac.padEnd(want, "0")}`;
}

const SYMBOLS: Record<string, string> = { GBP: "£", EUR: "€", USD: "$", UAH: "₴", PLN: "zł", JPY: "¥" };

export function money(amount: string, currency: string): string {
  const sym = SYMBOLS[currency];
  return sym && sym.length === 1 ? `${sym}${group(amount)}` : `${group(amount)} ${currency}`;
}

export function shortDate(iso: string | null): string {
  if (!iso) return "none";
  const d = new Date(iso + "T00:00:00");
  return d.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

export function todayIso(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** Split clipboard text (tab-separated, as Excel and Google Sheets copy it) into a grid. */
export function parseTsv(text: string): string[][] {
  const rows = text.replace(/\r\n?/g, "\n").replace(/\n$/, "").split("\n");
  return rows.map((r) => r.split("\t"));
}

export function toTsv(grid: string[][]): string {
  return grid.map((r) => r.join("\t")).join("\n");
}

export function transpose<T>(grid: T[][]): T[][] {
  const width = Math.max(0, ...grid.map((r) => r.length));
  return Array.from({ length: width }, (_, c) => grid.map((r) => r[c]));
}

/** A stored moment ("2026-09-18T10:46:16.13Z") in local time: "2026-09-18 13:46:16". */
export function localMoment(iso: string | null | undefined): string | null {
  if (!iso || !/^\d{4}-\d\d-\d\dT\d\d:\d\d/.test(iso)) return null;
  const d = new Date(iso);
  if (isNaN(d.getTime())) return null;
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** Calculated values: numbers grouped, moments in local time, anything else as is. */
export function showValue(v: string | null | undefined): string {
  if (v == null) return "";
  if (/^-?\d+(\.\d+)?$/.test(v)) return group(v);
  return localMoment(v) ?? v;
}

/**
 * Project colours. Each project stores a hue (OKLCH degrees) chosen when it was made,
 * so projects look different from each other; lightness and chroma come from the
 * stylesheet (--proj-l, --proj-c), which adjusts them for dark mode.
 */
const FALLBACK_HUES = [25, 230, 135, 310, 70, 185, 270, 350];

/** A project's hue: its stored colour, or (if it has none yet) one worked out from its folder name. */
export function projectHue(projects: { dir_name: string; hue: number | null }[], dirName: string): number {
  const stored = projects.find((p) => p.dir_name === dirName)?.hue;
  if (stored !== null && stored !== undefined) return stored;
  let h = 0;
  for (const c of dirName) h = (Math.imul(h, 31) + c.codePointAt(0)!) >>> 0;
  return FALLBACK_HUES[h % FALLBACK_HUES.length];
}

/** Inline style that gives an element (and its children) the project's hue as --hue. */
export function projectStyle(projects: { dir_name: string; hue: number | null }[], dirName: string): import("react").CSSProperties {
  return { ["--hue" as string]: projectHue(projects, dirName) } as import("react").CSSProperties;
}
