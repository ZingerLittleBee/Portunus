import defaultMdxComponents from 'fumadocs-ui/mdx';
import type { MDXComponents } from 'mdx/types';
import type { ComponentProps, ComponentType } from 'react';

const DefaultAnchor = defaultMdxComponents.a as ComponentType<
  ComponentProps<'a'>
>;

/**
 * Render a native `<a>` for fragment links so the browser performs standard
 * hash scrolling. fumadocs' default `a` routes through TanStack Router's
 * `<Link to={href}>`, which does not understand `to="#hash"` (the hash must be
 * passed via a separate `hash` prop) and silently drops the fragment — turning
 * every in-page anchor into a no-op same-page navigation. This applies to both
 * same-page links (`#id`) and internal links that carry a fragment
 * (`/path#id`). All other links keep the SPA-aware fumadocs Link.
 */
function Anchor({ href = '', ...props }: ComponentProps<'a'>) {
  const hasFragment = href.startsWith('#') || (href.startsWith('/') && href.includes('#'));
  if (hasFragment) {
    return <a href={href} {...props} />;
  }
  return <DefaultAnchor href={href} {...props} />;
}

export function getMDXComponents(components?: MDXComponents) {
  return {
    ...defaultMdxComponents,
    a: Anchor,
    ...components,
  } satisfies MDXComponents;
}

export const useMDXComponents = getMDXComponents;

declare global {
  type MDXProvidedComponents = ReturnType<typeof getMDXComponents>;
}
