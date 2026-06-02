import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Agentation } from "agentation";
import { BrowserRouter } from "react-router-dom";
import { I18nextProvider } from "react-i18next";

import { ThemeProvider } from "@/theme/ThemeProvider";
import { i18n } from "@/i18n";
import { App } from "@/App";
import { Toaster } from "@/components/ui/sonner";
import { initScrollbarAutoHide } from "@/lib/scrollbar-autohide";

import "@/theme/tokens.css";

// Reveal scrollbars only while actively scrolling (see scrollbar-autohide.ts).
initScrollbarAutoHide();

// Dev-only: react-grab overlay for inspecting React component sources.
if (import.meta.env.DEV) {
  void import("react-grab");
}

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      refetchOnWindowFocus: false,
      staleTime: 5_000,
    },
  },
});

const root = document.getElementById("root");
if (!root) throw new Error("#root mount point not found");

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <I18nextProvider i18n={i18n}>
          <BrowserRouter>
            <App />
            {import.meta.env.DEV ? <Agentation /> : null}
            <Toaster richColors closeButton position="top-right" />
          </BrowserRouter>
        </I18nextProvider>
      </ThemeProvider>
    </QueryClientProvider>
  </React.StrictMode>,
);
