// Mirrors the JSON produced by tally-core and the Tauri commands.

export type RateSource = "ecb" | "nbu" | "hmrc_monthly" | "manual";
export type RateMode = { mode: "live" } | { mode: "locked"; date: string };
export type RowKind = "item" | "section";

export type ColumnKind =
  | { type: "text" }
  | { type: "link" }
  | { type: "number" }
  | { type: "money"; currency: string }
  | { type: "calc"; formula: string };

export type ColumnSpec = { name: string; total: boolean } & ColumnKind;

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
  col_id: string;
  letter: string;
  position: number;
  name: string;
  total: boolean;
  error: string | null;
} & ColumnKind;

export interface CellView {
  raw: string | null;
  value: string | null;
  currency: string | null;
  error: string | null;
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

export interface SheetView {
  sheet_id: string;
  name: string;
  rates: RateMode;
  as_of: string;
  sources: RateSource[];
  effective_sources: RateSource[];
  fixed_rates: FixedRate[];
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
