import { defineConfig } from 'vitepress'
import { tabsMarkdownPlugin } from 'vitepress-plugin-tabs'
import { mathjaxPlugin } from './mathjax-plugin'
import { juliaReplTransformer } from './julia-repl-transformer'
import footnote from "markdown-it-footnote";
import path from 'path'
import fs from 'fs'

const mathjax = mathjaxPlugin()

// The pixel size of every PNG under assets/ (from its IHDR header), as the
// virtual module `virtual:sot-media-sizes`, keyed like media.ts ("media/x.png",
// "readme/x.png", "x.png"). DemoShot and DemoLoop give each image its width
// and height, so the page reserves the space before the image loads.
function mediaSizes() {
  const id = 'virtual:sot-media-sizes'
  const resolved = '\0' + id
  return {
    name: 'sot-media-sizes',
    resolveId: (s: string) => (s === id ? resolved : undefined),
    load(s: string) {
      if (s !== resolved) return
      const root = path.resolve(__dirname, '../assets')
      const sizes: Record<string, [number, number]> = {}
      for (const dir of ['media', 'readme', '.']) {
        const abs = path.join(root, dir)
        if (!fs.existsSync(abs)) continue
        for (const f of fs.readdirSync(abs)) {
          if (!f.endsWith('.png')) continue
          const head = Buffer.alloc(24)
          const fd = fs.openSync(path.join(abs, f), 'r')
          fs.readSync(fd, head, 0, 24, 0)
          fs.closeSync(fd)
          sizes[path.posix.join(dir, f)] = [head.readUInt32BE(16), head.readUInt32BE(20)]
        }
      }
      return `export default ${JSON.stringify(sizes)}`
    },
  }
}

function getBaseRepository(base: string): string {
  if (!base || base === '/') return '/';
  const parts = base.split('/').filter(Boolean);
  return parts.length > 0 ? `/${parts[0]}/` : '/';
}

const baseTemp = {
  base: 'REPLACE_ME_DOCUMENTER_VITEPRESS',// TODO: replace this in makedocs!
}

const navTemp = {
  nav: 'REPLACE_ME_DOCUMENTER_VITEPRESS',
}

// The generated nav (one dropdown per top-level page group) is replaced by a
// short hand-written one; GitHub is the social link and search sits beside it.
// No version picker: the site publishes a single dev version.
void navTemp
const nav = [
  { text: 'Get started', link: '/start/quickstart' },
  { text: 'Guides', link: '/guide/agents' },
  { text: 'Reference', link: '/ref/keybindings' },
]

const sidebarTemp = {
  sidebar: 'REPLACE_ME_DOCUMENTER_VITEPRESS',
}

// DocumenterVitepress emits every group expanded; collapse them all. VitePress
// still opens the group that holds the current page.
function collapseGroups(items: any): any {
  if (!Array.isArray(items)) return items
  return items.map((it: any) =>
    it && Array.isArray(it.items) ? { ...it, collapsed: true, items: collapseGroups(it.items) } : it)
}

// https://vitepress.dev/reference/site-config
export default defineConfig({
  base: 'REPLACE_ME_DOCUMENTER_VITEPRESS',// TODO: replace this in makedocs!
  title: 'REPLACE_ME_DOCUMENTER_VITEPRESS',
  description: 'REPLACE_ME_DOCUMENTER_VITEPRESS',
  lastUpdated: true,
  cleanUrls: true,
  // Dark-first: the app itself is a dark native window, so the site opens dark.
  appearance: 'dark',
  outDir: 'REPLACE_ME_DOCUMENTER_VITEPRESS', // This is required for MarkdownVitepress to work correctly...
  head: [
    ['link', { rel: 'icon', href: 'REPLACE_ME_DOCUMENTER_VITEPRESS_FAVICON' }],
    ['script', {src: `${getBaseRepository(baseTemp.base)}versions.js`}],
    // ['script', {src: '/versions.js'], for custom domains, I guess if deploy_url is available.
    ['script', {src: `${baseTemp.base}siteinfo.js`}],
    // REPLACE_ME_DOCUMENTER_VITEPRESS_NOINDEX
  ],
  
  markdown: {
    codeTransformers: [juliaReplTransformer()],
    config(md) {
      md.use(tabsMarkdownPlugin);
      md.use(footnote);
      mathjax.markdownConfig(md);
    },
    theme: {
      light: "github-light",
      dark: "github-dark"
    },
  },
  vite: {
    plugins: [
      mathjax.vitePlugin,
      mediaSizes(),
    ],
    define: {
      __DEPLOY_ABSPATH__: JSON.stringify('REPLACE_ME_DOCUMENTER_VITEPRESS_DEPLOY_ABSPATH'),
    },
    resolve: {
      alias: {
        '@': path.resolve(__dirname, '../components')
      }
    },
    optimizeDeps: {
      exclude: [ 
        '@nolebase/vitepress-plugin-enhanced-readabilities/client',
        'vitepress',
        '@nolebase/ui',
      ], 
    }, 
    ssr: { 
      noExternal: [ 
        // If there are other packages that need to be processed by Vite, you can add them here.
        '@nolebase/vitepress-plugin-enhanced-readabilities',
        '@nolebase/ui',
      ], 
    },
  },
  themeConfig: {
    outline: 'deep',
    // The helm mark, light/dark variants (docs/src/assets/logo*.svg; any
    // asset named *logo* is copied into public/ by DocumenterVitepress).
    logo: { light: '/logo.svg', dark: '/logo-dark.svg', alt: 'Ship of Tools' },
    siteTitle: 'Ship of Tools',
    search: {
      provider: 'local',
      options: {
        detailedView: true
      }
    },
    nav,
    sidebar: collapseGroups(sidebarTemp.sidebar),
    sidebarDrawer: 'REPLACE_ME_DOCUMENTER_VITEPRESS_SIDEBAR_DRAWER',
    editLink: 'REPLACE_ME_DOCUMENTER_VITEPRESS',
    socialLinks: [
      { icon: 'github', link: 'REPLACE_ME_DOCUMENTER_VITEPRESS' }
    ],
    footer: {
      message: 'An agentic development environment for Julia. Released under the AGPL-3.0-or-later, with a commercial license on request.',
      copyright: 'Built with <a href="https://luxdl.github.io/DocumenterVitepress.jl/dev/" target="_blank">DocumenterVitepress.jl</a>'
    }
  }
})
