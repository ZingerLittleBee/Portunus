import * as React from "react";

import { ThemeContext, type ThemeContextValue } from "@/theme/theme-context";

export function useTheme(): ThemeContextValue {
  const ctx = React.use(ThemeContext);
  if (!ctx) throw new Error("useTheme must be used inside <ThemeProvider>");
  return ctx;
}
