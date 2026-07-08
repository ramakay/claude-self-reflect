import { Link } from 'react-router-dom'
import { useEffect, useRef, useState } from 'react'
import { Check, Copy, Github, Moon, Sun } from 'lucide-react'

function useDelayedReveal(baseDelay = 0) {
  const [ready, setReady] = useState(false)

  useEffect(() => {
    const t = setTimeout(() => setReady(true), 1800 + baseDelay)
    return () => clearTimeout(t)
  }, [baseDelay])

  return ready
}

function BentoCard({ children, className = '', delay = 0, id }) {
  const ready = useDelayedReveal(delay)

  return (
    <article id={id} className={`bento-card${ready ? ' is-visible' : ''} ${className}`} style={{ animationDelay: `${delay}ms` }}>
      {children}
    </article>
  )
}

function CardKicker({ number, label, accent = 'purple' }) {
  return (
    <div className="card-kicker">
      <span className={`section-num section-number section-num--${accent}`}>{number}</span>
      <span className="card-kicker__rule" />
      {label ? <span className="type-dateline">{label}</span> : null}
    </div>
  )
}

function CopyBtn({ text }) {
  const [copied, setCopied] = useState(false)

  async function copyText() {
    await navigator.clipboard.writeText(text)
    setCopied(true)
    window.setTimeout(() => setCopied(false), 1600)
  }

  return (
    <>
      <button className="install-copy" type="button" onClick={copyText} aria-label="Copy install command">
        {copied ? <Check size={15} aria-hidden="true" /> : <Copy size={15} aria-hidden="true" />}
      </button>
      <span className="sr-only" aria-live="polite">{copied ? 'Install command copied' : ''}</span>
    </>
  )
}

function PostIt({ children, className = '', bg = 'var(--color-amber-paper)', rot = '-2deg' }) {
  return (
    <div className={`post-it ${className}`} style={{ background: bg, '--rot': rot, '--from-rot': '-4deg' }}>
      {children}
    </div>
  )
}

function ForgettingChart() {
  const points = [
    [48, 32],
    [68, 62],
    [90, 80],
    [112, 93],
    [136, 108],
    [162, 124],
    [186, 136],
    [212, 148],
    [236, 157],
  ]
  const d = points.map(([x, y], i) => `${i === 0 ? 'M' : 'L'}${x} ${y}`).join(' ')
  const area = `${d} L236 168 L48 168 Z`

  return (
    <figure className="forgetting-chart">
      <figcaption className="chart-title">Context Lost Per Session</figcaption>
      <svg viewBox="0 0 268 190" role="img" aria-label="Context retention declines from session 1 through session 20">
        {[32, 66, 100, 134, 168].map((y) => (
          <line key={y} x1="48" x2="236" y1={y} y2={y} className="chart-grid" />
        ))}
        <line x1="48" x2="48" y1="28" y2="168" className="chart-axis" />
        <line x1="48" x2="238" y1="168" y2="168" className="chart-axis" />
        <path d={area} fill="rgba(181,131,141,0.11)" />
        <path className="chart-path" pathLength="1" d={d} fill="none" stroke="var(--color-rose)" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" />
        {points.map(([x, y], i) => (
          <circle key={`${x}-${y}`} cx={x} cy={y} r={i % 2 === 0 ? 2.8 : 2.2} fill="var(--color-rose)" opacity={i % 2 === 0 ? 0.82 : 0.5} />
        ))}
        <text x="38" y="36" textAnchor="end" className="chart-label">100</text>
        <text x="38" y="104" textAnchor="end" className="chart-label">50</text>
        <text x="38" y="172" textAnchor="end" className="chart-label">0</text>
        {[
          ['1', 48],
          ['5', 92],
          ['10', 140],
          ['15', 188],
          ['20', 236],
        ].map(([label, x]) => (
          <text key={label} x={x} y="181" textAnchor="middle" className="chart-label">{label}</text>
        ))}
      </svg>
      <p className="type-caption chart-caption">Average context retention drops below 20% after 10 sessions.</p>
    </figure>
  )
}

function HookTimeline() {
  const nodes = [
    ['Start', 28, '#6b5b95'],
    ['Prompt', 100, '#7c9473'],
    ['ToolUse', 172, '#6b5b95'],
    ['Stop', 244, '#7c9473'],
    ['Compact', 316, '#6b5b95'],
    ['End', 388, '#7c9473'],
  ]

  return (
    <svg className="hook-timeline" viewBox="0 0 418 92" role="img" aria-label="Six active memory lifecycle hooks">
      <line x1="28" x2="388" y1="38" y2="38" stroke="rgba(107,91,149,0.32)" strokeWidth="2" />
      {nodes.map(([label, x, color], i) => (
        <g key={`${label}-${i}`} className="hook-node">
          <circle cx={x} cy="38" r="15" fill={color} opacity="0.12" />
          <circle className="hook-node__halo" cx={x} cy="38" r="20" fill="none" stroke={color} opacity="0.12" />
          <circle className="hook-node__dot" cx={x} cy="38" r="5.8" fill={color} style={{ animationDelay: `${260 + i * 90}ms` }} />
          <text x={x} y="76" textAnchor="middle" className="chart-label">{label}</text>
        </g>
      ))}
    </svg>
  )
}

function SearchLatencyBars() {
  const bars = [12, 18, 8, 15, 22, 10, 6, 14, 20, 11, 25, 16]

  return (
    <figure className="search-bars">
      <svg viewBox="0 0 220 132" role="img" aria-label="Latency distribution from zero to twenty five milliseconds">
        {[16, 58, 100].map((y) => (
          <line key={y} x1="34" x2="206" y1={y} y2={y} className="chart-grid" />
        ))}
        <line x1="34" x2="34" y1="16" y2="100" className="chart-axis" />
        <line x1="34" x2="208" y1="100" y2="100" className="chart-axis" />
        {bars.map((v, i) => {
          const h = (v / 25) * 82
          const x = 42 + i * 13
          return (
            <rect
              key={`${v}-${i}`}
              className="bar-vert"
              x={x}
              y={100 - h}
              width="8"
              height={h}
              rx="2"
              fill={i === 10 ? 'var(--color-rose)' : 'var(--color-purple)'}
              opacity={i === 10 ? 0.78 : 0.62}
              style={{ animationDelay: `${420 + i * 25}ms` }}
            />
          )
        })}
        <text x="25" y="104" textAnchor="end" className="chart-label">0ms</text>
        <text x="25" y="62" textAnchor="end" className="chart-label">12</text>
        <text x="25" y="20" textAnchor="end" className="chart-label">25</text>
        <text x="34" y="123" textAnchor="middle" className="chart-label">0ms</text>
        <text x="120" y="123" textAnchor="middle" className="chart-label">12ms</text>
        <text x="206" y="123" textAnchor="middle" className="chart-label">25ms</text>
      </svg>
    </figure>
  )
}

