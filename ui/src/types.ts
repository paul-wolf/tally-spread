// Mirrors the JSON produced by tally-core and the Tauri commands.

export type Column = "item" | "url" | "qty" | "unit_price" | "currency" | "note";
export type RateSource = "ecb" | "nbu" | "hmrc_monthly" | "manual";
export type RateMode = { mode: "live" } | { mode: "locked"; date: string };

export type Op =
  | { op: "create_sheet"; sheet_id: string; name: string }
  | { op: "rename_sheet"; sheet_id: string; name: string }
  | { op: "set_sheet_archived"; sheet_id: string; archived: boolean }
  | { op: "set_sheet_currencies"; sheet_id: string; currencies: string[] }
  | { op: "set_sheet_rates"; sheet_id: string; rates: RateMode }
  | { op: "insert_row"; sheet_id: string; row_id: string; position: number }
  | { op: "set_row_deleted"; sheet_id: string; row_id: string; deleted: boolean }
  | { op: "set_cell"; sheet_id: string; row_id: string; column: Column; value: string | null }
  | { op: "batch"; ops: Op[] };

export interface RowView {
  row_id: string;
  position: number;
  item: string | null;
  url: string | null;
  qty: string | null;
  unit_price: string | null;
  currency: string | null;
  note: string | null;
  line_total: string | null;
  converted: (string | null)[];
  problem: string | null;
}

export interface TotalView {
  currency: string;
  amount: string;
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

export interface CostListView {
  sheet_id: string;
  name: string;
  currencies: string[];
  rates: RateMode;
  as_of: string;
  rows: RowView[];
  totals: TotalView[];
  subtotals: TotalView[];
  rates_used: RateLeg[];
  warnings: string[];
}

export interface SheetInfo {
  sheet_id: string;
  name: string;
  archived: boolean;
  currencies: string[];
  rates: RateMode;
}

export interface ProjectSettings {
  name: string;
  reporting_currency: string;
  display_currencies: string[];
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
  sheet: CostListView | null;
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
