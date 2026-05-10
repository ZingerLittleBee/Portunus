import i18next from "i18next";
import { initReactI18next } from "react-i18next";

import en from "./en.json";
import zhCN from "./zh-CN.json";

export const SUPPORTED_LANGUAGES = ["en", "zh-CN"] as const;
export type Language = (typeof SUPPORTED_LANGUAGES)[number];

const LANG_KEY = "portunus.lang";

function readStoredLang(): Language | null {
  try {
    const v = window.localStorage.getItem(LANG_KEY);
    if (v && (SUPPORTED_LANGUAGES as readonly string[]).includes(v)) return v as Language;
  } catch {
    /* ignore */
  }
  return null;
}

function detectLang(): Language {
  const stored = readStoredLang();
  if (stored) return stored;
  const nav = (typeof navigator !== "undefined" ? navigator.languages : undefined) ?? [];
  for (const tag of nav) {
    if (tag.toLowerCase().startsWith("zh")) return "zh-CN";
  }
  return "en";
}

void i18next.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    "zh-CN": { translation: zhCN },
  },
  lng: detectLang(),
  fallbackLng: "en",
  interpolation: { escapeValue: false },
  returnNull: false,
});

export const i18n = i18next;

export function setLanguage(lang: Language): void {
  void i18next.changeLanguage(lang);
  try {
    window.localStorage.setItem(LANG_KEY, lang);
  } catch {
    /* ignore */
  }
}