function ImportBatchChart() {
  const bars = [48, 64, 52, 70, 40, 50, 74, 58, 66]

  return (
    <svg className="import-batches" viewBox="0 0 312 116" role="img" aria-label="Nine import batches from A1 through A9">
      <line x1="24" x2="292" y1="88" y2="88" className="chart-axis" />
      <line x1="24" x2="24" y1="14" y2="88" className="chart-axis" />
      {[32, 56, 80].map((y) => (
        <line key={y} x1="24" x2="292" y1={y} y2={y} className="chart-grid" />
      ))}
      {bars.map((h, i) => {
        const x = 38 + i * 28
        return (
          <g key={`A${i + 1}`}>
            <rect
              className="bar-vert"
              x={x}
              y={88 - h}
              width="16"
              height={h}
              rx="2"
              fill={i % 3 === 1 ? 'var(--color-rose)' : 'var(--color-sage)'}
              opacity="0.72"
              style={{ animationDelay: `${420 + i * 45}ms` }}
            />
            <text x={x + 8} y="105" textAnchor="middle" className="chart-label">{`A${i + 1}`}</text>
          </g>
        )
      })}
      <text x="16" y="91" textAnchor="end" className="chart-label">0</text>
      <text x="16" y="35" textAnchor="end" className="chart-label">2k</text>
    </svg>
  )
}

function BinaryDiagram() {
  return (
    <svg className="binary-diagram" viewBox="0 0 150 150" role="img" aria-label="Single binary size is forty four megabytes">
      <circle cx="75" cy="75" r="58" fill="rgba(246,230,169,0.20)" stroke="rgba(26,26,46,0.62)" strokeWidth="2.4" />
      <circle cx="75" cy="75" r="45" fill="none" stroke="rgba(26,26,46,0.13)" strokeDasharray="4 6" />
      <text x="75" y="72" textAnchor="middle" className="binary-diagram__metric">44MB</text>
      <text x="75" y="94" textAnchor="middle" className="binary-diagram__label">local</text>
    </svg>
  )
}

function PipelineGraphic() {
  const stages = [
    { no: '1', score: '0.074', label: 'Chunked text', color: 'var(--color-rose)', pct: '11%' },
    { no: '2', score: '0.345', label: 'Tools + AST extracted', color: 'var(--color-purple)', pct: '50%' },
    { no: '3', score: '0.691', label: 'AI narrative', color: 'var(--color-sage)', pct: '100%' },
  ]

  return (
    <div className="pipeline-graphic">
      <svg className="pipeline-line" viewBox="0 0 520 64" aria-hidden="true">
        <defs>
          <marker id="pipeline-arrow" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="5" markerHeight="5" orient="auto-start-reverse">
            <path d="M1 1 L8 5 L1 9" fill="none" stroke="var(--color-purple)" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
          </marker>
        </defs>
        <path className="pipeline-line__dash chart-path" pathLength="1" d="M88 31 H432" markerEnd="url(#pipeline-arrow)" />
      </svg>
      {stages.map((stage, index) => (
        <div className="pipeline-stage" key={stage.no} style={{ '--stage-color': stage.color }}>
          <div className="pipeline-stage__circle" style={{ borderColor: stage.color, color: stage.color }}>{stage.no}</div>
          <p className="type-metric-md">
            {index > 0 ? <span className="pipeline-arrow-text" aria-hidden="true">-&gt;</span> : null}
            {stage.score}
          </p>
          <p className="type-dateline">{stage.label}</p>
          <div className="pipeline-score-bar">
            <span style={{ width: stage.pct, background: stage.color, animationDelay: `${620 + index * 80}ms` }} />
          </div>
        </div>
      ))}
    </div>
  )
}

function ForgettingCard() {
  return (
    <BentoCard className="card--forgetting" delay={0}>
      <CardKicker number="01" label="FROM THE DAILY CONTEXT" />
      <h2 className="type-hl-lg">The Forgetting Problem</h2>
      <div className="rule-double mt-3" />
      <div className="forgetting-layout">
        <div className="forgetting-copy">
          <p className="type-body news-lede">Claude has amnesia. Every new session starts from zero.</p>
          <div className="newspaper-lines" aria-hidden="true">
            <span style={{ width: '92%' }} />
            <span style={{ width: '74%' }} />
            <span style={{ width: '86%' }} />
            <span style={{ width: '63%' }} />
          </div>
        </div>
        <ForgettingChart />
      </div>
      <div className="source-row">
        <span>SOURCE: Claude code analytical core</span>
        <span>APR 24, 2025</span>
      </div>
    </BentoCard>
  )
}

function ActiveMemoryCard() {
  return (
    <BentoCard className="card--active" delay={80}>
      <CardKicker number="02" />
      <h2 className="type-hl-md">Active Memory</h2>
      <p className="type-dateline mt-3">Context appears when you need it</p>
      <p className="type-body-sm active-copy">Six hooks across the session lifecycle. Context injected at start, on prompt, after edits, on stop, before compaction, and at end. 12 MCP tools for explicit search.</p>
      <HookTimeline />
      <div className="rule mt-4 pt-4">
        <p className="active-metric">&lt; 50ms — you never notice</p>
        <p className="type-caption mt-2">Runs locally. Never blocks. <Link to="/docs/hooks" className="type-caption" style={{ color: 'var(--color-purple)' }}>How it works →</Link></p>
      </div>
    </BentoCard>
  )
}

function SearchCard() {
  return (
    <BentoCard className="card--search" delay={160}>
      <CardKicker number="03" />
      <h2 className="type-hl-sm">The Search</h2>
      <p className="type-dateline mt-3">From query to relevant memory in</p>
      <div className="search-metric">&lt;1ms</div>
      <p className="type-caption">P95 search latency</p>
      <SearchLatencyBars />
      <ul className="search-flow">
        <li>natural language query</li>
        <li>semantic search</li>
        <li>ranked results</li>
      </ul>
    </BentoCard>
  )
}

function ImportCard() {
  return (
    <BentoCard className="card--import" delay={240}>
      <CardKicker number="04" />
      <h2 className="type-hl-md">The Import</h2>
      <p className="type-body-sm mt-2">Your past. Brought forward.</p>
      <div className="import-metrics">
        <div>
          <p className="type-metric-md">1,107</p>
          <span>conversations</span>
        </div>
        <div>
          <p className="type-metric-md">15,745</p>
          <span>chunks</span>
        </div>
      </div>
      <div className="progress-label">
        <span>IMPORT PROGRESS</span>
        <span>100%</span>
      </div>
      <div className="progress-rail">
        <div className="progress-rail__fill" />
      </div>
      <ImportBatchChart />
      <p className="type-caption">Sample data — your numbers depend on usage.</p>
    </BentoCard>
  )
}

