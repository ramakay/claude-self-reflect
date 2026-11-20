import { Link, useOutletContext } from 'react-router'
import { ArrowRight, ChevronRight, Search } from 'lucide-react'
import { versions, LANDING_URL } from '../content/versions'

const PAPER_URL = 'https://github.com/ramakay/claude-self-reflect/blob/main/docs/plans/annaswamy-2026-similarity-drowns-intent.pdf'

// "Start here" cards, in order; a version without one of these pages falls back to the next slug.
const START_SLUGS = ['/docs/installation', '/docs/quickstart', '/docs/companion-views', '/docs/why-csr']

/** The docs index: what the docs app shows at its root. The marketing landing lives at LANDING_URL. */
export default function DocsHome({ version = 'current' }) {
  const v = versions[version]
  const { openSearch } = useOutletContext() ?? {}
  const page = href => v.pages[href]
  const start = START_SLUGS.map(s => page(`${v.prefix}${s}`)).filter(Boolean).slice(0, 3)
  const whatsNew = page(`${v.prefix}/docs/whats-new`)
  // Pages already shown as a card or the what's-new callout are not listed again
  const shown = new Set([...start, whatsNew].filter(Boolean).map(p => p.path))
  const groups = v.navigation
    .map(g => ({ ...g, items: g.items.filter(i => !shown.has(i.href)) }))
    .filter(g => g.items.length)

  return (
    <article className="docs-home">
      <div className="flex items-center gap-1.5 text-sm" style={{ color: 'var(--color-muted)' }}>
        <a href={LANDING_URL} className="hover:underline" style={{ color: 'var(--color-muted)' }}>Home</a>
        <ChevronRight size={12} aria-hidden="true" />
        <span style={{ color: 'var(--color-body)' }}>Docs</span>
      </div>

      {version !== 'current' && (
        <div className="version-banner" role="note">
          You are reading the 9.x docs. <Link to="/">10.1 is current &rarr;</Link>
        </div>
      )}

      <h1 className="docs-home__title">{version === 'current' ? 'Self-Reflect docs' : 'Self-Reflect 9.x docs'}</h1>
      <p className="docs-home__intro">
        How to install Self-Reflect, how it works, and every tool, command and setting.
      </p>

      <button type="button" className="docs-home__search" onClick={() => openSearch?.()} aria-label="Search docs">
        <Search size={18} aria-hidden="true" />
        <span>Search the docs</span>
        <kbd>/</kbd>
      </button>

      <section aria-labelledby="start-here">
        <h2 id="start-here" className="docs-home__h2">Start here</h2>
        <div className="docs-home__cards">
          {start.map(p => (
            <Link key={p.path} to={p.path} className="docs-home__card">
              <strong>{v.navigation.flatMap(g => g.items).find(i => i.href === p.path)?.label ?? p.title}</strong>
              <span>{p.description}</span>
              <ArrowRight size={15} aria-hidden="true" className="docs-home__card-arrow" />
            </Link>
          ))}
        </div>
      </section>

      {whatsNew && (
        <Link to={whatsNew.path} className="docs-home__news">
          <span className="docs-home__news-kicker">What's new in 10.1</span>
          <span>{whatsNew.description}</span>
          <ArrowRight size={15} aria-hidden="true" />
        </Link>
      )}

      <div className="docs-home__groups">
        {groups.map(g => (
          <section key={g.label} aria-labelledby={`grp-${g.label}`}>
            <h2 id={`grp-${g.label}`} className="docs-home__group">{g.label}</h2>
            <ul>
              {g.items.map(i => (
                <li key={i.href}>
                  <Link to={i.href}>{i.label}</Link>
                  {page(i.href)?.description && <span>{page(i.href).description}</span>}
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>

      <p className="docs-home__paper">
        Research behind it: <a href={PAPER_URL} target="_blank" rel="noopener noreferrer">Similarity Drowns Intent (paper, PDF) &rarr;</a>
      </p>
    </article>
  )
}
