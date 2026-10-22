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
