import { invoke } from "@tauri-apps/api/core";
import type { Op, ProjectSettings, RateSource, RefreshReport, Snapshot } from "./types";

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
  checkFormula: (formula: string) => invoke<string | null>("check_formula", { formula }),
  saveSettings: (settings: ProjectSettings, sheetId: string | null) =>
    invoke<Snapshot>("save_settings", { settings, sheetId }),
  refreshRates: () => invoke<RefreshReport[]>("refresh_rates"),
  importRates: (source: RateSource, path: string) => invoke<number>("import_rates", { source, path }),
  exportXlsx: (sheetId: string, path: string) => invoke<void>("export_xlsx", { sheetId, path }),
};

export const newId = (): string => crypto.randomUUID().replace(/-/g, "");
