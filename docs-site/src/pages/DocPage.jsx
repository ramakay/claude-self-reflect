import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import rehypeRaw from 'rehype-raw'
import { Link } from 'react-router'
import { ChevronRight } from 'lucide-react'
import { navigation } from '../content'

function findAdjacent(currentPath) {
  const all = navigation.flatMap(g => g.items)
  const idx = all.findIndex(i => i.href === currentPath)
  return {
    prev: idx > 0 ? all[idx - 1] : null,
    next: idx < all.length - 1 ? all[idx + 1] : null,
  }
}

export default function DocPage({ title, description, content, path }) {
  const { prev, next } = findAdjacent(path)

  return (
    <article>
      {/* Breadcrumb */}
      <div className="flex items-center gap-1.5 text-sm" style={{ color: 'var(--color-muted)' }}>
        <Link to="/" className="hover:underline" style={{ color: 'var(--color-muted)' }}>Home</Link>
        <ChevronRight size={12} />
        <span style={{ color: 'var(--color-body)' }}>{title}</span>
      </div>

      {/* Header */}
      <h1 className="font-serif mt-6 mb-3" style={{ fontSize: 36, fontWeight: 700, color: 'var(--color-ink)' }}>{title}</h1>
      {description && <p className="mb-8 leading-relaxed" style={{ fontSize: 17, color: 'var(--color-muted)' }}>{description}</p>}

      {/* Content */}
      <div className="prose">
        <ReactMarkdown
          remarkPlugins={[remarkGfm]}
          rehypePlugins={[rehypeRaw]}
          components={{
            a: ({ href, children }) => {
              if (href?.startsWith('/claude-self-reflect/')) {
                const hashPath = href.replace('/claude-self-reflect/', '/docs/')
                return <Link to={hashPath} style={{ color: 'var(--color-purple)' }}>{children}</Link>
              }
              return <a href={href} target={href?.startsWith('http') ? '_blank' : undefined} rel="noopener noreferrer">{children}</a>
            },
          }}
        >
          {content}
        </ReactMarkdown>
      </div>

      {/* Navigation */}
      <div className="flex justify-between mt-16 pt-8" style={{ borderTop: '1px solid var(--color-rule)' }}>
        {prev ? (
          <Link to={prev.href} className="group flex flex-col items-start">
            <span className="text-xs mb-1" style={{ color: 'var(--color-muted)' }}>Previous</span>
            <span className="text-sm" style={{ color: 'var(--color-purple)' }}>&larr; {prev.label}</span>
          </Link>
        ) : <div />}
        {next ? (
          <Link to={next.href} className="group flex flex-col items-end">
            <span className="text-xs mb-1" style={{ color: 'var(--color-muted)' }}>Next</span>
            <span className="text-sm" style={{ color: 'var(--color-purple)' }}>{next.label} &rarr;</span>
          </Link>
        ) : <div />}
      </div>
    </article>
  )
}
