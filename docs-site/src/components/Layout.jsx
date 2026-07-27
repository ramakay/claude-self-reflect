import { useState, useEffect, useRef, useMemo, useCallback } from 'react'
import { Outlet, Link, useLocation, useNavigate } from 'react-router'
import { Github, ChevronDown, ChevronRight, Menu, X, ArrowLeft, Sun, Moon, Search } from 'lucide-react'
import { navigation, pages } from '../content'

function SidebarGroup({ group, location }) {
  const active = group.items.some(i => location.pathname === i.href)
  const [open, setOpen] = useState(active)
  return (
    <div className="mb-[4px]">
      <button type="button" onClick={() => setOpen(!open)} aria-expanded={open} aria-label={`${group.label} section`}
        className="flex items-center w-full px-[12px] py-[6px] type-dateline hover:text-[var(--color-ink)] rounded-lg hover:bg-white/30 transition-colors">
        <span className="flex-1 text-left">{group.label}</span>
        {open ? <ChevronDown size={11} aria-hidden="true" /> : <ChevronRight size={11} aria-hidden="true" />}
      </button>
      {open && (
        <div className="ml-[12px] mt-[2px] border-l border-[var(--color-rule)] pl-[12px] space-y-[2px]">
          {group.items.map(item => (
            <Link key={item.href} to={item.href}
              className={`block px-[12px] py-[5px] text-[13px] rounded-lg transition-colors ${
                location.pathname === item.href
                  ? 'text-[var(--color-purple)] font-medium bg-[rgba(107,91,149,0.06)]'
                  : 'text-[var(--color-body)] hover:text-[var(--color-ink)] hover:bg-white/20'
              }`}>
              {item.label}
            </Link>
          ))}
        </div>
      )}
    </div>
  )
}

