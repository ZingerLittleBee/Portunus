import { describe, expect, it } from "vitest";

import en from "@/i18n/en.json";
import zhCN from "@/i18n/zh-CN.json";

type Bag = Record<string, unknown>;

function flatten(obj: Bag, prefix = ""): string[] {
  const keys: string[] = [];
  for (const [k, v] of Object.entries(obj)) {
    const path = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === "object" && !Array.isArray(v)) {
      keys.push(...flatten(v as Bag, path));
    } else {
      keys.push(path);
    }
  }
  return keys.sort();
}

describe("i18n bundles", () => {
  it("en and zh-CN expose identical key sets", () => {
    const enKeys = flatten(en as Bag);
    const zhKeys = flatten(zhCN as Bag);
    const onlyInEn = enKeys.filter((k) => !zhKeys.includes(k));
    const onlyInZh = zhKeys.filter((k) => !enKeys.includes(k));
    expect(onlyInEn, "keys missing from zh-CN").toEqual([]);
    expect(onlyInZh, "keys missing from en").toEqual([]);
  });

  it("every leaf is a non-empty string", () => {
    for (const [name, bundle] of [
      ["en", en],
      ["zh-CN", zhCN],
    ] as const) {
      const stack: { obj: Bag; path: string }[] = [{ obj: bundle as Bag, path: "" }];
      while (stack.length > 0) {
        const { obj, path } = stack.pop()!;
        for (const [k, v] of Object.entries(obj)) {
          const next = path ? `${path}.${k}` : k;
          if (v && typeof v === "object" && !Array.isArray(v)) {
            stack.push({ obj: v as Bag, path: next });
          } else {
            expect(typeof v, `${name}:${next}`).toBe("string");
            expect((v as string).length, `${name}:${next} is empty`).toBeGreaterThan(0);
          }
        }
      }
    }
  });
});