function OneBinaryCard() {
  const dependencies = ['docker', 'compose', 'daemon', 'python', 'pip', 'venv', 'db', 'pgvector']

  return (
    <BentoCard className="card--binary" delay={320}>
      <CardKicker number="05" />
      <h2 className="type-hl-sm">One Binary</h2>
      <PostIt className="binary-note" bg="var(--color-amber-paper)" rot="-2deg">
        44MB CSR<br />(single binary)
      </PostIt>
      <BinaryDiagram />
      <div className="competitor-box">
        <p className="type-dateline">COMPETITOR STACK</p>
        <div className="competitor-stack" aria-label="Docker plus Python plus database equals about one thousand two hundred sixty eight megabytes">
          <span>Docker</span>
          <b>+</b>
          <span>Python</span>
          <b>+</b>
          <span>DB</span>
        </div>
        <p className="competitor-total">= ~1,268MB</p>
        <p className="dependency-list">
          40+ dependencies: {dependencies.join(', ')}
        </p>
      </div>
    </BentoCard>
  )
}

function PipelineCard() {
  return (
    <BentoCard className="card--pipeline" delay={400}>
      <CardKicker number="06" label="ENRICHMENT PIPELINE" />
      <h2 className="type-hl-md">The Pipeline</h2>
      <PipelineGraphic />
      <p className="pipeline-caption">Search relevance score improves 9.3x from raw chunks to AI-enriched narratives.</p>
    </BentoCard>
  )
}

function PrivacyCard() {
  return (
    <BentoCard className="card--privacy" delay={480}>
      <CardKicker number="07" />
      <div className="privacy-content">
        <svg className="privacy-lock" viewBox="0 0 48 48" aria-hidden="true">
          <path
            className="lock-path"
            pathLength="1"
            d="M15 22V17C15 10.9 19.5 7 24 7C28.5 7 33 10.9 33 17V22 M13 22H35V40H13V22Z M24 29V34"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.4"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
        <p>Local by default.</p>
        <span>127.0.0.1 — search + embed on-device</span>
        <span className="type-caption mt-2" style={{ display: 'block', maxWidth: 180, color: 'var(--color-muted)' }}>AI narratives optionally use Anthropic Batch API</span>
      </div>
    </BentoCard>
  )
}

function ASTTreeViz() {
  // Left-to-right AST tree — big, readable, fills the panel
  const leaves = [
    { y: 18, label: 'Engine', color: 'var(--color-purple)' },
    { y: 52, label: 'Storage', color: 'var(--color-purple)' },
    { y: 86, label: 'search', color: 'var(--color-sage)' },
    { y: 120, label: 'embed', color: 'var(--color-sage)' },
    { y: 154, label: 'hnsw', color: 'var(--color-rose)' },
    { y: 188, label: 'rmcp', color: 'var(--color-rose)' },
  ]
  const l1 = [
    { y: 35, label: 'struct', color: 'var(--color-purple)', leaves: [0, 1] },
    { y: 103, label: 'impl', color: 'var(--color-sage)', leaves: [2, 3] },
    { y: 171, label: 'use', color: 'var(--color-rose)', leaves: [4, 5] },
  ]

  return (
    <svg className="ast-tree" viewBox="0 0 280 206" role="img" aria-label="Abstract syntax tree, left to right">
      {/* Root — fn */}
      <circle cx="28" cy="103" r="14" fill="var(--color-purple)" opacity="0.7" />
      <text x="28" y="107" textAnchor="middle" style={{ fontSize: '11px', fill: 'white', fontFamily: 'var(--font-mono)', fontWeight: 700 }}>fn</text>

      {/* Root → L1 branches */}
      {l1.map(n => (
        <line key={n.label} x1="42" y1="103" x2="90" y2={n.y} stroke={n.color} strokeWidth="2" opacity="0.3" />
      ))}

      {/* L1 nodes */}
      {l1.map(n => (
        <g key={n.label}>
          <circle cx="100" cy={n.y} r="11" fill={n.color} opacity="0.5" />
          <text x="100" y={n.y + 4} textAnchor="middle" style={{ fontSize: '8px', fill: 'white', fontFamily: 'var(--font-mono)', fontWeight: 600 }}>{n.label}</text>
          {/* L1 → L2 branches */}
          {n.leaves.map(li => (
            <line key={li} x1="111" y1={n.y} x2="172" y2={leaves[li].y} stroke={n.color} strokeWidth="1.5" opacity="0.2" />
          ))}
        </g>
      ))}

      {/* L2 leaf dots + labels */}
      {leaves.map((leaf, i) => (
        <g key={leaf.label}>
          <circle cx="180" cy={leaf.y} r="7" fill={leaf.color} opacity="0.4" className="hook-node__dot" style={{ animationDelay: `${800 + i * 80}ms` }} />
          <text x="194" y={leaf.y + 1} className="chart-label" dominantBaseline="middle" style={{ fontSize: '11px', fontWeight: 500 }}>{leaf.label}</text>
        </g>
      ))}
    </svg>
  )
}

function ASTFlowDiagram() {
  const steps = [
    { label: 'code', icon: '{ }', color: 'var(--color-muted)' },
    { label: 'parse', icon: '⟶', color: 'var(--color-purple)' },
    { label: 'AST', icon: '🌳', color: 'var(--color-sage)' },
    { label: 'extract', icon: '⟶', color: 'var(--color-purple)' },
    { label: 'search', icon: '◉', color: 'var(--color-rose)' },
  ]

  return (
    <div className="ast-flow">
      {steps.map((s, i) => (
        <div key={s.label} className="ast-flow__step" style={{ color: s.color, animationDelay: `${640 + i * 80}ms` }}>
          <span className="ast-flow__icon">{s.icon}</span>
          <span className="ast-flow__label">{s.label}</span>
        </div>
      ))}
    </div>
  )
}

function ASTCard() {
  const langs = [
    { name: 'Rust', color: 'var(--color-rose)' },
    { name: 'Python', color: 'var(--color-purple)' },
    { name: 'TS', color: 'var(--color-sage)' },
    { name: 'JS', color: '#d4a574' },
    { name: 'Go', color: '#5b7b95' },
    { name: 'TSX', color: 'var(--color-purple)' },
  ]

  return (
    <BentoCard className="card--ast" delay={640}>
      <div className="ast-layout">
        <div className="ast-left">
          <CardKicker number="09" label="CODE-AWARE SEARCH" />
          <h2 className="type-hl-sm">AST Analysis</h2>
          <p className="type-body-sm mt-2">Search by function name, type, or import — not just text.</p>
          <div className="ast-langs">
            {langs.map(l => (
              <span key={l.name} className="ast-lang" style={{ borderColor: l.color, color: l.color }}>{l.name}</span>
            ))}
          </div>
        </div>
        <div className="ast-right">
          <ASTFlowDiagram />
          <ASTTreeViz />
        </div>
      </div>
    </BentoCard>
  )
}

