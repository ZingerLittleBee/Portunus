import type { ComponentProps } from "react";

export type RechartsModule = typeof import("recharts");
export type RechartsResponsiveContainerProps = ComponentProps<
  RechartsModule["ResponsiveContainer"]
>;
export type RechartsTooltipProps = ComponentProps<RechartsModule["Tooltip"]>;
export type RechartsLegendProps = ComponentProps<RechartsModule["Legend"]>;

let rechartsModule: RechartsModule | undefined;
let rechartsError: unknown;

const rechartsPromise = import("recharts").then(
  (module) => {
    rechartsModule = module;
    return module;
  },
  (error: unknown) => {
    rechartsError = error;
    throw error;
  },
);

export function preloadRecharts() {
  return rechartsPromise;
}

export function useRecharts(): RechartsModule {
  if (rechartsModule) {
    return rechartsModule;
  }
  if (rechartsError) {
    throw rechartsError;
  }
  throw rechartsPromise;
}
