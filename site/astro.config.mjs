import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLinksValidator from 'starlight-links-validator';
import rslibTheme from 'starlight-theme-rslib';
import commandGroups from './src/generated/sidebar.json' with { type: 'json' };

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
      plugins: [rslibTheme(), starlightLinksValidator()],
      sidebar: [
        { label: 'Start', items: [{ label: 'rgit', slug: '' }] },
        { label: 'Guides', items: [{ autogenerate: { directory: 'guides' } }] },
        {
          label: 'Reference',
          items: [
            { label: 'Commands overview', slug: 'reference/commands' },
            ...commandGroups.map((group) => ({ label: group.label, collapsed: true, items: group.items })),
            { label: 'Global options', slug: 'reference/commands/global-options' },
            { label: 'TUI keys', slug: 'reference/tui-keys' },
            { label: 'Configuration', slug: 'reference/configuration' },
          ],
        },
      ],
    }),
  ],
  // npm links the theme from .theme, so Vite may serve its fonts from there.
  vite: { server: { fs: { allow: ['.', './.theme/starlight'] } } },
});