// Build search index from all page content
function buildIndex() {
  return Object.entries(pages).map(([path, page]) => {
    // Extract headings from markdown
    const headings = []
    const lines = page.content.split('\n')
    for (const line of lines) {
      const m = line.match(/^(#{1,3})\s+(.+)/)
      if (m) headings.push(m[2])
    }
    // Plain text for matching (strip markdown syntax)
    const plain = page.content
      .replace(/```[\s\S]*?```/g, '')
      .replace(/[#*`\[\]()>|_~-]/g, ' ')
      .replace(/\s+/g, ' ')
      .toLowerCase()
    return { path, title: page.title, description: page.description || '', headings, plain }
  })
}

function SearchDialog({ open, onClose }) {
  const [query, setQuery] = useState('')
  const inputRef = useRef(null)
  const navigate = useNavigate()
  const index = useMemo(buildIndex, [])

  useEffect(() => {
    if (open) {
      setQuery('')
      setTimeout(() => inputRef.current?.focus(), 50)
    }
  }, [open])

  const results = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (q.length < 2) return []

    const terms = q.split(/\s+/)
    const scored = index.map(entry => {
      let score = 0
      for (const term of terms) {
        if (entry.title.toLowerCase().includes(term)) score += 10
        if (entry.description.toLowerCase().includes(term)) score += 5
        for (const h of entry.headings) {
          if (h.toLowerCase().includes(term)) { score += 3; break }
        }
        if (entry.plain.includes(term)) score += 1
      }
      // Find snippet
      let snippet = ''
      if (score > 0) {
        const idx = entry.plain.indexOf(terms[0])
        if (idx >= 0) {
          const start = Math.max(0, idx - 40)
          const end = Math.min(entry.plain.length, idx + 80)
          snippet = (start > 0 ? '...' : '') + entry.plain.slice(start, end).trim() + (end < entry.plain.length ? '...' : '')
        }
      }
      return { ...entry, score, snippet }
    })

    return scored.filter(r => r.score > 0).sort((a, b) => b.score - a.score).slice(0, 8)
  }, [query, index])

  const go = useCallback((path) => {
    navigate(path)
    onClose()
  }, [navigate, onClose])

  useEffect(() => {
    if (!open) return
    function onKey(e) {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open, onClose])

  // Focus trap: Tab cycles within dialog
  const dialogRef = useRef(null)
  const onDialogKey = useCallback((e) => {
    if (e.key !== 'Tab' || !dialogRef.current) return
    const focusable = dialogRef.current.querySelectorAll('input, button, [tabindex]:not([tabindex="-1"])')
    if (!focusable.length) return
    const first = focusable[0], last = focusable[focusable.length - 1]
    if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus() }
    else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus() }
  }, [])

  if (!open) return null

  return (
    <div className="search-overlay" onClick={onClose} role="presentation">
      <div ref={dialogRef} className="search-dialog" role="dialog" aria-modal="true" aria-label="Search documentation" onClick={e => e.stopPropagation()} onKeyDown={onDialogKey}>
        <div className="search-input-row">
          <Search size={16} style={{ color: 'var(--color-muted)', flexShrink: 0 }} aria-hidden="true" />
          <input
            ref={inputRef}
            className="search-input"
            type="text"
            placeholder="Search docs..."
            aria-label="Search documentation"
            value={query}
            onChange={e => setQuery(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter' && results.length) go(results[0].path) }}
          />
          <kbd className="search-kbd">esc</kbd>
        </div>
        <div aria-live="polite" aria-atomic="true" className="sr-only">
          {query.length >= 2 ? (results.length ? `${results.length} results` : `No results for ${query}`) : ''}
        </div>
        {results.length > 0 && (
          <div className="search-results" role="listbox">
            {results.map(r => (
              <button key={r.path} className="search-result" role="option" onClick={() => go(r.path)}>
                <strong>{r.title}</strong>
                {r.snippet && <span>{r.snippet}</span>}
              </button>
            ))}
          </div>
        )}
        {query.length >= 2 && results.length === 0 && (
          <div className="search-empty">No results for "{query}"</div>
        )}
      </div>
    </div>
  )
}

function getTheme() {
  const saved = localStorage.getItem('csr-theme')
  if (saved === 'light' || saved === 'dark') return saved
  return typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}

export default function Layout() {
  const location = useLocation()
  const [sidebarOpen, setSidebarOpen] = useState(false)
  const [searchOpen, setSearchOpen] = useState(false)
  const [theme, setTheme] = useState(getTheme)
  useEffect(() => { document.documentElement.dataset.theme = theme }, [theme])

  // Cmd+K or / to open search
  useEffect(() => {
    function onKey(e) {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') { e.preventDefault(); setSearchOpen(true) }
      if (e.key === '/' && !['INPUT', 'TEXTAREA'].includes(e.target.tagName)) { e.preventDefault(); setSearchOpen(true) }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  return (
    <div className="docs-shell">
      <SearchDialog open={searchOpen} onClose={() => setSearchOpen(false)} />

      {/* Nav — matches landing */}
      <header className="site-nav docs-nav">
        <div className="site-nav__inner">
          <button type="button" onClick={() => setSidebarOpen(!sidebarOpen)} aria-expanded={sidebarOpen} aria-label="Toggle navigation menu" className="lg:hidden mr-3 text-[var(--color-muted)] hover:text-[var(--color-ink)]">
            {sidebarOpen ? <X size={20} aria-hidden="true" /> : <Menu size={20} aria-hidden="true" />}
          </button>
          <Link className="site-nav__brand" to="/">
            <img src="/claude-self-reflect/favicon.svg" alt="" width={28} height={28} />
            <span>
              <span className="site-nav__name" style={{ fontSize: 18 }}>Claude Self-Reflect</span>
              <span className="site-nav__tag">Documentation</span>
            </span>
          </Link>
          <nav className="site-nav__links" aria-label="Primary">
            <button className="search-trigger" onClick={() => setSearchOpen(true)} aria-label="Search docs">
              <Search size={14} />
              <span>Search</span>
              <kbd>/</kbd>
            </button>
            <Link className="site-nav__link" to="/docs/why-csr">Guide</Link>
            <Link className="site-nav__link" to="/docs/mcp-tools">Reference</Link>
            <Link className="site-nav__link" to="/docs/architecture">Architecture</Link>
            <button className="theme-toggle" onClick={() => { const n = theme === 'dark' ? 'light' : 'dark'; localStorage.setItem('csr-theme', n); setTheme(n) }} aria-label="Toggle theme">
              {theme === 'dark' ? <Sun size={17} /> : <Moon size={17} />}
            </button>
            <a className="site-nav__github" href="https://github.com/ramakay/claude-self-reflect" target="_blank" rel="noopener">
              <Github size={16} /> <span>GitHub</span>
            </a>
          </nav>
        </div>
      </header>

      <div className="flex">
        {/* Sidebar — glassmorphic */}
        <aside className={`fixed lg:sticky top-[72px] left-0 z-40 w-[220px] h-[calc(100vh-72px)] overflow-y-auto p-[16px] transition-transform lg:translate-x-0
          ${sidebarOpen ? 'translate-x-0' : '-translate-x-full'}
          bg-[#e8e5f0] border-r border-[rgba(0,0,0,0.06)] dark:bg-[#0e1225] dark:border-[rgba(255,255,255,0.08)]`}>
          <button onClick={() => setSearchOpen(true)} className="search-sidebar-btn">
            <Search size={13} />
            <span>Search</span>
            <kbd>⌘K</kbd>
          </button>
          <Link to="/" className="flex items-center gap-[6px] px-[12px] py-[6px] mb-[12px] type-caption hover:text-[var(--color-ink)] rounded-lg hover:bg-white/20">
            <ArrowLeft size={11} /> Home
          </Link>
          {navigation.map(g => <SidebarGroup key={g.label} group={g} location={location} />)}
        </aside>

        {/* Overlay */}
        {sidebarOpen && <div className="fixed inset-0 z-30 bg-black/10 lg:hidden" onClick={() => setSidebarOpen(false)} />}

        {/* Content */}
        <main className="flex-1 min-w-0 px-[24px] lg:px-[48px] py-[36px] max-w-[720px] mx-auto">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
