// The pane's picture, painted here: shapes in, pixels out. No DOM, no canvas, no engine picture.
// Every edge is a distance, so every edge is smooth at any size.
import { FACES } from './glyphs'
import type { FaceData } from './glyphs'

/** A colour as 0xRRGGBB. */
export type Rgb = number

type Paint = { color: Rgb; alpha?: number }

/**
 * One thing in the drawing, in the drawing's own units. An angle is a turn, clockwise
 * from the top: 0 is twelve o'clock, 0.25 is three.
 */
export type Shape =
  | ({ kind: 'disc'; x: number; y: number; r: number } & Paint)
  | ({ kind: 'ring'; x: number; y: number; r: number; width: number } & Paint)
  | ({ kind: 'arc'; x: number; y: number; r: number; from: number; to: number; width: number } & Paint)
  | ({ kind: 'line'; x1: number; y1: number; x2: number; y2: number; width: number } & Paint)
  | ({ kind: 'glow'; x: number; y: number; r: number } & Paint)
  /** A box with round corners: filled (top to bottom from `color` to `color2` when given), or only its edge when `stroke` is set. */
  | ({ kind: 'rect'; x: number; y: number; w: number; h: number; r: number; stroke?: number; color2?: Rgb } & Paint)
  /** A soft shadow under a round box: `blur` units of fall-off. */
  | ({ kind: 'shade'; x: number; y: number; w: number; h: number; r: number; blur: number } & Paint)
  /**
   * The pane's ground under the whole image: `ground`, then radial layers, each centred at
   * (`x`, `y`) of a page `top` units above this image, its stops [offset 0..1, rgb, alpha 0..1].
   */
  | {
      kind: 'backdrop'
      top: number
      ground: Rgb
      layers: { x: number; y: number; r: number; stops: [number, Rgb, number][] }[]
    }
  /** The ground under a line through `points` (left to right) down to `base`. */
  | ({ kind: 'area'; points: [number, number][]; base: number } & Paint)
  /** A disc with a bite out of it: the bite is a disc at (`bx`, `by`) of radius `br`. */
  | ({ kind: 'moon'; x: number; y: number; r: number; bx: number; by: number; br: number } & Paint)
  | ({
      kind: 'text'
      x: number
      y: number
      text: string
      size: number
      face: keyof typeof FACES
      anchor?: 'start' | 'middle' | 'end'
      spacing?: number
      /** The cell view only: the ground under these characters (a bar's track). Pixels ignore it. */
      back?: Rgb
    } & Paint)

/** A whole drawing: its size in units and its shapes, the first drawn first. */
export type Scene = { width: number; height: number; shapes: Shape[] }

/** A painted frame: straight (not premultiplied) RGBA, row by row. */
export type Frame = { width: number; height: number; rgba: Uint8Array }

/** The most bytes an Image takes, decoded. */
export const MAX_IMAGE_BYTES = 2 * 1024 * 1024

const TURN = Math.PI * 2

/** The point at `turn` on a circle: clockwise from the top. */
export function at(x: number, y: number, r: number, turn: number): { x: number; y: number } {
  return { x: x + r * Math.sin(turn * TURN), y: y - r * Math.cos(turn * TURN) }
}

// ---- letters ---------------------------------------------------------------------------------------

type Face = { data: FaceData; bytes: Uint8Array; offsets: number[]; index: Map<string, number> }
const faces = new Map<string, Face>()

const fromBase64 = (text: string): Uint8Array => Uint8Array.from(atob(text), c => c.charCodeAt(0))

function faceOf(name: keyof typeof FACES): Face {
  const had = faces.get(name)
  if (had) return had
  const data = FACES[name]
  const offsets: number[] = []
  let offset = 0
  for (const advance of data.advances) {
    offsets.push(offset)
    offset += advance * data.height
  }
  const index = new Map<string, number>()
  Array.from(data.chars).forEach((ch, i) => index.set(ch, i))
  const face = { data, bytes: fromBase64(data.data), offsets, index }
  faces.set(name, face)
  return face
}

/** How wide `text` is at `size` units of line height, in units. */
export function textWidth(text: string, size: number, face: keyof typeof FACES, spacing = 0): number {
  const f = faceOf(face)
  const k = size / f.data.height
  let width = 0
  for (const ch of Array.from(text)) width += (f.data.advances[f.index.get(ch) ?? 0] ?? 0) * k + spacing
  return width - (text.length > 0 ? spacing : 0)
}

