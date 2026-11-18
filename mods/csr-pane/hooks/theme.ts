// The pane's colours, taken from the herdr pane's own tokens (csr-engine companion render/theme.rs,
// the "dark" and "light" sets), so the two panes read as one product.
import type { Rgb } from './paint'

export type Palette = {
  name: 'dark' | 'light'
  /** The card's fill and its alpha over the terminal's ground. */
  card: Rgb
  cardAlpha: number
  /** The card's hairline edge and its alpha. */
  hair: Rgb
  hairAlpha: number
  ink: Rgb
  quiet: Rgb
  faint: Rgb
  mint: Rgb
  sky: Rgb
  pink: Rgb
  gold: Rgb
  amber: Rgb
  risk: Rgb
  asked: Rgb
  claude: Rgb
  track: Rgb
  band: Rgb
  /** A chip's fill and alpha: the kind tag and the commit stamp. */
  chip: Rgb
  chipAlpha: number
  /** The pane's ground and its three aurora layers (theme.rs `mesh_ground`, `aurora`). */
  mesh: Rgb
  aurora: [number, Rgb, number][][]
  /** The glass card: top and bottom fill, and its alpha over the aurora. */
  glass: [Rgb, Rgb]
  glassAlpha: number
  /** The line of light along a card's top edge. */
  highlight: Rgb
  highlightAlpha: number
  /** The card's ambient shadow: drop, blur, colour, alpha. */
  shadow: { y: number; blur: number; color: Rgb; alpha: number }
  /**
   * The cell view's flat colours: the page's ground, a card's fill, the lighter row along its
   * top. Kept clear of a terminal's own dark or white, which a terminal may draw in their place.
   */
  cells: { ground: Rgb; card: Rgb; top: Rgb }
}

const rgb = (r: number, g: number, b: number): Rgb => (r << 16) | (g << 8) | b

export const DARK: Palette = {
  name: 'dark',
  card: rgb(29, 34, 41),
  cardAlpha: 0.94,
  hair: 0xffffff,
  hairAlpha: 23 / 255,
  ink: rgb(242, 244, 246),
  quiet: rgb(184, 193, 203),
  faint: rgb(150, 160, 172),
  mint: rgb(155, 232, 194),
  sky: rgb(124, 196, 255),
  pink: rgb(255, 151, 209),
  gold: rgb(232, 197, 71),
  amber: rgb(242, 182, 50),
  risk: rgb(255, 163, 140),
  asked: rgb(184, 173, 255),
  claude: rgb(217, 119, 87),
  track: rgb(58, 66, 77),
  band: rgb(130, 150, 173),
  chip: 0xffffff,
  chipAlpha: 0.08,
  mesh: rgb(17, 20, 24),
  aurora: [
    [
      [0, rgb(14, 58, 58), 217 / 255],
      [1, rgb(14, 58, 58), 0],
    ],
    [
      [0, rgb(29, 42, 92), 191 / 255],
      [1, rgb(29, 42, 92), 0],
    ],
    [
      [0, rgb(44, 90, 46), 140 / 255],
      [1, rgb(44, 90, 46), 0],
    ],
  ],
  glass: [rgb(31, 36, 43), rgb(26, 31, 38)],
  glassAlpha: 184 / 255,
  highlight: 0xffffff,
  highlightAlpha: 15 / 255,
  shadow: { y: 6, blur: 18, color: 0, alpha: 89 / 255 },
  cells: { ground: rgb(15, 17, 21), card: rgb(39, 44, 53), top: rgb(54, 61, 73) },
}

export const LIGHT: Palette = {
  name: 'light',
  card: rgb(255, 255, 255),
  cardAlpha: 0.97,
  hair: rgb(23, 25, 28),
  hairAlpha: 35 / 255,
  ink: rgb(23, 25, 28),
  quiet: rgb(69, 77, 88),
  faint: rgb(92, 101, 114),
  mint: rgb(26, 109, 78),
  sky: rgb(32, 91, 147),
  pink: rgb(162, 27, 103),
  gold: rgb(121, 85, 0),
  amber: rgb(132, 86, 0),
  risk: rgb(169, 53, 27),
  asked: rgb(104, 68, 164),
  claude: rgb(204, 90, 40),
  track: rgb(212, 219, 223),
  band: rgb(128, 145, 163),
  chip: rgb(23, 25, 28),
  chipAlpha: 0.06,
  mesh: rgb(247, 247, 245),
  aurora: [
    [
      [0, rgb(95, 218, 150), 245 / 255],
      [0.35, rgb(95, 218, 150), 245 / 255],
      [1, rgb(95, 218, 150), 0],
    ],
    [
      [0, rgb(180, 155, 241), 235 / 255],
      [0.35, rgb(180, 155, 241), 235 / 255],
      [1, rgb(180, 155, 241), 0],
    ],
    [
      [0, rgb(250, 177, 115), 225 / 255],
      [0.35, rgb(250, 177, 115), 225 / 255],
      [1, rgb(250, 177, 115), 0],
    ],
  ],
  glass: [rgb(255, 255, 255), rgb(249, 249, 249)],
  glassAlpha: 248 / 255,
  highlight: 0xffffff,
  highlightAlpha: 191 / 255,
  shadow: { y: 8, blur: 24, color: rgb(16, 24, 40), alpha: 20 / 255 },
  cells: { ground: rgb(226, 229, 224), card: rgb(246, 246, 243), top: rgb(236, 238, 233) },
}

/** The colour as CSS: `#rrggbb`. */
export const css = (color: Rgb): string => '#' + color.toString(16).padStart(6, '0')
