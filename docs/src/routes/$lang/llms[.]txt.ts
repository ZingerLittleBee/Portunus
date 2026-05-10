import { source } from '@/lib/source';
import { createFileRoute } from '@tanstack/react-router';
import { llms } from 'fumadocs-core/source';

export const Route = createFileRoute('/$lang/llms.txt')({
  server: {
    handlers: {
      GET({ params }) {
        return new Response(llms(source, { language: params.lang }).index());
      },
    },
  },
});
