import { afterEach, describe, expect, it } from "vitest";
import {
  getCurrentLocale,
  readStoredLocale,
  setCurrentLocale,
  t,
  type TranslationKey,
} from "../../src/i18n";

describe("i18n", () => {
  afterEach(() => {
    window.localStorage.clear();
    setCurrentLocale("en");
  });

  it("reads the stored locale, ignoring unknown values", () => {
    expect(readStoredLocale()).toBe("en");

    window.localStorage.setItem("mira.locale", "zh");
    expect(readStoredLocale()).toBe("zh");

    window.localStorage.setItem("mira.locale", "fr");
    expect(readStoredLocale()).toBe("en");
  });

  it("translates keys using the active locale", () => {
    setCurrentLocale("en");
    expect(getCurrentLocale()).toBe("en");
    expect(t("composer.placeholder")).toBe("Ask anything");

    setCurrentLocale("zh");
    expect(getCurrentLocale()).toBe("zh");
    expect(t("composer.placeholder")).toBe("询问任何问题");
  });

  it("interpolates parameters", () => {
    setCurrentLocale("en");
    expect(t("errors.sendFailed", { error: "boom" })).toBe("Send failed: boom");
  });

  it("returns the key when a translation is missing", () => {
    const missingKey = "does.not.exist" as unknown as TranslationKey;
    expect(t(missingKey)).toBe("does.not.exist");
  });
});