function InstallCard() {
  const command = 'curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh'

  return (
    <BentoCard className="card--install" delay={720} id="install">
      <div className="install-layout">
        <div className="install-left">
          <CardKicker number="08" label="GET STARTED" />
          <h2 className="type-hl-md">Install</h2>
          <p className="install-classified">Memory is not a feature. It is the foundation.</p>
        </div>
        <div className="install-right">
          <div className="install-command">
            <code>curl -fsSL .../scripts/install.sh | sh</code>
            <CopyBtn text={command} />
          </div>
          <p className="type-caption mt-2">One command. Downloads binary, auto-runs setup, registers hooks + MCP. Restart Claude Code.</p>
          <div className="install-tabs-inline">
            {['44MB binary', 'zero deps', '6 hooks', '12 MCP tools'].map((label, index) => (
              <span key={label} className="tear-tab" style={{ animationDelay: `${720 + index * 70}ms` }}>{label}</span>
            ))}
          </div>
          <div className="platform-badges">
            <span className="platform-badge platform-badge--ok" title="Apple Silicon Mac">
              <svg viewBox="0 0 16 16" width="12" height="12"><path d="M12.2 5.4c-.1.1-1.8 1-1.8 3.2 0 2.5 2.2 3.3 2.2 3.3 0 .1-.3 1.2-1.1 2.3-.7 1-1.4 2-2.5 2s-1.4-.6-2.7-.6c-1.3 0-1.7.6-2.7.6S2 15.3 1.3 14.2C.5 12.9 0 10.7 0 8.7 0 5.6 2 4 3.9 4c1 0 1.9.7 2.5.7.6 0 1.6-.7 2.8-.7.5 0 2.1 0 3 1.4z" fill="currentColor"/><circle cx="10" cy="1.8" r="1.8" fill="currentColor"/></svg>
              Mac (ARM)
            </span>
            <span className="platform-badge platform-badge--ok" title="Linux x86_64 and ARM64, including WSL">
              <svg viewBox="0 0 16 16" width="12" height="12"><path d="M8 1C4.7 1 2 3.7 2 7v2.5c0 .8.3 1.5.8 2L4 13v1.5c0 .3.2.5.5.5h2c.3 0 .5-.2.5-.5V13h2v1.5c0 .3.2.5.5.5h2c.3 0 .5-.2.5-.5V13l1.2-1.5c.5-.5.8-1.2.8-2V7c0-3.3-2.7-6-6-6z" fill="none" stroke="currentColor" strokeWidth="1.3"/><circle cx="6" cy="6.5" r="1" fill="currentColor"/><circle cx="10" cy="6.5" r="1" fill="currentColor"/></svg>
              Linux / WSL
            </span>
            <span className="platform-badge platform-badge--warn" title="Intel Mac requires building from source">
              <svg viewBox="0 0 16 16" width="12" height="12"><path d="M12.2 5.4c-.1.1-1.8 1-1.8 3.2 0 2.5 2.2 3.3 2.2 3.3 0 .1-.3 1.2-1.1 2.3-.7 1-1.4 2-2.5 2s-1.4-.6-2.7-.6c-1.3 0-1.7.6-2.7.6S2 15.3 1.3 14.2C.5 12.9 0 10.7 0 8.7 0 5.6 2 4 3.9 4c1 0 1.9.7 2.5.7.6 0 1.6-.7 2.8-.7.5 0 2.1 0 3 1.4z" fill="currentColor"/><circle cx="10" cy="1.8" r="1.8" fill="currentColor"/></svg>
              Mac (Intel) — source
            </span>
          </div>
        </div>
      </div>
    </BentoCard>
  )
}

function TechBadge({ name, abbr, color, icon }) {
  return (
    <div className="tech-badge" style={{ '--badge-color': color }}>
      <div className="tech-badge__icon">{icon}</div>
      <div className="tech-badge__label">
        <strong>{name}</strong>
        <span>{abbr}</span>
      </div>
    </div>
  )
}

function ArchCard() {
  return (
    <BentoCard className="card--arch" delay={740}>
      <CardKicker number="—" label="UNDER THE HOOD" />
      <div className="arch-layout">
        <div className="arch-copy">
          <h2 className="type-hl-md">Built on proven primitives</h2>
          <p className="type-body-sm mt-2">No framework wrappers. Direct access to the algorithms — compiled to a single native binary. Code-aware search via AST across 6 languages.</p>
        </div>
        <div className="arch-mid">
          <ASTFlowDiagram />
          <ASTTreeViz />
          <div className="ast-langs" style={{ marginTop: 6 }}>
            {['Rust','Python','TS','JS','Go','TSX'].map(name => {
              const colors = { Rust: 'var(--color-rose)', Python: 'var(--color-purple)', TS: 'var(--color-sage)', JS: '#d4a574', Go: '#5b7b95', TSX: 'var(--color-purple)' }
              return <span key={name} className="ast-lang" style={{ borderColor: colors[name], color: colors[name] }}>{name}</span>
            })}
          </div>
        </div>
        <div className="arch-stack">
          <TechBadge name="Rust" abbr="zero-cost abstractions" color="var(--color-rose)" icon={
            <svg viewBox="0 0 24 24" width="22" height="22"><circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" strokeWidth="1.8" /><path d="M8 16l4-10 4 10M9.5 13h5" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" /></svg>
          } />
          <TechBadge name="HNSW" abbr="vector search <1ms" color="var(--color-purple)" icon={
            <svg viewBox="0 0 24 24" width="22" height="22"><circle cx="12" cy="6" r="2.5" fill="currentColor" opacity="0.6" /><circle cx="6" cy="16" r="2.5" fill="currentColor" opacity="0.6" /><circle cx="18" cy="16" r="2.5" fill="currentColor" opacity="0.6" /><line x1="12" y1="8.5" x2="7" y2="13.5" stroke="currentColor" strokeWidth="1.2" /><line x1="12" y1="8.5" x2="17" y2="13.5" stroke="currentColor" strokeWidth="1.2" /><line x1="8.5" y1="16" x2="15.5" y2="16" stroke="currentColor" strokeWidth="1.2" /></svg>
          } />
          <TechBadge name="FastEmbed" abbr="384-dim local vectors" color="var(--color-sage)" icon={
            <svg viewBox="0 0 24 24" width="22" height="22"><rect x="4" y="8" width="3" height="10" rx="1" fill="currentColor" opacity="0.5" /><rect x="10.5" y="4" width="3" height="14" rx="1" fill="currentColor" opacity="0.7" /><rect x="17" y="10" width="3" height="8" rx="1" fill="currentColor" opacity="0.5" /></svg>
          } />
          <TechBadge name="SQLite" abbr="embedded storage" color="#d4a574" icon={
            <svg viewBox="0 0 24 24" width="22" height="22"><ellipse cx="12" cy="7" rx="8" ry="3" fill="none" stroke="currentColor" strokeWidth="1.4" /><path d="M4 7v10c0 1.66 3.58 3 8 3s8-1.34 8-3V7" fill="none" stroke="currentColor" strokeWidth="1.4" /><path d="M4 12c0 1.66 3.58 3 8 3s8-1.34 8-3" fill="none" stroke="currentColor" strokeWidth="1.2" opacity="0.5" /></svg>
          } />
          <TechBadge name="ast-grep" abbr="6-language AST parser" color="var(--color-purple)" icon={
            <svg viewBox="0 0 24 24" width="22" height="22"><path d="M12 4v4M12 8l-5 4M12 8l5 4M7 12v4M17 12v4M7 16l-2 3M7 16l2 3M17 16l-2 3M17 16l2 3" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" /></svg>
          } />
          <TechBadge name="rmcp" abbr="MCP protocol (12 tools)" color="#5b7b95" icon={
            <svg viewBox="0 0 24 24" width="22" height="22"><rect x="3" y="9" width="7" height="6" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.3" /><rect x="14" y="9" width="7" height="6" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.3" /><line x1="10" y1="12" x2="14" y2="12" stroke="currentColor" strokeWidth="1.3" /><circle cx="12" cy="12" r="1.2" fill="currentColor" /></svg>
          } />
        </div>
      </div>
    </BentoCard>
  )
}

