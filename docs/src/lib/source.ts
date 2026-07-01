import { docs } from "collections/server";
import { loader } from "fumadocs-core/source";
import { lucideIconsPlugin } from "fumadocs-core/source/lucide-icons";
import { i18n } from "./i18n";
import { docsRoute, siteUrl } from "./shared";

export const source = loader({
  source: docs.toFumadocsSource(),
  baseUrl: docsRoute,
  plugins: [lucideIconsPlugin()],
  i18n,
});

export function markdownPathToSlugs(segs: string[]) {
  if (segs.length === 0) return [];

  const out = [...segs];
  out[out.length - 1] = out[out.length - 1].replace(/\.md$/, "");
  if (out.length === 1 && out[0] === "index") out.pop();
  return out;
}

export function slugsToMarkdownPath(slugs: string[], lang?: string) {
  const segments = [...slugs];
  if (segments.length === 0) {
    segments.push("index.md");
  } else {
    segments[segments.length - 1] += ".md";
  }

  const prefix = lang ? `/${lang}${docsRoute}` : docsRoute;
  return {
    segments,
    url: `${prefix}/${segments.join("/")}`,
  };
}

/** Prefix a root-relative app path with the production origin. */
function absoluteUrl(path: string) {
  return path.startsWith("http") ? path : `${siteUrl}${path}`;
}

/** Rewrite root-relative markdown links (`](/...)`) to absolute production URLs. */
export function absolutizeMarkdownLinks(text: string) {
  return text.replace(
    /\]\((\/[^)]+)\)/g,
    (_m, p: string) => `](${siteUrl}${p})`,
  );
}

export async function getLLMText(page: (typeof source)["$inferPage"]) {
  const processed = await page.data.getText("processed");

  return `# ${page.data.title} (${absoluteUrl(page.url)})

${processed}`;
}
