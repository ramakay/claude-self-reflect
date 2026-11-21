// The same cards for the places that take no pixels: SVG for the desktop app and the editor.
import { FACES } from './glyphs'
import { at } from './paint'
import type { Scene, Shape } from './paint'

const hex = (color: number) => '#' + color.toString(16).padStart(6, '0')
const n = (value: number) => String(Math.round(value * 100) / 100)
const escape = (text: string) => text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')

const FAMILY: Record<keyof typeof FACES, string> = {
  sans: "Inter, system-ui, -apple-system, 'Segoe UI', sans-serif",
  bold: "Inter, system-ui, -apple-system, 'Segoe UI', sans-serif",
  serif: "Newsreader, 'Iowan Old Style', Georgia, 'Times New Roman', serif",
}

function svgOf(s: Shape, id: number, width: number, height: number): string {
  if (s.kind === 'backdrop') {
    const layers = s.layers
      .map((l, i) => {
        const stops = l.stops.map(([at, c, a]) => `<stop offset="${n(at)}" stop-color="${hex(c)}" stop-opacity="${n(a)}"/>`).join('')
        return (
          `<radialGradient id="a${id}-${i}" gradientUnits="userSpaceOnUse" cx="${n(l.x)}" cy="${n(l.y - s.top)}" r="${n(l.r)}">${stops}</radialGradient>` +
          `<rect width="${width}" height="${height}" fill="url(#a${id}-${i})"/>`
        )
      })
      .join('')
    return `<rect width="${width}" height="${height}" fill="${hex(s.ground)}"/>${layers}`
  }
  const opacity = s.alpha === undefined || s.alpha >= 1 ? '' : ` opacity="${n(s.alpha)}"`
  switch (s.kind) {
    case 'shade':
      return (
        `<filter id="f${id}" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="${n(s.blur / 3)}"/></filter>` +
        `<rect x="${n(s.x)}" y="${n(s.y)}" width="${n(s.w)}" height="${n(s.h)}" rx="${n(s.r)}" fill="${hex(s.color)}" filter="url(#f${id})"${opacity}/>`
      )
    case 'disc':
      return `<circle cx="${n(s.x)}" cy="${n(s.y)}" r="${n(s.r)}" fill="${hex(s.color)}"${opacity}/>`
    case 'ring':
      return `<circle cx="${n(s.x)}" cy="${n(s.y)}" r="${n(s.r)}" fill="none" stroke="${hex(s.color)}" stroke-width="${n(s.width)}"${opacity}/>`
    case 'arc': {
      if (s.to - s.from >= 0.999) return svgOf({ kind: 'ring', x: s.x, y: s.y, r: s.r, width: s.width, color: s.color, alpha: s.alpha }, id, width, height)
      const a = at(s.x, s.y, s.r, s.from)
      const b = at(s.x, s.y, s.r, s.to)
      const large = s.to - s.from > 0.5 ? 1 : 0
      return `<path d="M${n(a.x)} ${n(a.y)}A${n(s.r)} ${n(s.r)} 0 ${large} 1 ${n(b.x)} ${n(b.y)}" fill="none" stroke="${hex(s.color)}" stroke-width="${n(s.width)}" stroke-linecap="round"${opacity}/>`
    }
    case 'line':
      return `<line x1="${n(s.x1)}" y1="${n(s.y1)}" x2="${n(s.x2)}" y2="${n(s.y2)}" stroke="${hex(s.color)}" stroke-width="${n(s.width)}" stroke-linecap="round"${opacity}/>`
    case 'glow':
      return (
        `<radialGradient id="g${id}"><stop offset="0" stop-color="${hex(s.color)}" stop-opacity="${n(s.alpha ?? 1)}"/>` +
        `<stop offset="1" stop-color="${hex(s.color)}" stop-opacity="0"/></radialGradient>` +
        `<circle cx="${n(s.x)}" cy="${n(s.y)}" r="${n(s.r)}" fill="url(#g${id})"/>`
      )
    case 'rect': {
      if (s.color2 !== undefined && s.stroke === undefined) {
        return (
          `<linearGradient id="g${id}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${hex(s.color)}"/><stop offset="1" stop-color="${hex(s.color2)}"/></linearGradient>` +
          `<rect x="${n(s.x)}" y="${n(s.y)}" width="${n(s.w)}" height="${n(s.h)}" rx="${n(s.r)}" fill="url(#g${id})"${opacity}/>`
        )
      }
      const paint = s.stroke === undefined ? `fill="${hex(s.color)}"` : `fill="none" stroke="${hex(s.color)}" stroke-width="${n(s.stroke)}"`
      return `<rect x="${n(s.x)}" y="${n(s.y)}" width="${n(s.w)}" height="${n(s.h)}" rx="${n(s.r)}" ${paint}${opacity}/>`
    }
    case 'area': {
      const first = s.points[0]
      const last = s.points[s.points.length - 1]
      if (!first || !last) return ''
      const line = s.points.map(([x, y]) => `L${n(x)} ${n(y)}`).join('')
      return `<path d="M${n(first[0])} ${n(s.base)}${line}L${n(last[0])} ${n(s.base)}Z" fill="${hex(s.color)}"${opacity}/>`
    }
    case 'moon':
      return (
        `<mask id="m${id}"><rect x="${n(s.x - s.r)}" y="${n(s.y - s.r)}" width="${n(2 * s.r)}" height="${n(2 * s.r)}" fill="#fff"/>` +
        `<circle cx="${n(s.bx)}" cy="${n(s.by)}" r="${n(s.br)}" fill="#000"/></mask>` +
        `<circle cx="${n(s.x)}" cy="${n(s.y)}" r="${n(s.r)}" fill="${hex(s.color)}" mask="url(#m${id})"${opacity}/>`
      )
    case 'text': {
      const face = FACES[s.face]
      const anchor = s.anchor === undefined || s.anchor === 'start' ? '' : ` text-anchor="${s.anchor}"`
      const spacing = s.spacing ? ` letter-spacing="${n(s.spacing)}"` : ''
      const weight = s.face === 'bold' ? 700 : 500
      return `<text x="${n(s.x)}" y="${n(s.y)}" font-family="${FAMILY[s.face]}" font-size="${n((s.size * face.em) / face.height)}" font-weight="${weight}" fill="${hex(s.color)}"${anchor}${spacing}${opacity}>${escape(s.text)}</text>`
    }
  }
}

/** The drawing as an SVG document. */
export function toSvg(scene: Scene): string {
  const body = scene.shapes.map((s, i) => svgOf(s, i, scene.width, scene.height)).join('')
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${scene.width}" height="${scene.height}" viewBox="0 0 ${scene.width} ${scene.height}">${body}</svg>`
}