const ASK_QUESTIONS = [
  '"How did we solve re-renders on this component?"',
  '"What did we tell Joe about that commit?"',
  '"What were our frustrations with this approach?"',
  '"Where did we put the auth middleware config?"',
  '"Why did we switch from Redis to SQLite?"',
  '"What broke last time we upgraded React?"',
]

function RotatingQuestions() {
  const [idx, setIdx] = useState(0)

  useEffect(() => {
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return
    const t = setInterval(() => setIdx(i => (i + 1) % ASK_QUESTIONS.length), 3200)
    return () => clearInterval(t)
  }, [])

  return (
    <div className="ask-questions" aria-live="polite" aria-atomic="true">
      {ASK_QUESTIONS.map((q, i) => (
        <p key={q} className={`ask-question ${i === idx ? 'ask-question--active' : ''}`} aria-hidden={i !== idx}>{q}</p>
      ))}
    </div>
  )
}

function AskPipeline() {
  const steps = [
    { label: 'Install', icon: '↓', sub: 'curl | sh' },
    { label: 'Hooks inject', icon: '⚡', sub: 'auto, <50ms' },
    { label: 'You ask', icon: '?', sub: 'natural language' },
    { label: 'Next session', icon: '∞', sub: 'context persists' },
  ]

  return (
    <div className="ask-pipeline">
      {steps.map((step, i) => (
        <div key={step.label} className="ask-step">
          <div className="ask-step__icon" style={{ animationDelay: `${1800 + i * 120}ms` }}>{step.icon}</div>
          <strong>{step.label}</strong>
          <span>{step.sub}</span>
          {i < steps.length - 1 && <div className="ask-step__arrow" aria-hidden="true" />}
        </div>
      ))}
    </div>
  )
}

function WhatYouAskCard() {
  return (
    <BentoCard className="card--ask" delay={760}>
      <div className="ask-layout">
        <div className="ask-left">
          <CardKicker number="—" label="AFTER INSTALL" />
          <h2 className="type-hl-md">What you'll ask</h2>
          <p className="type-body-sm mt-2">Natural language. No syntax. Just ask what you'd ask a teammate who was there.</p>
          <RotatingQuestions />
        </div>
        <div className="ask-right">
          <AskPipeline />
        </div>
      </div>
    </BentoCard>
  )
}

function CeilingChart() {
  // File size grows linearly until it hits context window, then flatlines with a red danger zone
  const fileLine = [
    [32, 140], [58, 128], [84, 112], [110, 92], [136, 68],
    [162, 44], [178, 34], [188, 30], [198, 28], [216, 27],
  ]
  const d = fileLine.map(([x, y], i) => `${i === 0 ? 'M' : 'L'}${x} ${y}`).join(' ')

  return (
    <svg className="ceiling-chart" viewBox="0 0 248 168" role="img" aria-label="File size grows until it hits the context window ceiling">
      {/* Grid */}
      {[44, 76, 108, 140].map(y => (
        <line key={y} x1="32" x2="216" y1={y} y2={y} className="chart-grid" />
      ))}
      <line x1="32" x2="32" y1="20" y2="148" className="chart-axis" />
      <line x1="32" x2="218" y1="148" y2="148" className="chart-axis" />

      {/* Danger zone above ceiling */}
      <rect x="32" y="20" width="184" height="12" fill="rgba(181,131,141,0.12)" rx="0" />
      <line x1="32" x2="216" y1="32" y2="32" stroke="var(--color-rose)" strokeWidth="1.5" strokeDasharray="4 3" opacity="0.6" />
      <text x="218" y="36" className="chart-label" fill="var(--color-rose)">limit</text>

      {/* Growth curve */}
      <path className="chart-path" pathLength="1" d={d} fill="none" stroke="var(--color-rose)" strokeWidth="2.2" strokeLinecap="round" />

      {/* Labels */}
      <text x="24" y="148" textAnchor="end" className="chart-label">0</text>
      <text x="24" y="92" textAnchor="end" className="chart-label">64K</text>
      <text x="24" y="36" textAnchor="end" className="chart-label">128K</text>
      <text x="48" y="162" className="chart-label">1</text>
      <text x="124" y="162" textAnchor="middle" className="chart-label">sessions</text>
      <text x="210" y="162" textAnchor="end" className="chart-label">500</text>
    </svg>
  )
}

function ExtractionLossChart() {
  // Funnel showing data loss through extraction pipeline
  const bars = [
    { label: 'raw', w: 148, color: 'var(--color-muted)', pct: '100%' },
    { label: 'entities', w: 62, color: 'var(--color-sage)', pct: '42%' },
    { label: 'recall', w: 28, color: 'var(--color-rose)', pct: '19%' },
  ]

  return (
    <svg className="extraction-chart" viewBox="0 0 240 132" role="img" aria-label="Knowledge graph extraction retains only nineteen percent of original context">
      {bars.map((bar, i) => {
        const y = 12 + i * 38
        const x = 72
        return (
          <g key={bar.label}>
            <text x="68" y={y + 14} textAnchor="end" className="chart-label">{bar.label}</text>
            <rect
              className="bar-horiz"
              x={x} y={y}
              width={bar.w} height="20" rx="3"
              fill={bar.color} opacity="0.6"
              style={{ animationDelay: `${880 + i * 80}ms` }}
            />
            <text x={x + bar.w + 6} y={y + 14} className="chart-label" style={{ fontWeight: 600 }}>{bar.pct}</text>
          </g>
        )
      })}
      {/* Loss arrows */}
      <path d="M146 32 L146 42" stroke="var(--color-rose)" strokeWidth="1.2" opacity="0.4" markerEnd="url(#loss-arrow)" />
      <path d="M106 70 L106 80" stroke="var(--color-rose)" strokeWidth="1.2" opacity="0.4" markerEnd="url(#loss-arrow)" />
      <defs>
        <marker id="loss-arrow" viewBox="0 0 6 6" refX="3" refY="3" markerWidth="4" markerHeight="4" orient="auto">
          <path d="M0 0 L6 3 L0 6" fill="var(--color-rose)" />
        </marker>
      </defs>
    </svg>
  )
}

