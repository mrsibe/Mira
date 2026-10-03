import type { Locale } from "../core/types";
import { en } from "./locales/en";
import { zh } from "./locales/zh";

export type TranslationDict = typeof en;

type PathsToStringProps<T> = T extends string
  ? []
  : { [K in keyof T]: [K, ...PathsToStringProps<T[K]>] }[keyof T];

type Join<T extends string[], D extends string> = T extends []
  ? never
  : T extends [infer F]
    ? F
    : T extends [infer F, ...infer R]
      ? F extends string
        ? `${F}${D}${Join<Extract<R, string[]>, D>}`
        : never
      : string;

export type TranslationKey = Join<PathsToStringProps<TranslationDict>, ".">;

const dictionaries: Record<Locale, TranslationDict> = { en, zh };

const storageKey = "mira.locale";

export function readStoredLocale(): Locale {
  if (typeof window === "undefined") {
    return "en";
  }
  const value = window.localStorage.getItem(storageKey);
  if (value === "en" || value === "zh") {
    return value;
  }
  return "en";
}

export function storeLocale(locale: Locale) {
  window.localStorage.setItem(storageKey, locale);
}

let currentLocale: Locale = readStoredLocale();

if (typeof document !== "undefined") {
  document.documentElement.lang = currentLocale;
}

export function getCurrentLocale(): Locale {
  return currentLocale;
}

export function setCurrentLocale(locale: Locale) {
  currentLocale = locale;
  if (typeof document !== "undefined") {
    document.documentElement.lang = locale;
  }
}

function getPath(obj: unknown, path: string): unknown {
  return path
    .split(".")
    .reduce<unknown>(
      (acc, key) => (acc as Record<string, unknown> | null)?.[key] ?? null,
      obj,
    );
}

function interpolate(
  value: string,
  params?: Record<string, string | number>,
): string {
  if (!params) {
    return value;
  }
  return value.replace(/\{\{(\w+)\}\}/g, (_, key: string) =>
    key in params ? String(params[key]) : `{{${key}}}`,
  );
}

export function t(
  key: TranslationKey,
  params?: Record<string, string | number>,
): string {
  const dict = dictionaries[currentLocale];
  const value = getPath(dict, key);
  if (typeof value !== "string") {
    return key;
  }
  return interpolate(value, params);
}
