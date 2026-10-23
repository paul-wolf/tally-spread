// Mirrors the JSON produced by tally-core and the Tauri commands.

export type RateSource = "ecb" | "nbu" | "hmrc_monthly" | "manual";
export type RateMode = { mode: "live" } | { mode: "locked"; date: string };
export type RowKind = "item" | "section";

export type ColumnKind =
  | { type: "text" }
  | { type: "link" }
  | { type: "number" }
  | { type: "money"; currency: string }
  | { type: "date" }
  | { type: "datetime" }
  | { type: "calc"; formula: string; currency?: string | null };

/** name = display name ("Price (UAH)"); var = variable name ("price_uah"; empty = derive). */
/** How a column looks (format, not type). */
export interface ColumnStyle {
  bold?: boolean;
}

export type ColumnSpec = { name: string; var: string; total: boolean; style?: ColumnStyle } & ColumnKind;

export interface FixedRate {
  base: string;
  quote: string;
  rate: string;
}

export type Op =
  | { op: "create_sheet"; sheet_id: string; name: string }
  | { op: "rename_sheet"; sheet_id: string; name: string }
  | { op: "set_sheet_archived"; sheet_id: string; archived: boolean }
  | { op: "set_sheet_rates"; sheet_id: string; rates: RateMode }
  | { op: "set_sheet_sources"; sheet_id: string; sources: RateSource[] }
  | { op: "set_sheet_fixed_rates"; sheet_id: string; rates: FixedRate[] }
  | { op: "set_sheet_base"; sheet_id: string; base: string | null }
  | { op: "add_column"; sheet_id: string; col_id: string; position: number; spec: ColumnSpec }
  | { op: "update_column"; sheet_id: string; col_id: string; spec: ColumnSpec }
  | { op: "move_column"; sheet_id: string; col_id: string; position: number }
  | { op: "set_column_deleted"; sheet_id: string; col_id: string; deleted: boolean }
  | { op: "insert_row"; sheet_id: string; row_id: string; position: number }
  | { op: "set_row_kind"; sheet_id: string; row_id: string; kind: RowKind }
  | { op: "set_row_deleted"; sheet_id: string; row_id: string; deleted: boolean }
  | { op: "set_cell"; sheet_id: string; row_id: string; col_id: string; value: string | null }
  | { op: "batch"; ops: Op[] };

export type ColumnView = {
  /** Invariant ID: c1, c2, … */
  col_id: string;
  position: number;
  /** Display name. */
  name: string;
  /** Variable name for formulas and SQL. */
  var: string;
  /** Calculated columns: the formula written with variable names. */
  formula_display: string | null;
  total: boolean;
  style: ColumnStyle;
  error: string | null;
} & ColumnKind;

export interface CellView {
  raw: string | null;
  value: string | null;
  currency: string | null;
  error: string | null;
  /** Not an error, but probably a mistake (e.g. a price in two currencies). */
  warning?: string | null;
}

export interface RowView {
  row_id: string;
  position: number;
  kind: RowKind;
  cells: CellView[];
}

export interface TotalView {
  amount: string;
  currency: string | null;
  complete: boolean;
}

export interface RateLeg {
  from: string;
  to: string;
  rate: string;
  inverted: boolean;
  source: RateSource;
  date: string;
}

export interface CardEntry {
  currency: string;
  /** 1 base = rate currency */
  rate: string | null;
  pinned: boolean;
  legs: RateLeg[];
  rate_date: string | null;
  fetched_at: string | null;
  error: string | null;
}

export interface RateCard {
  base: string;
  base_chosen: boolean;
  as_of: string;
  entries: CardEntry[];
}

export interface UserFunction {
  name: string;
  params: string[];
  body: string;
  description: string;
}

export interface SheetView {
  sheet_id: string;
  name: string;
  rates: RateMode;
  as_of: string;
  sources: RateSource[];
  effective_sources: RateSource[];
  fixed_rates: FixedRate[];
  rate_card: RateCard;
  columns: ColumnView[];
  rows: RowView[];
  totals: (TotalView | null)[];
  rates_used: RateLeg[];
  warnings: string[];
}

export interface SheetInfo {
  sheet_id: string;
  name: string;
  archived: boolean;
  rates: RateMode;
  sources: RateSource[];
  fixed_rates: FixedRate[];
  base: string | null;
}

export interface ProjectSettings {
  format: number;
  name: string;
  reporting_currency: string;
  rate_sources: RateSource[];
  rounding: "half_up" | "half_even";
  rounding_scope: "per_line" | "on_total";
  tax_home: string | null;
  notes: string;
}

export interface Snapshot {
  home: string;
  machine: string;
  projects: { dir_name: string; name: string }[];
  project: string;
  settings: ProjectSettings;
  sheets: SheetInfo[];
  sheet: SheetView | null;
  can_undo: boolean;
  can_redo: boolean;
  rate_status: { source: RateSource; label: string; latest: string | null }[];
  functions: UserFunction[];
  notices: string[];
}

export interface RefreshReport {
  source: RateSource;
  ok: boolean;
  added: number;
  message: string;
}

export const SOURCE_LABEL: Record<RateSource, string> = {
  ecb: "ECB",
  nbu: "NBU",
  hmrc_monthly: "HMRC",
  manual: "Manual",
};
