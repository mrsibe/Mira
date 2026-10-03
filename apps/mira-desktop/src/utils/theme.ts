import type { ThemeMode } from "../core/types";

const storageKey = "mira.theme";

export function readStoredThemeMode(): ThemeMode {
  if (typeof window === "undefined") {
    return "system";
  }
  const value = window.localStorage.getItem(storageKey);
  if (value === "light" || value === "dark" || value === "system") {
    return value;
  }
  return "system";
}

export function storeThemeMode(mode: ThemeMode) {
  window.localStorage.setItem(storageKey, mode);
}

export function resolveThemeMode(mode: ThemeMode): "light" | "dark" {
  if (mode !== "system") {
    return mode;
  }
  return window.matchMedia("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

export function applyThemeMode(mode: ThemeMode) {
  const resolved = resolveThemeMode(mode);
  document.documentElement.dataset.theme = resolved;
  document.documentElement.dataset.themeMode = mode;
  document.documentElement.style.colorScheme = resolved;
}