// ---- the canvas ------------------------------------------------------------------------------------

/** Premultiplied colour, 0 to 255 a channel, alpha 0 to 1. */
type Canvas = { width: number; height: number; px: Float32Array }

function put(c: Canvas, i: number, r: number, g: number, b: number, a: number): void {
  const keep = 1 - a
  c.px[i] = r * a + (c.px[i] ?? 0) * keep
  c.px[i + 1] = g * a + (c.px[i + 1] ?? 0) * keep
  c.px[i + 2] = b * a + (c.px[i + 2] ?? 0) * keep
  c.px[i + 3] = a + (c.px[i + 3] ?? 0) * keep
}

/** Paints where `distance` is under zero, one pixel of soft edge, inside the box given. */
function fill(
  c: Canvas,
  x0: number,
  y0: number,
  x1: number,
  y1: number,
  paint: Paint,
  cover: (x: number, y: number) => number,
  hole?: { x0: number; y0: number; x1: number; y1: number },
): void {
  const r = (paint.color >> 16) & 255
  const g = (paint.color >> 8) & 255
  const b = paint.color & 255
  const alpha = paint.alpha ?? 1
  const left = Math.max(0, Math.floor(x0))
  const right = Math.min(c.width - 1, Math.ceil(x1))
  const top = Math.max(0, Math.floor(y0))
  const bottom = Math.min(c.height - 1, Math.ceil(y1))
  for (let y = top; y <= bottom; y++) {
    // A ring-like shape paints nothing inside its hole: those pixels are not visited.
    const inHole = hole !== undefined && y > hole.y0 && y < hole.y1
    for (let x = left; x <= right; x++) {
      if (inHole && x > (hole?.x0 ?? 0) && x < (hole?.x1 ?? 0)) {
        x = Math.ceil(hole?.x1 ?? x) - 1
        continue
      }
      const a = cover(x + 0.5, y + 0.5) * alpha
      if (a > 0.003) put(c, (y * c.width + x) * 4, r, g, b, a > 1 ? 1 : a)
    }
  }
}

const edge = (distance: number) => (distance <= -0.5 ? 1 : distance >= 0.5 ? 0 : 0.5 - distance)

function drawText(c: Canvas, s: Extract<Shape, { kind: 'text' }>, scale: number): void {
  const f = faceOf(s.face)
  const k = (s.size * scale) / f.data.height
  const spacing = (s.spacing ?? 0) * scale
  const chars = Array.from(s.text)
  const width = textWidth(s.text, s.size, s.face, s.spacing ?? 0) * scale
  let pen = s.x * scale - (s.anchor === 'middle' ? width / 2 : s.anchor === 'end' ? width : 0)
  const top = s.y * scale - f.data.ascent * k
  const r = (s.color >> 16) & 255
  const g = (s.color >> 8) & 255
  const b = s.color & 255
  const alpha = s.alpha ?? 1
  const h = f.data.height
  for (const ch of chars) {
    const at = f.index.get(ch)
    const advance = at === undefined ? 0 : (f.data.advances[at] ?? 0)
    const offset = at === undefined ? 0 : (f.offsets[at] ?? 0)
    if (advance > 0) {
      const x0 = Math.max(0, Math.floor(pen))
      const x1 = Math.min(c.width - 1, Math.ceil(pen + advance * k))
      const y0 = Math.max(0, Math.floor(top))
      const y1 = Math.min(c.height - 1, Math.ceil(top + h * k))
      for (let y = y0; y <= y1; y++) {
        // The source rows and columns this pixel covers: averaged, so small letters stay even.
        const sy0 = (y - top) / k
        const sy1 = (y + 1 - top) / k
        for (let x = x0; x <= x1; x++) {
          const sx0 = (x - pen) / k
          const sx1 = (x + 1 - pen) / k
          let sum = 0
          let area = 0
          for (let sy = Math.max(0, Math.floor(sy0)); sy < Math.min(h, Math.ceil(sy1)); sy++) {
            const wy = Math.min(sy + 1, sy1) - Math.max(sy, sy0)
            for (let sx = Math.max(0, Math.floor(sx0)); sx < Math.min(advance, Math.ceil(sx1)); sx++) {
              const wx = Math.min(sx + 1, sx1) - Math.max(sx, sx0)
              sum += (f.bytes[offset + sy * advance + sx] ?? 0) * wx * wy
              area += wx * wy
            }
          }
          const full = (sx1 - sx0) * (sy1 - sy0)
          const a = full > 0 ? (sum / 255 / full) * alpha : 0
          if (area > 0 && a > 0.003) put(c, (y * c.width + x) * 4, r, g, b, a > 1 ? 1 : a)
        }
      }
    }
    pen += advance * k + spacing
  }
}

