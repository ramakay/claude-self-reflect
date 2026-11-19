// Navigation structure
export const navigation = [
  {
    label: 'Getting Started',
    icon: 'Book',
    items: [
      { label: 'Why CSR?', href: '/docs/why-csr' },
      { label: 'Installation', href: '/docs/installation' },
      { label: 'Quick Start', href: '/docs/quickstart' },
    ],
  },
  {
    label: 'How It Works',
    icon: 'Zap',
    items: [
      { label: 'Architecture', href: '/docs/architecture' },
      { label: 'Active Memory Hooks', href: '/docs/hooks' },
      { label: 'Search & Retrieval', href: '/docs/search' },
      { label: 'Enrichment Pipeline', href: '/docs/enrichment' },
    ],
  },
  {
    label: 'Reference',
    icon: 'Wrench',
    items: [
      { label: 'MCP Tools', href: '/docs/mcp-tools' },
      { label: 'CLI Reference', href: '/docs/cli' },
      { label: 'Configuration', href: '/docs/configuration' },
    ],
  },
  {
    label: 'Guides',
    icon: 'Compass',
    items: [
      { label: 'Upgrading to v8', href: '/docs/upgrading' },
      { label: 'Search Strategies', href: '/docs/search-strategies' },
      { label: 'Privacy & Security', href: '/docs/privacy' },
      { label: 'Troubleshooting', href: '/docs/troubleshooting' },
    ],
  },
  {
    label: 'Contributing',
    icon: 'Users',
    items: [
      { label: 'Development Setup', href: '/docs/dev-setup' },
      { label: 'Project Structure', href: '/docs/structure' },
    ],
  },
]

// Import markdown content - strip frontmatter
function stripFrontmatter(md) {
  return md.replace(/^---[\s\S]*?---\n*/, '')
}

// We'll inline the content here for simplicity and build speed
// Each page gets: title, description, content (markdown string), path

import whyCsr from './why-csr.md?raw'
import installation from './installation.md?raw'
import quickstart from './quickstart.md?raw'
import architecture from './architecture.md?raw'
import hooks from './hooks.md?raw'
import search from './search.md?raw'
import enrichment from './enrichment.md?raw'
import mcpTools from './mcp-tools.md?raw'
import cli from './cli.md?raw'
import configuration from './configuration.md?raw'
import upgrading from './upgrading.md?raw'
import searchStrategies from './search-strategies.md?raw'
import privacy from './privacy.md?raw'
import troubleshooting from './troubleshooting.md?raw'
import devSetup from './dev-setup.md?raw'
import structure from './structure.md?raw'

export const pages = {
  '/docs/why-csr': { title: 'Why Claude Self-Reflect?', description: 'What makes CSR different from other Claude Code memory tools.', content: stripFrontmatter(whyCsr), path: '/docs/why-csr' },
  '/docs/installation': { title: 'Installation', description: 'Install CSR in under a minute. One command, zero dependencies.', content: stripFrontmatter(installation), path: '/docs/installation' },
  '/docs/quickstart': { title: 'Quick Start', description: 'Your first search in 60 seconds.', content: stripFrontmatter(quickstart), path: '/docs/quickstart' },
  '/docs/architecture': { title: 'Architecture', description: 'How CSR works — single binary, five components, zero services.', content: stripFrontmatter(architecture), path: '/docs/architecture' },
  '/docs/hooks': { title: 'Active Memory Hooks', description: 'Six real-time hooks that inject context exactly when Claude needs it.', content: stripFrontmatter(hooks), path: '/docs/hooks' },
  '/docs/search': { title: 'Search & Retrieval', description: 'How CSR finds relevant context — hybrid search, decay scoring, cross-project.', content: stripFrontmatter(search), path: '/docs/search' },
  '/docs/enrichment': { title: 'Enrichment Pipeline', description: 'Three layers of progressive enrichment.', content: stripFrontmatter(enrichment), path: '/docs/enrichment' },
  '/docs/mcp-tools': { title: 'MCP Tools Reference', description: 'All 12 MCP tools with parameters, examples, and return formats.', content: stripFrontmatter(mcpTools), path: '/docs/mcp-tools' },
  '/docs/cli': { title: 'CLI Reference', description: 'Every csr-engine command and flag.', content: stripFrontmatter(cli), path: '/docs/cli' },
  '/docs/configuration': { title: 'Configuration', description: 'Environment variables, settings, and customization.', content: stripFrontmatter(configuration), path: '/docs/configuration' },
  '/docs/upgrading': { title: 'Upgrading to v8', description: 'Migrate from v7.x to the single Rust binary.', content: stripFrontmatter(upgrading), path: '/docs/upgrading' },
  '/docs/search-strategies': { title: 'Search Strategies', description: 'Get the best results from CSR\'s 12 search tools.', content: stripFrontmatter(searchStrategies), path: '/docs/search-strategies' },
  '/docs/privacy': { title: 'Privacy & Security', description: 'Everything runs locally by default.', content: stripFrontmatter(privacy), path: '/docs/privacy' },
  '/docs/troubleshooting': { title: 'Troubleshooting', description: 'Solutions compiled from 160+ resolved GitHub issues.', content: stripFrontmatter(troubleshooting), path: '/docs/troubleshooting' },
  '/docs/dev-setup': { title: 'Development Setup', description: 'Get started contributing to CSR.', content: stripFrontmatter(devSetup), path: '/docs/dev-setup' },
  '/docs/structure': { title: 'Project Structure', description: 'A tour of the csr-engine codebase.', content: stripFrontmatter(structure), path: '/docs/structure' },
}
