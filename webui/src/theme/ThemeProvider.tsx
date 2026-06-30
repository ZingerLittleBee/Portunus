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

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [theme, setThemeState] = React.useState<ThemeChoice>(() => readStoredTheme());
  const [systemDark, setSystemDark] = React.useState(() => systemPrefersDark());
  const effective: EffectiveTheme =
    theme === "system" ? (systemDark ? "dark" : "light") : theme;

  React.useEffect(() => {
    document.documentElement.classList.toggle("dark", effective === "dark");
    writeStoredTheme(theme);
  }, [theme, effective]);

  React.useEffect(() => {
    const mql = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = (event: MediaQueryListEvent) => {
      setSystemDark(event.matches);
    };
    mql.addEventListener("change", handler);
    return () => mql.removeEventListener("change", handler);
  }, []);

  const value = React.useMemo(
    () => ({ theme, effective, setTheme: setThemeState }),
    [theme, effective],
  );

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

export function useTheme(): ThemeContextValue {
  const ctx = React.use(ThemeContext);
  if (!ctx) throw new Error("useTheme must be used inside <ThemeProvider>");
  return ctx;
}
