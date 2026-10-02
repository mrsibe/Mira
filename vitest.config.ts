import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Unit / integration tests for the frontend. These run against the real React
// store and client modules; the Tauri IPC boundary itself is replaced with
// mocks (see tests/unit/*.test.ts) so no native process is involved.
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    include: ["tests/unit/**/*.test.{ts,tsx}"],
    setupFiles: ["tests/setup.ts"],
    restoreMocks: true,
  },
});
