import { invoke } from "@tauri-apps/api/core";
import type { Distribution, Groups, Op, ProjectSettings, RateSource, RefreshReport, Series, SheetEntry, Snapshot, UserFunction } from "./types";

export type ExportFormat = "xlsx" | "pdf" | "csv" | "json";

// Thin typed wrappers over the Tauri commands in src-tauri/src/lib.rs.
export const api = {
  snapshot: (sheetId: string | null) => invoke<Snapshot>("snapshot", { sheetId }),
  apply: (op: Op, sheetId: string | null) => invoke<Snapshot>("apply", { op, sheetId }),
  undo: (sheetId: string | null) => invoke<Snapshot>("undo", { sheetId }),
  redo: (sheetId: string | null) => invoke<Snapshot>("redo", { sheetId }),
  sync: (sheetId: string | null) => invoke<Snapshot | null>("sync", { sheetId }),
  openProject: (dirName: string) => invoke<Snapshot>("open_project", { dirName }),
  createProject: (name: string) => invoke<Snapshot>("create_project", { name }),
  /** A new sheet in Unfiled, or in `project` when given. */
  newSheet: (template: "blank" | "spending", project: string | null = null) => invoke<Snapshot>("new_sheet", { template, project }),
  sheetIndex: (includeArchived: boolean) => invoke<SheetEntry[]>("sheet_index", { includeArchived }),
  restoreSheet: (project: string, sheetId: string) => invoke<Snapshot>("restore_sheet", { project, sheetId }),
  pinTab: (project: string, sheetId: string, pinned: boolean, current: string | null) =>
    invoke<Snapshot>("pin_tab", { project, sheetId, pinned, current }),
  newSheetFromText: (text: string) => invoke<Snapshot>("new_sheet_from_text", { text }),
  openSheet: (project: string, sheetId: string) => invoke<Snapshot>("open_sheet", { project, sheetId }),
  closeTab: (project: string, sheetId: string, current: string | null) => invoke<Snapshot>("close_tab", { project, sheetId, current }),
  /** Move to an existing project (`to`), or to a new one named `newProject`. */
  moveSheet: (project: string, sheetId: string, to: string | null, newProject: string | null = null) =>
    invoke<Snapshot>("move_sheet", { project, sheetId, to, newProject }),
  previewFormula: (sheetId: string, colId: string | null, formula: string, currency: string | null) =>
    invoke<string[]>("preview_formula", { sheetId, colId, formula, currency }),
  saveSettings: (settings: ProjectSettings, sheetId: string | null) =>
    invoke<Snapshot>("save_settings", { settings, sheetId }),
  refreshRates: () => invoke<RefreshReport[]>("refresh_rates"),
  importRates: (source: RateSource, path: string) => invoke<number>("import_rates", { source, path }),
  saveFunction: (fn: UserFunction, oldName: string | null, sheetId: string | null) =>
    invoke<Snapshot>("save_function", { function: fn, oldName, sheetId }),
  deleteFunction: (name: string, sheetId: string | null) => invoke<Snapshot>("delete_function", { name, sheetId }),
  tryFunction: (fn: UserFunction, expression: string) => invoke<string>("try_function", { function: fn, expression }),
  exportXlsx: (sheetId: string, path: string) => invoke<void>("export_xlsx", { sheetId, path }),
  analyzeSeries: (sheetId: string, x: string | null, cols: string[], maxPoints: number) =>
    invoke<Series>("analyze_series", { sheetId, x, cols, maxPoints }),
  analyzeDistribution: (sheetId: string, col: string) => invoke<Distribution>("analyze_distribution", { sheetId, col }),
  analyzeGroups: (sheetId: string, by: string, cols: string[], bucket: number | null) =>
    invoke<Groups>("analyze_groups", { sheetId, by, cols, bucket }),
  saveGroupsSheet: (sheetId: string, by: string, cols: string[], bucket: number | null, name: string) =>
    invoke<Snapshot>("save_groups_sheet", { sheetId, by, cols, bucket, name }),
  writeFile: (path: string, bytes: number[]) => invoke<void>("write_file", { path, bytes }),
  importFile: (path: string) => invoke<Snapshot>("import_file", { path }),
  exportSheet: (sheetId: string, path: string, format: ExportFormat) => invoke<void>("export_sheet", { sheetId, path, format }),
};

export const newId = (): string => crypto.randomUUID().replace(/-/g, "");
