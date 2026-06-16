import defaultMdxComponents from 'fumadocs-ui/mdx';
import { Accordion, Accordions } from 'fumadocs-ui/components/accordion';
import { Tab, Tabs } from 'fumadocs-ui/components/tabs';
import { buttonVariants } from 'fumadocs-ui/components/ui/button';
import { useCopyButton } from 'fumadocs-ui/utils/use-copy-button';
import { CopyCheckIcon, LinkIcon } from 'lucide-react';
import type { MDXComponents } from 'mdx/types';
import type { ComponentProps, ComponentType, ElementType } from 'react';
import { cn } from '@/lib/cn';
import { siteUrl } from '@/lib/shared';

const DefaultAnchor = defaultMdxComponents.a as ComponentType<
  ComponentProps<'a'>
>;

type HeadingTag = 'h1' | 'h2' | 'h3' | 'h4' | 'h5' | 'h6';

/**
 * Heading with an anchor + copy-link button, mirroring fumadocs' default but
 * copying the absolute production URL (`siteUrl + pathname + #id`) instead of
 * `window.location.href`. This keeps copied anchors pointing at the canonical
 * docs domain even when the page is viewed on localhost or a preview host.
 */
function Heading({ as, ...props }: { as: HeadingTag } & ComponentProps<'h1'>) {
  const As = as as ElementType;
  const [isChecked, onCopy] = useCopyButton(() => {
    if (!props.id) return;
    const path = typeof window !== 'undefined' ? window.location.pathname : '';
    return navigator.clipboard.writeText(`${siteUrl}${path}#${props.id}`);
  });

  if (!props.id) return <As {...props} />;

  const { className, children, ...rest } = props;
  return (
    <As
      {...rest}
      className={cn(
        'group/heading flex scroll-m-28 flex-row items-center gap-1',
        className,
      )}
    >
      <a data-card="" href={`#${props.id}`}>
        {children}
      </a>
      <button
        type="button"
        aria-label="Copy Anchor Link"
        className={cn(
          buttonVariants({ variant: 'ghost', size: 'icon-xs' }),
          'not-prose shrink-0 text-fd-muted-foreground opacity-0 transition-opacity group-hover/heading:opacity-100',
        )}
        onClick={onCopy}
      >
        {isChecked ? <CopyCheckIcon /> : <LinkIcon />}
      </button>
    </As>
  );
}

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
    Accordion,
    Accordions,
    Tab,
    Tabs,
    a: Anchor,
    h1: (p: ComponentProps<'h1'>) => <Heading as="h1" {...p} />,
    h2: (p: ComponentProps<'h2'>) => <Heading as="h2" {...p} />,
    h3: (p: ComponentProps<'h3'>) => <Heading as="h3" {...p} />,
    h4: (p: ComponentProps<'h4'>) => <Heading as="h4" {...p} />,
    h5: (p: ComponentProps<'h5'>) => <Heading as="h5" {...p} />,
    h6: (p: ComponentProps<'h6'>) => <Heading as="h6" {...p} />,
    ...components,
  } satisfies MDXComponents;
}

export const useMDXComponents = getMDXComponents;

declare global {
  type MDXProvidedComponents = ReturnType<typeof getMDXComponents>;
}