function LatencyCompareChart() {
  // Horizontal bar chart: local <1ms vs cloud 100-500ms
  const rows = [
    { label: 'CSR local', ms: '0.8ms', w: 4, color: 'var(--color-sage)' },
    { label: 'cloud API', ms: '~300ms', w: 148, color: 'var(--color-rose)' },
  ]

  return (
    <svg className="latency-compare" viewBox="0 0 240 96" role="img" aria-label="Local search zero point eight milliseconds versus cloud three hundred milliseconds">
      {rows.map((row, i) => {
        const y = 10 + i * 42
        return (
          <g key={row.label}>
            <text x="66" y={y + 14} textAnchor="end" className="chart-label">{row.label}</text>
            <rect
              className="bar-horiz"
              x={70} y={y}
              width={row.w} height="22" rx="3"
              fill={row.color} opacity="0.65"
              style={{ animationDelay: `${960 + i * 100}ms` }}
            />
            <text x={70 + row.w + 6} y={y + 15} className="chart-label" style={{ fontWeight: 700, fontSize: '11px' }}>{row.ms}</text>
          </g>
        )
      })}
      {/* Scale */}
      <line x1="70" x2="220" y1="82" y2="82" className="chart-axis" />
      <text x="70" y="92" className="chart-label">0</text>
      <text x="144" y="92" textAnchor="middle" className="chart-label">150ms</text>
      <text x="220" y="92" textAnchor="end" className="chart-label">300ms</text>
    </svg>
  )
}

function FlatFileCard() {
  return (
    <BentoCard className="card--flatfile" delay={800}>
      <CardKicker number="10" label="COMPARED" accent="rose" />
      <h2 className="type-hl-sm">The Flat File</h2>
      <p className="type-dateline mt-2">One markdown file. Grows until it hits the context ceiling.</p>
      <CeilingChart />
      <PostIt className="flatfile-note" bg="var(--color-rose-paper)" rot="2deg">
        no search<br />no decay
      </PostIt>
      <a className="cite-link" href="https://arxiv.org/abs/2404.07143" target="_blank" rel="noopener noreferrer">Liu et al. 2024 — "Lost in the Middle"</a>
    </BentoCard>
  )
}

function KnowledgeGraphCard() {
  return (
    <BentoCard className="card--kgraph" delay={880}>
      <CardKicker number="11" label="COMPARED" accent="sage" />
      <h2 className="type-hl-sm">The Knowledge Graph</h2>
      <p className="type-dateline mt-2">Extracts entities. Loses the rest.</p>
      <ExtractionLossChart />
      <div className="kgraph-stat">
        <span className="type-metric-md" style={{ color: 'var(--color-rose)' }}>81%</span>
        <span className="type-caption">context lost in extraction</span>
      </div>
      <a className="cite-link" href="https://arxiv.org/abs/2306.04136" target="_blank" rel="noopener noreferrer">Pan et al. 2023 — "Unifying Large Language Models and Knowledge Graphs"</a>
    </BentoCard>
  )
}

function WhatsNewCard() {
  return (
    <BentoCard className="card--whatsnew" delay={560} id="whats-new">
      <CardKicker number="—" label="LATEST RELEASE" accent="purple" />
      <h2 className="type-hl-sm">What's New in v9.2.0</h2>
      <ul className="whatsnew-list">
        <li>
          <strong>Episode intelligence</strong> — every session ends as a structured episode; SessionStart opens with a CONTINUUM block and a pickup menu of recent threads
        </li>
        <li>
          <strong>Intent routing</strong> — "pick up where we left off" is detected semantically (exemplar embeddings, zero new models) and answered from episode state
        </li>
        <li>
          <strong>Provenance re-ranking</strong> — recall favors what was decided and done over what was merely proposed; CSR now beats grep on its own founding-conversation benchmark
        </li>
        <li>
          <strong>Code graph</strong> — <code>csr_code_graph</code> links functions to the conversations that shaped them via AST anchors
        </li>
        <li>
          <strong>Full-transcript recall</strong> — tool results are embedded with size-based chunking; coverage went from ~1% of each conversation to effectively all of it
        </li>
        <li>
          <strong>Telemetry dashboard</strong> — <code>csr-engine telemetry</code> with hook latency percentiles, startup stats, and a live TUI
        </li>
      </ul>
      <a className="cite-link" href="https://github.com/ramakay/claude-self-reflect/releases/tag/v9.2.0" target="_blank" rel="noopener noreferrer">Full release notes →</a>
    </BentoCard>
  )
}

function HostedMemoryCard() {
  return (
    <BentoCard className="card--hosted" delay={960}>
      <CardKicker number="12" label="COMPARED" accent="purple" />
      <h2 className="type-hl-sm">The Cloud Memory</h2>
      <p className="type-dateline mt-2">Your conversations leave localhost.</p>
      <LatencyCompareChart />
      <div className="hosted-footer">
        <div className="hosted-icon" aria-hidden="true">
          <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="var(--color-rose)" strokeWidth="1.8" strokeLinecap="round">
            <path d="M12 2v10m0 0l3-3m-3 3l-3-3" />
            <path d="M4 14c0 3.3 3.6 6 8 6s8-2.7 8-6" opacity="0.5" />
          </svg>
        </div>
        <span className="type-caption">data sent to external servers on every operation</span>
      </div>
      <a className="cite-link" href="https://arxiv.org/abs/2310.08560" target="_blank" rel="noopener noreferrer">Zhang et al. 2023 — "A Survey on LLM-based Autonomous Agents"</a>
    </BentoCard>
  )
}

const TRAIL_COUNT = 5
const TRAIL_COLORS = ['#f6e6a9', '#f3c9c3', '#dce8cf', '#e0d8ee', '#f6e6a9']
const TRAIL_WORDS = ['remember', 'recall', 'context', 'memory', 'archive']

