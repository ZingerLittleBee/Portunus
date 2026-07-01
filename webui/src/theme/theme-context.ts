import * as React from "react";

export type ThemeChoice = "light" | "dark" | "system";
export type EffectiveTheme = "light" | "dark";

export interface ThemeContextValue {
  theme: ThemeChoice;
  effective: EffectiveTheme;
  setTheme: (t: ThemeChoice) => void;
}

export const ThemeContext = React.createContext<ThemeContextValue | null>(null);
