import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The UI lives in ui/; Tauri loads the built files from dist/.
export default defineConfig({
  root: "ui",
  plugins: [react()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    target: "es2021",
  },
});
