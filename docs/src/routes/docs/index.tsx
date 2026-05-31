import { createFileRoute, redirect } from '@tanstack/react-router';

// Bare /docs (no language prefix) defaults to English.
export const Route = createFileRoute('/docs/')({
  beforeLoad: () => {
    throw redirect({
      to: '/$lang/docs/$',
      params: { lang: 'en', _splat: '' },
    });
  },
});
