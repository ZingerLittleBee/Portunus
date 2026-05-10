import {
  createRootRoute,
  HeadContent,
  Outlet,
  Scripts,
  useParams,
} from "@tanstack/react-router";
import { RootProvider } from "fumadocs-ui/provider/tanstack";
import SearchDialog from "@/components/search";
import { i18nUI } from "@/lib/layout.shared";
import appCss from "@/styles/app.css?url";

export const Route = createRootRoute({
  head: () => ({
    meta: [
      {
        charSet: "utf-8",
      },
      {
        name: "viewport",
        content: "width=device-width, initial-scale=1",
      },
      {
        title: "Portunus Docs",
      },
    ],
    links: [
      { rel: "stylesheet", href: appCss },
      { rel: "icon", href: "/favicon.svg", type: "image/svg+xml" },
      { rel: "shortcut icon", href: "/favicon.svg", type: "image/svg+xml" },
      { rel: "mask-icon", href: "/waypoints.svg", color: "#c8f36f" },
    ],
  }),
  component: RootComponent,
});

function RootComponent() {
  const params = useParams({ strict: false }) as { lang?: string };
  const lang = params.lang ?? "en";

  return (
    <html suppressHydrationWarning lang={lang}>
      <head>
        <HeadContent />
      </head>
      <body className="flex flex-col min-h-screen">
        <RootProvider search={{ SearchDialog }} i18n={i18nUI.provider(lang)}>
          <Outlet />
        </RootProvider>
        <Scripts />
      </body>
    </html>
  );
}