/** The colour of radial stops at `t` (0 at the centre, 1 at the radius): rgb and alpha. */
function stopAt(stops: [number, Rgb, number][], t: number): [number, number, number, number] {
  const first = stops[0]
  const last = stops[stops.length - 1]
  if (!first || !last) return [0, 0, 0, 0]
  const rgb = (c: Rgb, a: number): [number, number, number, number] => [(c >> 16) & 255, (c >> 8) & 255, c & 255, a]
  if (t <= first[0]) return rgb(first[1], first[2])
  if (t >= last[0]) return rgb(last[1], last[2])
  for (let i = 1; i < stops.length; i++) {
    const a = stops[i - 1]
    const b = stops[i]
    if (!a || !b || t > b[0]) continue
    const u = b[0] > a[0] ? (t - a[0]) / (b[0] - a[0]) : 0
    const [ar, ag, ab] = rgb(a[1], 0)
    const [br, bg, bb] = rgb(b[1], 0)
    return [ar + (br - ar) * u, ag + (bg - ag) * u, ab + (bb - ab) * u, a[2] + (b[2] - a[2]) * u]
  }
  return rgb(last[1], last[2])
}

/** A filled round box, painted only on the pixel rows from `y0` to `y1` (pixels). */
function drawClipped(c: Canvas, s: Extract<Shape, { kind: 'rect' }>, k: number, y0: number, y1: number): void {
  const x0 = s.x * k
  const top = s.y * k
  const w = s.w * k
  const h = s.h * k
  const r = Math.min(s.r * k, w / 2, h / 2)
  const cx = x0 + w / 2
  const cy = top + h / 2
  fill(c, x0 - 1, Math.max(top - 1, Math.floor(y0)), x0 + w + 1, Math.min(top + h + 1, Math.ceil(y1) - 1), s, (px, py) => {
    if (py < y0 || py >= y1) return 0
    const qx = Math.abs(px - cx) - w / 2 + r
    const qy = Math.abs(py - cy) - h / 2 + r
    return edge(Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - r)
  })
}