function CursorTrail() {
  const canvasRef = useRef(null)

  useEffect(() => {
    const mql = window.matchMedia('(pointer: fine) and (min-width: 900px) and (prefers-reduced-motion: no-preference)')
    if (!mql.matches) return

    const container = canvasRef.current
    if (!container) return

    const els = Array.from({ length: TRAIL_COUNT }, (_, i) => {
      const el = document.createElement('div')
      el.textContent = TRAIL_WORDS[i]
      el.style.cssText = `
        position: fixed; pointer-events: none; z-index: 100;
        font-family: Caveat, cursive; font-size: ${16 - i * 1.5}px;
        color: #343145; padding: 3px 7px; border-radius: 3px;
        background: ${TRAIL_COLORS[i]};
        box-shadow: 1px 2px 6px rgba(0,0,0,0.05);
        opacity: 0; will-change: transform, opacity;
        transition: opacity 0.6s ease;
      `
      container.appendChild(el)
      return { el, x: 0, y: 0, targetX: 0, targetY: 0, rot: (i - 2) * 3 }
    })

    let mouseX = 0
    let mouseY = 0
    let moving = false
    let idleTimer = null
    let raf = null

    const onMove = (e) => {
      mouseX = e.clientX
      mouseY = e.clientY
      moving = true
      clearTimeout(idleTimer)
      idleTimer = setTimeout(() => { moving = false }, 800)
    }

    const tick = () => {
      els[0].targetX = mouseX
      els[0].targetY = mouseY

      for (let i = 1; i < TRAIL_COUNT; i += 1) {
        els[i].targetX = els[i - 1].x
        els[i].targetY = els[i - 1].y
      }

      for (let i = 0; i < TRAIL_COUNT; i += 1) {
        const speed = 0.12 - i * 0.018
        els[i].x += (els[i].targetX - els[i].x) * speed
        els[i].y += (els[i].targetY - els[i].y) * speed

        const item = els[i]
        item.el.style.transform = `translate(${item.x - 20}px, ${item.y - 10 + i * 22}px) rotate(${item.rot}deg)`
        item.el.style.opacity = moving ? (0.65 - i * 0.1) : 0
      }

      raf = requestAnimationFrame(tick)
    }

    window.addEventListener('mousemove', onMove, { passive: true })
    raf = requestAnimationFrame(tick)

    return () => {
      window.removeEventListener('mousemove', onMove)
      cancelAnimationFrame(raf)
      clearTimeout(idleTimer)
      els.forEach((item) => item.el.remove())
    }
  }, [])

  return <div ref={canvasRef} className="fixed inset-0 pointer-events-none z-[100]" aria-hidden="true" />
}

function getInitialTheme() {
  if (typeof window === 'undefined') return 'light'
  const saved = localStorage.getItem('csr-theme')
  if (saved === 'light' || saved === 'dark') return saved
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}

function ThemeToggle({ theme, setTheme }) {
  const next = theme === 'dark' ? 'light' : 'dark'

  return (
    <button
      className="theme-toggle"
      type="button"
      aria-label={`Switch to ${next} mode`}
      onClick={() => {
        localStorage.setItem('csr-theme', next)
        setTheme(next)
        document.documentElement.dataset.theme = next
      }}
    >
      {theme === 'dark' ? <Sun size={17} aria-hidden="true" /> : <Moon size={17} aria-hidden="true" />}
    </button>
  )
}

function LogoPreviews() {
  return (
    <section className="logo-previews" aria-label="Logo concept previews">
      <div className="go-deeper__heading">
        <span />
        <h2>Logo concepts</h2>
        <span />
      </div>
      <div className="logo-grid">
        {/* 1. Recursive Mirror */}
        <div className="logo-option">
          <div className="logo-option__mark">
            <svg viewBox="0 0 32 32" fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round">
              <path d="M7 6h12a6 6 0 0 1 6 6v13H13a6 6 0 0 1-6-6V6Z" strokeWidth="3"/>
              <path d="M14 12h5a3 3 0 0 1 3 3v6h-5a3 3 0 0 1-3-3v-6Z" strokeWidth="2.5"/>
              <path d="M9 24 24 9" strokeWidth="2.25"/>
            </svg>
          </div>
          <strong>Recursive Mirror</strong>
          <span>Nested echo — reflection within reflection</span>
        </div>

        {/* 2. Memory Spiral */}
        <div className="logo-option">
          <div className="logo-option__mark">
            <svg viewBox="0 0 32 32" fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round">
              <path d="M22 7c-3-2-8-2-11 1-4 3-5 9-2 13 3 5 10 6 14 2 4-3 4-9 1-12-3-3-8-3-11 0-2 2-2 6 0 8 2 3 6 3 8 1 2-2 2-5 0-7-1-1-3-1-5 0" strokeWidth="3"/>
              <circle cx="16" cy="16" r="2.5" fill="currentColor" stroke="none"/>
            </svg>
          </div>
          <strong>Memory Spiral</strong>
          <span>Conversation memory coiling inward</span>
        </div>

        {/* 3. Infinity Recall */}
        <div className="logo-option">
          <div className="logo-option__mark">
            <svg viewBox="0 0 32 32" fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round">
              <path d="M5 16c3-6 8-6 11 0s8 6 11 0c-3-6-8-6-11 0S8 22 5 16Z" strokeWidth="3"/>
              <circle cx="16" cy="16" r="3" fill="currentColor" stroke="none"/>
              <path d="M16 9v3M16 20v3" strokeWidth="2.5"/>
            </svg>
          </div>
          <strong>Infinity Recall</strong>
          <span>Continuous loop — memory never ends</span>
        </div>

        {/* 4a. Isometric Mirror — clean */}
        <div className="logo-option">
          <div className="logo-option__mark">
            <svg viewBox="0 0 32 32" fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round">
              {/* Back face — faded reflection */}
              <path d="M16 4 L28 10 L28 22 L16 28 Z" strokeWidth="2" opacity="0.25" fill="currentColor"/>
              {/* Front face — solid mirror */}
              <path d="M4 10 L16 4 L16 28 L4 22 Z" strokeWidth="2.5"/>
              {/* Inner reflection of front face — smaller echo */}
              <path d="M8 13 L14 10 L14 22 L8 19 Z" strokeWidth="1.5" opacity="0.4"/>
              {/* Self dot at hinge */}
              <circle cx="16" cy="16" r="2.2" fill="currentColor" stroke="none"/>
            </svg>
          </div>
          <strong>Mirror — Echo</strong>
          <span>Nested reflection inside the front face</span>
        </div>

        {/* 4b. Isometric Mirror — open book */}
        <div className="logo-option">
          <div className="logo-option__mark">
            <svg viewBox="0 0 32 32" fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round">
              {/* Left page */}
              <path d="M16 6 L4 11 L4 24 L16 28 Z" strokeWidth="2.5"/>
              {/* Right page — reflection, lighter */}
              <path d="M16 6 L28 11 L28 24 L16 28 Z" strokeWidth="2" opacity="0.3" fill="currentColor"/>
              {/* Left inner lines — content */}
              <line x1="7" y1="14" x2="14" y2="11.5" strokeWidth="1.4" opacity="0.35"/>
              <line x1="7" y1="17.5" x2="14" y2="15" strokeWidth="1.4" opacity="0.25"/>
              <line x1="7" y1="21" x2="14" y2="18.5" strokeWidth="1.4" opacity="0.15"/>
              {/* Spine glow */}
              <circle cx="16" cy="17" r="1.8" fill="currentColor" stroke="none" opacity="0.7"/>
            </svg>
          </div>
          <strong>Mirror — Book</strong>
          <span>Open book reflecting its own pages</span>
        </div>

        {/* 4c. Isometric Mirror — folded */}
        <div className="logo-option">
          <div className="logo-option__mark">
            <svg viewBox="0 0 32 32" fill="none" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round">
              {/* Left face */}
              <path d="M16 5 L4 12 L4 23 L16 27 Z" strokeWidth="2.5"/>
              {/* Right face — the reflection */}
              <path d="M16 5 L28 12 L28 23 L16 27 Z" strokeWidth="2" opacity="0.20"/>
              {/* Reflected silhouette on right — echoing left shape */}
              <path d="M18 10 L25 13.5 L25 21 L18 24 Z" fill="currentColor" opacity="0.08" strokeWidth="0"/>
              {/* Center hinge — bright */}
              <line x1="16" y1="5" x2="16" y2="27" strokeWidth="2.5" opacity="0.6"/>
              <circle cx="16" cy="16" r="2.5" fill="currentColor" stroke="none"/>
            </svg>
          </div>
          <strong>Mirror — Hinge</strong>
          <span>Two faces meeting at a bright spine</span>
        </div>
      </div>
    </section>
  )
}

