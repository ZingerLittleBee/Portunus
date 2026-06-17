import {
  createFileRoute,
  Link,
  notFound,
  redirect,
} from '@tanstack/react-router';
import { DocsLayout } from 'fumadocs-ui/layouts/docs';
import { createServerFn } from '@tanstack/react-start';
import { OLD_TO_NEW } from '@/lib/redirects';
import { slugsToMarkdownPath, source } from '@/lib/source';
import browserCollections from 'collections/browser';
import {
  DocsBody,
  DocsDescription,
  DocsPage,
  DocsTitle,
  MarkdownCopyButton,
  ViewOptionsPopover,
} from 'fumadocs-ui/layouts/docs/page';
import { baseOptions } from '@/lib/layout.shared';
import { gitConfig, siteUrl } from '@/lib/shared';
import { staticFunctionMiddleware } from '@tanstack/start-static-server-functions';
import { useFumadocsLoader } from 'fumadocs-core/source/client';
import { Suspense } from 'react';
import { useMDXComponents } from '@/components/mdx';

export const Route = createFileRoute('/$lang/docs/$')({
  component: Page,
  beforeLoad: ({ params }) => {
    const oldSlug = params._splat ?? '';
    // The docs root has no page of its own — land on the Overview section.
    if (oldSlug === '') {
      throw redirect({
        to: '/$lang/docs/$',
        params: { lang: params.lang, _splat: 'overview' },
        statusCode: 301,
      });
    }
    if (oldSlug in OLD_TO_NEW) {
      throw redirect({
        to: '/$lang/docs/$',
        params: { lang: params.lang, _splat: OLD_TO_NEW[oldSlug] },
        statusCode: 301,
      });
    }
  },
  loader: async ({ params }) => {
    const slugs = params._splat?.split('/') ?? [];
    const data = await loader({ data: { slugs, lang: params.lang } });
    await clientLoader.preload(data.path);
    return data;
  },
  head: ({ loaderData }) => {
    if (!loaderData?.url) return {};
    const canonical = `${siteUrl}${loaderData.url}`;
    return {
      links: [{ rel: 'canonical', href: canonical }],
      meta: [{ property: 'og:url', content: canonical }],
    };
  },
});

const loader = createServerFn({
  method: 'GET',
})
  .inputValidator((input: { slugs: string[]; lang: string }) => input)
  .middleware([staticFunctionMiddleware])
  .handler(async ({ data: { slugs, lang } }) => {
    const page = source.getPage(slugs, lang);
    if (!page) throw notFound();

    return {
      path: page.path,
      lang,
      url: page.url,
      markdownUrl: slugsToMarkdownPath(page.slugs, lang).url,
      pageTree: await source.serializePageTree(source.getPageTree(lang)),
    };
  });

const clientLoader = browserCollections.docs.createClientLoader({
  component(
    { toc, frontmatter, default: MDX },
    {
      markdownUrl,
      path,
    }: {
      markdownUrl: string;
      path: string;
    },
  ) {
    return (
      <DocsPage toc={toc}>
        <DocsTitle>{frontmatter.title}</DocsTitle>
        <DocsDescription>{frontmatter.description}</DocsDescription>
        <div className="flex flex-row gap-2 items-center border-b -mt-4 pb-6">
          <MarkdownCopyButton markdownUrl={markdownUrl} />
          <ViewOptionsPopover
            markdownUrl={markdownUrl}
            githubUrl={`https://github.com/${gitConfig.user}/${gitConfig.repo}/blob/${gitConfig.branch}/content/docs/${path}`}
          />
        </div>
        <DocsBody>
          <MDX components={useMDXComponents()} />
        </DocsBody>
      </DocsPage>
    );
  },
});

function Page() {
  const data = useFumadocsLoader(Route.useLoaderData());
  const { pageTree, path, markdownUrl, lang } = data;

  return (
    <DocsLayout {...baseOptions(lang)} tree={pageTree}>
      <Link to={markdownUrl} hidden />
      <Suspense>{clientLoader.useContent(path, { markdownUrl, path })}</Suspense>
    </DocsLayout>
  );
}