/** `thin` is the least a stroke is wide and a dot is across, in pixels: a grid of dots loses anything finer. */
function drawShape(c: Canvas, s: Shape, k: number, thin = 0): void {
  switch (s.kind) {
    case 'disc': {
      const x = s.x * k
      const y = s.y * k
      const r = Math.max(s.r * k, thin * 0.6)
      fill(c, x - r - 1, y - r - 1, x + r + 1, y + r + 1, s, (px, py) => edge(Math.hypot(px - x, py - y) - r))
      return
    }
    case 'ring': {
      const x = s.x * k
      const y = s.y * k
      const r = s.r * k
      const half = Math.max(s.width * k, thin) / 2
      const out = r + half + 1
      fill(c, x - out, y - out, x + out, y + out, s, (px, py) => edge(Math.abs(Math.hypot(px - x, py - y) - r) - half))
      return
    }
    case 'arc': {
      const span = s.to - s.from
      if (span <= 0) return
      if (span >= 0.999) return drawShape(c, { kind: 'ring', x: s.x, y: s.y, r: s.r, width: s.width, color: s.color, alpha: s.alpha }, k, thin)
      const x = s.x * k
      const y = s.y * k
      const r = s.r * k
      const half = Math.max(s.width * k, thin) / 2
      const mid = ((s.from + s.to) / 2) * TURN
      const open = (span / 2) * TURN
      // The arc's own frame: `m` points at its middle, `n` across it. Round ends come with the distance.
      const mx = Math.sin(mid)
      const my = -Math.cos(mid)
      const sx = Math.sin(open) * r
      const sy = Math.cos(open) * r
      const sin = Math.sin(open)
      const cos = Math.cos(open)
      const out = r + half + 1
      fill(c, x - out, y - out, x + out, y + out, s, (px, py) => {
        const vx = px - x
        const vy = py - y
        const qx = Math.abs(vx * -my + vy * mx)
        const qy = vx * mx + vy * my
        const d = cos * qx > sin * qy ? Math.hypot(qx - sx, qy - sy) : Math.abs(Math.hypot(qx, qy) - r)
        return edge(d - half)
      })
      return
    }
    case 'line': {
      const ax = s.x1 * k
      const ay = s.y1 * k
      const bx = s.x2 * k
      const by = s.y2 * k
      const half = Math.max(s.width * k, thin) / 2
      const dx = bx - ax
      const dy = by - ay
      const len2 = dx * dx + dy * dy || 1
      const pad = half + 1
      fill(c, Math.min(ax, bx) - pad, Math.min(ay, by) - pad, Math.max(ax, bx) + pad, Math.max(ay, by) + pad, s, (px, py) => {
        const t = Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / len2))
        return edge(Math.hypot(px - ax - dx * t, py - ay - dy * t) - half)
      })
      return
    }
    case 'glow': {
      const x = s.x * k
      const y = s.y * k
      const r = s.r * k
      fill(c, x - r, y - r, x + r, y + r, s, (px, py) => {
        const d = Math.hypot(px - x, py - y) / r
        return d >= 1 ? 0 : (1 - d) * (1 - d)
      })
      return
    }
    case 'shade': {
      const x0 = s.x * k
      const y0 = s.y * k
      const w = s.w * k
      const h = s.h * k
      const r = Math.min(s.r * k, w / 2, h / 2)
      const blur = Math.max(1, s.blur * k)
      const cx = x0 + w / 2
      const cy = y0 + h / 2
      fill(c, x0 - blur, y0 - blur, x0 + w + blur, y0 + h + blur, s, (px, py) => {
        const qx = Math.abs(px - cx) - w / 2 + r
        const qy = Math.abs(py - cy) - h / 2 + r
        const d = Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - r
        // CSS-style outer shadow, as herdr: the glass's own area is not darkened.
        if (d < 0) return 0
        const t = Math.max(0, Math.min(1, (d + blur / 2) / blur))
        return (1 - t) * (1 - t)
      }, { x0: x0 + r, y0: y0 + r, x1: x0 + w - r, y1: y0 + h - r })
      return
    }
    case 'backdrop': {
      // The layers are smooth: worked out on a grid every STEP pixels, filled in between.
      const STEP = 4
      const gw = Math.ceil(c.width / STEP) + 1
      const gh = Math.ceil(c.height / STEP) + 1
      const grid = new Float32Array(gw * gh * 3)
      const g = s.ground
      for (let gy = 0; gy < gh; gy++) {
        for (let gx = 0; gx < gw; gx++) {
          let r = (g >> 16) & 255
          let gg = (g >> 8) & 255
          let b = g & 255
          const ux = (gx * STEP + 0.5) / k
          const uy = (gy * STEP + 0.5) / k + s.top
          for (const l of s.layers) {
            const [cr, cg, cb, ca] = stopAt(l.stops, Math.hypot(ux - l.x, uy - l.y) / l.r)
            r = cr * ca + r * (1 - ca)
            gg = cg * ca + gg * (1 - ca)
            b = cb * ca + b * (1 - ca)
          }
          const i = (gy * gw + gx) * 3
          grid[i] = r
          grid[i + 1] = gg
          grid[i + 2] = b
        }
      }
      for (let y = 0; y < c.height; y++) {
        const gy = Math.floor(y / STEP)
        const ty = (y - gy * STEP) / STEP
        for (let x = 0; x < c.width; x++) {
          const gx = Math.floor(x / STEP)
          const tx = (x - gx * STEP) / STEP
          const i00 = (gy * gw + gx) * 3
          const i10 = i00 + 3
          const i01 = i00 + gw * 3
          const i11 = i01 + 3
          const o = (y * c.width + x) * 4
          for (let ch = 0; ch < 3; ch++) {
            const top = (grid[i00 + ch] ?? 0) * (1 - tx) + (grid[i10 + ch] ?? 0) * tx
            const bot = (grid[i01 + ch] ?? 0) * (1 - tx) + (grid[i11 + ch] ?? 0) * tx
            c.px[o + ch] = top * (1 - ty) + bot * ty
          }
          c.px[o + 3] = 1
        }
      }
      return
    }
    case 'rect': {
      if (s.color2 !== undefined && s.stroke === undefined) {
        const top = s.y * k
        const span = Math.max(1, s.h * k)
        const a = s.color
        const z = s.color2
        const mix = (sh: number, t: number) => Math.round(((a >> sh) & 255) * (1 - t) + ((z >> sh) & 255) * t)
        // One band of rows a colour: a gradient this short needs no finer step.
        for (let band = 0; band < 8; band++) {
          const t = (band + 0.5) / 8
          const color = (mix(16, t) << 16) | (mix(8, t) << 8) | mix(0, t)
          const y0 = top + (span * band) / 8
          const y1 = top + (span * (band + 1)) / 8
          drawClipped(c, { ...s, color, color2: undefined }, k, y0, y1)
        }
        return
      }
      const x0 = s.x * k
      const y0 = s.y * k
      const w = s.w * k
      const h = s.h * k
      const r = Math.min(s.r * k, w / 2, h / 2)
      const cx = x0 + w / 2
      const cy = y0 + h / 2
      const half = s.stroke === undefined ? 0 : Math.max(s.stroke * k, thin) / 2
      fill(c, x0 - 1 - half, y0 - 1 - half, x0 + w + 1 + half, y0 + h + 1 + half, s, (px, py) => {
        const qx = Math.abs(px - cx) - w / 2 + r
        const qy = Math.abs(py - cy) - h / 2 + r
        const d = Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - r
        return edge(s.stroke === undefined ? d : Math.abs(d) - half)
      }, s.stroke === undefined ? undefined : { x0: x0 + r + half + 1, y0: y0 + r + half + 1, x1: x0 + w - r - half - 1, y1: y0 + h - r - half - 1 })
      return
    }
    case 'area': {
      const pts = s.points.map(([x, y]) => [x * k, y * k] as const)
      if (pts.length < 2) return
      const base = s.base * k
      const first = pts[0] ?? [0, 0]
      const last = pts[pts.length - 1] ?? [0, 0]
      let top = base
      for (const [, y] of pts) top = Math.min(top, y)
      let i = 0
      fill(c, first[0], top - 1, last[0], base, s, (px, py) => {
        if (px < first[0] || px > last[0]) return 0
        while (i > 0 && (pts[i]?.[0] ?? 0) > px) i--
        while (i < pts.length - 2 && (pts[i + 1]?.[0] ?? 0) < px) i++
        const a = pts[i] ?? first
        const b = pts[i + 1] ?? last
        const t = b[0] > a[0] ? (px - a[0]) / (b[0] - a[0]) : 0
        const y = a[1] + (b[1] - a[1]) * t
        return py > base ? 0 : edge(y - py)
      })
      return
    }
    case 'moon': {
      const x = s.x * k
      const y = s.y * k
      const r = s.r * k
      const bx = s.bx * k
      const by = s.by * k
      const br = s.br * k
      fill(c, x - r - 1, y - r - 1, x + r + 1, y + r + 1, s, (px, py) =>
        edge(Math.max(Math.hypot(px - x, py - y) - r, br - Math.hypot(px - bx, py - by))),
      )
      return
    }
    case 'text':
      drawText(c, s, k)
  }
}