function GoDeeper() {
  const links = [
    ['Guide', '/docs/why-csr', 'Learn concepts, workflows, and best practices.'],
    ['Reference', '/docs/mcp-tools', 'CLI, hooks, files, and configuration.'],
    ['Architecture', '/docs/architecture', 'Inside CSR system design, pipeline, and data flow.'],
    ['FAQ', '/docs/troubleshooting', 'Common questions and real-world answers.'],
  ]

  return (
    <section className="go-deeper" aria-label="Documentation entry points">
      <div className="go-deeper__heading">
        <span />
        <h2>Go deeper</h2>
        <span />
      </div>
      <div className="go-deeper__grid">
        {links.map(([label, href, desc]) => (
          <Link key={label} to={href} className="doc-tile">
            <strong>{label}</strong>
            <span>{desc}</span>
            <em>Start reading</em>
          </Link>
        ))}
      </div>
      <div className="go-deeper__cta">
        <div>
          <strong>Built for long-term context.</strong>
          <p>CSR gives Claude Code a persistent local memory that gets better over time.</p>
        </div>
        <Link to="/docs/installation">Get started in 60 seconds</Link>
      </div>
    </section>
  )
}

export default function Landing() {
  const [theme, setTheme] = useState(getInitialTheme)
  const datelineVisible = useDelayedReveal(0)
  const footerVisible = useDelayedReveal(400)

  useEffect(() => {
    document.documentElement.dataset.theme = theme
  }, [theme])

  useEffect(() => {
    const mql = window.matchMedia('(prefers-color-scheme: dark)')
    const handler = (e) => {
      if (!localStorage.getItem('csr-theme')) {
        const nextTheme = e.matches ? 'dark' : 'light'
        setTheme(nextTheme)
        document.documentElement.dataset.theme = nextTheme
      }
    }
    mql.addEventListener('change', handler)
    return () => mql.removeEventListener('change', handler)
  }, [])

  return (
    <div className="sky-shell">
      <CursorTrail />
      <header className="site-nav">
        <div className="site-nav__inner">
          <Link className="site-nav__brand" to="/">
            <img src="/claude-self-reflect/favicon.svg" alt="" width={34} height={34} />
            <span>
              <span className="site-nav__name">Claude Self-Reflect</span>
              <span className="site-nav__tag">Memory for Claude Code.</span>
            </span>
          </Link>
          <nav className="site-nav__links" aria-label="Primary">
            <Link className="site-nav__link" to="/docs/why-csr">Guide</Link>
            <Link className="site-nav__link" to="/docs/mcp-tools">Reference</Link>
            <Link className="site-nav__link" to="/docs/architecture">Architecture</Link>
            <Link className="site-nav__link" to="/docs/troubleshooting">FAQ</Link>
            <Link className="site-nav__link" to="/docs/upgrading">Changelog</Link>
            <a className="nav-install-cta" href="#install" onClick={e => {
              e.preventDefault()
              const el = document.getElementById('install')
              const grid = el?.closest('.bento-grid')
              if (!el || !grid) return
              el.scrollIntoView({ behavior: 'smooth', block: 'center' })
              grid.classList.add('grid--spotlight')
              el.classList.add('card--flash')
              // Add dismiss button
              if (!el.querySelector('.spotlight-dismiss')) {
                const btn = document.createElement('button')
                btn.className = 'spotlight-dismiss'
                btn.innerHTML = '&times;'
                btn.setAttribute('aria-label', 'Dismiss highlight')
                btn.onclick = () => { grid.classList.remove('grid--spotlight'); el.classList.remove('card--flash'); btn.remove() }
                el.appendChild(btn)
              }
            }}>Install</a>
            <ThemeToggle theme={theme} setTheme={setTheme} />
            <a className="site-nav__github" href="https://github.com/ramakay/claude-self-reflect" target="_blank" rel="noopener noreferrer">
              <Github size={18} aria-hidden="true" /> <span>View on GitHub</span>
            </a>
          </nav>
        </div>
      </header>

      <div className="hero-watermark" aria-hidden="true">
        <h1 className="hero-watermark__title">You or your agent don't have to remember any of this</h1>
        <p className="hero-watermark__subtext">because Claude Code does.</p>
      </div>

      <main className="landing-main">
        <p className="type-dateline landing-dateline" style={{ opacity: datelineVisible ? 1 : 0 }}>
          Claude Self-Reflect / v8.2 / Rust Engine / Local Memory Archive
        </p>

        <section className="bento-grid" aria-label="Claude Self-Reflect overview">
          <ForgettingCard />
          <BentoCard className="card--demo" delay={40}>
            <CardKicker number="—" label="SEE IT IN ACTION" />
            <h2 className="type-hl-md">The Demo</h2>
            <p className="type-dateline mt-2">Real queries, real results — from actual sessions</p>
            <img
              src="/claude-self-reflect/images/csr-demo.gif"
              alt="CSR demo showing semantic search, cross-project search, and activity timeline"
              className="demo-gif"
              loading="lazy"
              width={800}
              height={448}
              style={{ width: '100%', height: 'auto', borderRadius: 6, marginTop: 12 }}
            />
          </BentoCard>
          <ActiveMemoryCard />
          <SearchCard />
          <ImportCard />
          <OneBinaryCard />
          <PipelineCard />
          <PrivacyCard />
          <InstallCard />
          <WhatYouAskCard />
          <ArchCard />
          <WhatsNewCard />
          <div className="bento-section-header">
            <span />
            <h2>How others do it</h2>
            <span />
          </div>
          <FlatFileCard />
          <KnowledgeGraphCard />
          <HostedMemoryCard />
        </section>

        <GoDeeper />

        <footer className="landing-footer" style={{ opacity: footerVisible ? 1 : 0 }}>
          <div className="landing-footer__inner">
            <p className="landing-footer__copy">(c) 2026 Claude Self-Reflect. Built for long-term context. MIT License.</p>
            <nav className="landing-footer__links" aria-label="Footer">
              <a className="landing-footer__link" href="https://github.com/ramakay/claude-self-reflect">GitHub</a>
              <a className="landing-footer__link" href="https://www.npmjs.com/package/claude-self-reflect">npm</a>
              <Link className="landing-footer__link" to="/docs/why-csr">Documentation</Link>
            </nav>
          </div>
        </footer>
      </main>
    </div>
  )
}
