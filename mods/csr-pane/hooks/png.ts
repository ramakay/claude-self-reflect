// A frame as a PNG file, deflated here: the mod's runtime has no zlib. The drawing is clear ground
// and flat colour around soft edges, so two looks back find most repeats: the pixel to the left
// and the pixel a row up. One block of deflate's fixed codes; a megabyte of pixels in, a few tens
// of kilobytes out, which is what a terminal can take many times a second.
import { toBase64 } from './paint'
import type { Frame } from './paint'

const LENGTH_BASE = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258]
const LENGTH_EXTRA = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0]
const DISTANCE_BASE = [
  1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289,
  16385, 24577,
]
const DISTANCE_EXTRA = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13]
const MAX_MATCH = 258
const MIN_MATCH = 4

// For each match length, its code and extra bits, found once.
const lengthCode = new Uint8Array(MAX_MATCH + 1)
for (let length = 3; length <= MAX_MATCH; length++) {
  let code = LENGTH_BASE.length - 1
  while (code > 0 && (LENGTH_BASE[code] ?? 0) > length) code--
  lengthCode[length] = code
}

const crcTable = new Uint32Array(256)
for (let n = 0; n < 256; n++) {
  let c = n
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
  crcTable[n] = c >>> 0
}

function crc32(bytes: Uint8Array, from: number, to: number): number {
  let c = 0xffffffff
  for (let i = from; i < to; i++) c = (crcTable[(c ^ (bytes[i] ?? 0)) & 255] ?? 0) ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

/** Bits out, the low bit first, as deflate packs them. */
class Bits {
  bytes: Uint8Array
  at = 0
  private hold = 0
  private held = 0

  constructor(size: number) {
    this.bytes = new Uint8Array(size)
  }

  /** `count` bits of `value`, low bit first: extra bits and block headers. */
  put(value: number, count: number): void {
    this.hold |= value << this.held
    this.held += count
    while (this.held >= 8) {
      if (this.at >= this.bytes.length) this.grow()
      this.bytes[this.at++] = this.hold & 255
      this.hold >>>= 8
      this.held -= 8
    }
  }

  /** A Huffman code: its high bit goes out first. */
  code(value: number, count: number): void {
    let reversed = 0
    for (let i = 0; i < count; i++) reversed |= ((value >> i) & 1) << (count - 1 - i)
    this.put(reversed, count)
  }

  flush(): void {
    if (this.held > 0) this.put(0, 8 - this.held)
  }

  private grow(): void {
    const next = new Uint8Array(this.bytes.length * 2)
    next.set(this.bytes)
    this.bytes = next
  }
}

function literal(out: Bits, byte: number): void {
  if (byte < 144) out.code(0x30 + byte, 8)
  else out.code(0x190 + byte - 144, 9)
}

function match(out: Bits, length: number, distanceCode: number, distanceExtra: number, distanceBits: number): void {
  const code = lengthCode[length] ?? 0
  const symbol = 257 + code
  if (symbol < 280) out.code(symbol - 256, 7)
  else out.code(0xc0 + symbol - 280, 8)
  const extra = LENGTH_EXTRA[code] ?? 0
  if (extra > 0) out.put(length - (LENGTH_BASE[code] ?? 0), extra)
  out.code(distanceCode, 5)
  if (distanceBits > 0) out.put(distanceExtra, distanceBits)
}

function distanceOf(distance: number): { code: number; extra: number; bits: number } {
  let code = DISTANCE_BASE.length - 1
  while (code > 0 && (DISTANCE_BASE[code] ?? 0) > distance) code--
  return { code, extra: distance - (DISTANCE_BASE[code] ?? 0), bits: DISTANCE_EXTRA[code] ?? 0 }
}

/** The rows of a frame as a zlib stream: each row a filter byte (none) and its pixels. */
function deflate(frame: Frame): Uint8Array {
  const stride = frame.width * 4 + 1
  const raw = new Uint8Array(stride * frame.height)
  for (let y = 0; y < frame.height; y++) {
    raw.set(frame.rgba.subarray(y * frame.width * 4, (y + 1) * frame.width * 4), y * stride + 1)
  }
  const left = distanceOf(4)
  // A row up is out of deflate's reach on a frame wider than 8191 pixels; none is.
  const up = stride <= 32768 ? distanceOf(stride) : undefined
  const out = new Bits(Math.max(1024, raw.length >> 3))
  out.put(0x78, 8)
  out.put(0x01, 8)
  out.put(1, 1)
  out.put(1, 2)
  let i = 0
  while (i < raw.length) {
    const room = Math.min(MAX_MATCH, raw.length - i)
    let a = 0
    if (i >= 4) while (a < room && raw[i + a] === raw[i + a - 4]) a++
    let b = 0
    if (up && i >= stride) while (b < room && raw[i + b] === raw[i + b - stride]) b++
    if (up && b >= MIN_MATCH && b >= a) {
      match(out, b, up.code, up.extra, up.bits)
      i += b
    } else if (a >= MIN_MATCH) {
      match(out, a, left.code, left.extra, left.bits)
      i += a
    } else {
      literal(out, raw[i] ?? 0)
      i++
    }
  }
  out.code(0, 7)
  out.flush()
  let s1 = 1
  let s2 = 0
  for (let k = 0; k < raw.length; k++) {
    s1 = (s1 + (raw[k] ?? 0)) % 65521
    s2 = (s2 + s1) % 65521
  }
  const stream = new Uint8Array(out.at + 4)
  stream.set(out.bytes.subarray(0, out.at))
  stream[out.at] = s2 >>> 8
  stream[out.at + 1] = s2 & 255
  stream[out.at + 2] = s1 >>> 8
  stream[out.at + 3] = s1 & 255
  return stream
}

function chunk(type: string, data: Uint8Array): Uint8Array {
  const out = new Uint8Array(12 + data.length)
  const view = new DataView(out.buffer)
  view.setUint32(0, data.length)
  for (let i = 0; i < 4; i++) out[4 + i] = type.charCodeAt(i)
  out.set(data, 8)
  view.setUint32(8 + data.length, crc32(out, 4, 8 + data.length))
  return out
}

/** The frame as a whole PNG file: 8 bits a channel, with alpha. */
export function toPng(frame: Frame): Uint8Array {
  const head = new Uint8Array(13)
  const view = new DataView(head.buffer)
  view.setUint32(0, frame.width)
  view.setUint32(4, frame.height)
  head[8] = 8
  head[9] = 6
  const parts = [Uint8Array.of(137, 80, 78, 71, 13, 10, 26, 10), chunk('IHDR', head), chunk('IDAT', deflate(frame)), chunk('IEND', new Uint8Array(0))]
  const file = new Uint8Array(parts.reduce((n, p) => n + p.length, 0))
  let at = 0
  for (const p of parts) {
    file.set(p, at)
    at += p.length
  }
  return file
}

/** A frame as the `source` an Image takes: a PNG, which a terminal takes far faster than raw pixels. */
export function imageSource(frame: Frame): { png: string } {
  return { png: toBase64(toPng(frame)) }
}