/** The pixels of `scene` at `scale` pixels a unit. Where nothing is painted the ground stays clear. */
export function paint(scene: Scene, scale: number): Frame {
  const width = Math.max(1, Math.round(scene.width * scale))
  const height = Math.max(1, Math.round(scene.height * scale))
  const c: Canvas = { width, height, px: new Float32Array(width * height * 4) }
  for (const shape of scene.shapes) drawShape(c, shape, scale)
  const rgba = new Uint8Array(width * height * 4)
  for (let i = 0; i < rgba.length; i += 4) {
    const a = c.px[i + 3] ?? 0
    if (a <= 0.003) continue
    rgba[i] = Math.min(255, Math.round((c.px[i] ?? 0) / a))
    rgba[i + 1] = Math.min(255, Math.round((c.px[i + 1] ?? 0) / a))
    rgba[i + 2] = Math.min(255, Math.round((c.px[i + 2] ?? 0) / a))
    rgba[i + 3] = Math.min(255, Math.round(a * 255))
  }
  return { width, height, rgba }
}

/** Bytes as base64, by the runtime's own encoder where it has one. */
export function toBase64(bytes: Uint8Array): string {
  const encode = (bytes as { toBase64?: () => string }).toBase64
  if (typeof encode === 'function') return encode.call(bytes)
  let text = ''
  for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  return btoa(text)
}
