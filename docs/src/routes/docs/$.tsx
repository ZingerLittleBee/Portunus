import { createFileRoute, redirect } from '@tanstack/react-router';

// A docs URL without a language prefix (e.g. /docs/getting-started/architecture)
// defaults to English and redirects to its /en/docs equivalent.
export const Route = createFileRoute('/docs/$')({
  beforeLoad: ({ params }) => {
    throw redirect({
      to: '/$lang/docs/$',
      params: { lang: 'en', _splat: params._splat ?? '' },
    });
  },
});
