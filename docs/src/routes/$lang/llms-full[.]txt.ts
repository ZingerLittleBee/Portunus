import { createFileRoute } from '@tanstack/react-router';
import { source, getLLMText } from '@/lib/source';

export const Route = createFileRoute('/$lang/llms-full.txt')({
  server: {
    handlers: {
      GET: async ({ params }) => {
        const pages = source.getPages(params.lang);
        const scanned = await Promise.all(pages.map(getLLMText));
        return new Response(scanned.join('\n\n'));
      },
    },
  },
});
