import { invoke } from "@tauri-apps/api/core";
import type { Op, ProjectSettings, RateSource, RefreshReport, Snapshot, UserFunction } from "./types";

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
  createSheet: (name: string, template: "blank" | "spending") => invoke<Snapshot>("create_sheet", { name, template }),
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
  importFile: (path: string) => invoke<Snapshot>("import_file", { path }),
  exportSheet: (sheetId: string, path: string, format: ExportFormat) => invoke<void>("export_sheet", { sheetId, path, format }),
};

export const newId = (): string => crypto.randomUUID().replace(/-/g, "");
