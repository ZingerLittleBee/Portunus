import * as React from "react";

export type ThemeChoice = "light" | "dark" | "system";
type EffectiveTheme = "light" | "dark";

const THEME_KEY = "portunus.theme";

interface ThemeContextValue {
  theme: ThemeChoice;
  effective: EffectiveTheme;
  setTheme: (t: ThemeChoice) => void;
}

const ThemeContext = React.createContext<ThemeContextValue | null>(null);

function readStoredTheme(): ThemeChoice {
  try {
    const v = window.localStorage.getItem(THEME_KEY);
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch {
    /* ignore */
  }
  return "system";
}

function writeStoredTheme(value: ThemeChoice) {
  try {
    window.localStorage.setItem(THEME_KEY, value);
  } catch {
    /* ignore */
  }
}

function systemPrefersDark(): boolean {
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false;
}

function resolve(theme: ThemeChoice): EffectiveTheme {
  if (theme === "system") return systemPrefersDark() ? "dark" : "light";
  return theme;
}

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [theme, setThemeState] = React.useState<ThemeChoice>(() => readStoredTheme());
  const [effective, setEffective] = React.useState<EffectiveTheme>(() => resolve(readStoredTheme()));

  React.useEffect(() => {
    const next = resolve(theme);
    setEffective(next);
    document.documentElement.classList.toggle("dark", next === "dark");
    writeStoredTheme(theme);
  }, [theme]);

  React.useEffect(() => {
    if (theme !== "system") return;
    const mql = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => {
      const next = resolve("system");
      setEffective(next);
      document.documentElement.classList.toggle("dark", next === "dark");
    };
    mql.addEventListener("change", handler);
    return () => mql.removeEventListener("change", handler);
  }, [theme]);

  const value = React.useMemo(
    () => ({ theme, effective, setTheme: setThemeState }),
    [theme, effective],
  );

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

export function useTheme(): ThemeContextValue {
  const ctx = React.useContext(ThemeContext);
  if (!ctx) throw new Error("useTheme must be used inside <ThemeProvider>");
  return ctx;
}
