// Navigation structure
export const navigation = [
  {
    label: 'Getting Started',
    icon: 'Book',
    items: [
      { label: 'Why Self-Reflect?', href: '/docs/why-csr' },
      { label: 'Features', href: '/docs/features' },
      { label: 'Installation', href: '/docs/installation' },
      { label: 'Quick Start', href: '/docs/quickstart' },
      { label: 'Choose your view', href: '/docs/companion-views' },
      { label: 'What\'s new in 10.1', href: '/docs/whats-new' },
    ],
  },
  {
    label: 'How It Works',
    icon: 'Zap',
    items: [
      { label: 'Architecture', href: '/docs/architecture' },
      { label: 'How the companion thinks', href: '/docs/how-the-companion-thinks' },
      { label: 'Session hooks', href: '/docs/hooks' },
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
      { label: 'Upgrading', href: '/docs/upgrading' },
      { label: 'Search Strategies', href: '/docs/search-strategies' },
      { label: 'Privacy & Security', href: '/docs/privacy' },
      { label: 'btw, these too', href: '/docs/btw-these-too' },
      { label: 'Troubleshooting', href: '/docs/troubleshooting' },
      { label: 'What we\'re working on', href: '/docs/report-card' },
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
import features from './features.md?raw'
import reportCard from './report-card.md?raw'
import installation from './installation.md?raw'
import quickstart from './quickstart.md?raw'
import companionViews from './companion-views.md?raw'
import howTheCompanionThinks from './how-the-companion-thinks.md?raw'
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
import btwTheseToo from './btw-these-too.md?raw'
import whatsNew from './whats-new.md?raw'

export const pages = {
  '/docs/why-csr': { title: 'Why Self-Reflect?', description: 'Claude reflecting on its own work, on top of a local index of your sessions.', content: stripFrontmatter(whyCsr), path: '/docs/why-csr' },
  '/docs/features': { title: 'Features', description: 'What Self-Reflect does in 10.1: sources, recap, dreams, receipts, code map, companion pane.', content: stripFrontmatter(features), path: '/docs/features' },
  '/docs/report-card': { title: 'What we\'re working on', description: 'Seven known gaps in 10.1.0, with measured numbers, targets and the work under way.', content: stripFrontmatter(reportCard), path: '/docs/report-card' },
  '/docs/installation': { title: 'Installation', description: 'Install CSR in under a minute. One command, zero dependencies.', content: stripFrontmatter(installation), path: '/docs/installation' },
  '/docs/quickstart': { title: 'Quick Start', description: 'Your first search in 60 seconds.', content: stripFrontmatter(quickstart), path: '/docs/quickstart' },
  '/docs/companion-views': { title: 'Choose your view: herdr pane or in-Claude panel', description: 'Where the companion shows, how to set up each view, how to switch, and how to turn it off.', content: stripFrontmatter(companionViews), path: '/docs/companion-views' },
  '/docs/whats-new': { title: 'What\'s new in 10.1', description: 'The 10.1.0 release in short: recap, dreaming, more sources, code map, companion.', content: stripFrontmatter(whatsNew), path: '/docs/whats-new' },
  '/docs/architecture': { title: 'Architecture', description: 'How CSR works — single binary, five components, zero services.', content: stripFrontmatter(architecture), path: '/docs/architecture' },
  '/docs/how-the-companion-thinks': { title: 'How the companion thinks', description: 'What starts a dream look, what the model sees, what is checked, what it costs, and how to turn it off.', content: stripFrontmatter(howTheCompanionThinks), path: '/docs/how-the-companion-thinks' },
  '/docs/hooks': { title: 'Session hooks', description: 'Eight hooks on seven events: they inject context when Claude needs it and capture the rest.', content: stripFrontmatter(hooks), path: '/docs/hooks' },
  '/docs/search': { title: 'Search & Retrieval', description: 'How CSR finds relevant context: hybrid search, cross-encoder re-ranking, project scope.', content: stripFrontmatter(search), path: '/docs/search' },
  '/docs/enrichment': { title: 'Enrichment Pipeline', description: 'Three layers of progressive enrichment.', content: stripFrontmatter(enrichment), path: '/docs/enrichment' },
  '/docs/mcp-tools': { title: 'MCP Tools Reference', description: 'All 16 MCP tools with parameters, examples, and return formats.', content: stripFrontmatter(mcpTools), path: '/docs/mcp-tools' },
  '/docs/cli': { title: 'CLI Reference', description: 'Every csr-engine command and flag.', content: stripFrontmatter(cli), path: '/docs/cli' },
  '/docs/configuration': { title: 'Configuration', description: 'Environment variables, settings, and customization.', content: stripFrontmatter(configuration), path: '/docs/configuration' },
  '/docs/upgrading': { title: 'Upgrading', description: 'Upgrade from 9.5.8, within 10.x, or from the v7.x Python stack.', content: stripFrontmatter(upgrading), path: '/docs/upgrading' },
  '/docs/search-strategies': { title: 'Search Strategies', description: 'Get the best results from CSR\'s search tools.', content: stripFrontmatter(searchStrategies), path: '/docs/search-strategies' },
  '/docs/privacy': { title: 'Privacy & Security', description: 'Everything runs locally by default.', content: stripFrontmatter(privacy), path: '/docs/privacy' },
  '/docs/btw-these-too': { title: 'btw, these too', description: 'Smaller features that ship with this release.', content: stripFrontmatter(btwTheseToo), path: '/docs/btw-these-too' },
  '/docs/troubleshooting': { title: 'Troubleshooting', description: 'Solutions compiled from 160+ resolved GitHub issues.', content: stripFrontmatter(troubleshooting), path: '/docs/troubleshooting' },
  '/docs/dev-setup': { title: 'Development Setup', description: 'Get started contributing to CSR.', content: stripFrontmatter(devSetup), path: '/docs/dev-setup' },
  '/docs/structure': { title: 'Project Structure', description: 'A tour of the csr-engine codebase.', content: stripFrontmatter(structure), path: '/docs/structure' },
}
