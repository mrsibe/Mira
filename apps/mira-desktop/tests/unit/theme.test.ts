import { afterEach, describe, expect, it, vi } from "vitest";
import {
  applyThemeMode,
  readStoredThemeMode,
  resolveThemeMode,
  storeThemeMode,
} from "../../src/utils/theme";

function stubPrefersDark(matches: boolean) {
  window.matchMedia = vi.fn((query: string) => ({
    matches,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

describe("theme", () => {
  afterEach(() => {
    window.localStorage.clear();
    delete document.documentElement.dataset.theme;
    delete document.documentElement.dataset.themeMode;
    document.documentElement.style.colorScheme = "";
  });

  it("reads the stored theme mode, ignoring unknown values", () => {
    expect(readStoredThemeMode()).toBe("system");

    storeThemeMode("dark");
    expect(readStoredThemeMode()).toBe("dark");

    window.localStorage.setItem("mira.theme", "sepia");
    expect(readStoredThemeMode()).toBe("system");
  });

  it("resolves explicit and system modes", () => {
    expect(resolveThemeMode("light")).toBe("light");
    expect(resolveThemeMode("dark")).toBe("dark");

    stubPrefersDark(true);
    expect(resolveThemeMode("system")).toBe("dark");

    stubPrefersDark(false);
    expect(resolveThemeMode("system")).toBe("light");
  });

  it("applies the resolved theme to the document element", () => {
    stubPrefersDark(false);
    applyThemeMode("system");
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(document.documentElement.dataset.themeMode).toBe("system");

    applyThemeMode("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.dataset.themeMode).toBe("dark");
    expect(document.documentElement.style.colorScheme).toBe("dark");
  });
});
