import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLinksValidator from 'starlight-links-validator';

export default defineConfig({
  site: 'https://rslib.github.io',
  base: '/rgit',
  integrations: [
    starlight({
      title: 'rgit',
      description:
        'A git client for people and coding agents: a magit-style TUI, a git-compatible CLI and an MCP server.',
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/rslib/rgit' }],
      plugins: [starlightLinksValidator()],
      sidebar: [
        { label: 'Start', items: [{ label: 'rgit', slug: '' }] },
        { label: 'Guides', items: [{ autogenerate: { directory: 'guides' } }] },
        {
          label: 'Reference',
          items: [
            { label: 'Commands', slug: 'reference/commands' },
            { label: 'Configuration', slug: 'reference/configuration' },
          ],
        },
      ],
    }),
  ],
});
