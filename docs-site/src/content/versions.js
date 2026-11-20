// Doc versions. 10.1 is current and keeps the `/docs/<slug>` routes, so existing links work.
// 9.x is the v9.5.8 snapshot in ./v9, served at `/v9/docs/<slug>`; its text is not edited.
import { navigation, pages } from './index'
import { navigation as v9Navigation, pages as v9Pages } from './v9/index'

const V9_PREFIX = '/v9'

function prefixed(path) {
  return `${V9_PREFIX}${path}`
}

export const versions = {
  current: {
    id: 'current',
    label: '10.1 (current)',
    prefix: '',
    navigation,
    pages,
  },
  v9: {
    id: 'v9',
    label: '9.x (previous)',
    prefix: V9_PREFIX,
    navigation: v9Navigation.map(g => ({ ...g, items: g.items.map(i => ({ ...i, href: prefixed(i.href) })) })),
    pages: Object.fromEntries(
      Object.entries(v9Pages).map(([path, page]) => [prefixed(path), { ...page, path: prefixed(path) }])
    ),
  },
}

/** The version a route belongs to. */
export function versionOf(pathname) {
  return pathname === V9_PREFIX || pathname.startsWith(`${V9_PREFIX}/`) ? versions.v9 : versions.current
}

/** The `/docs/<slug>` part of a route, whatever its version. */
export function slugPath(pathname) {
  return pathname.startsWith(`${V9_PREFIX}/`) ? pathname.slice(V9_PREFIX.length) : pathname
}

/** The marketing landing (repo site/), one level above the docs app: /claude-self-reflect/. */
export const LANDING_URL = import.meta.env.BASE_URL.replace(/docs\/$/, '')

/** A version's docs index route: `/` for 10.1, `/v9` for 9.x. */
export function homeOf(version) {
  return version.prefix || '/'
}

/** The same page in another version, or that version's first page when it has no such page. */
export function sameIn(target, pathname) {
  if (pathname === '/' || pathname === V9_PREFIX) return homeOf(target)
  const path = `${target.prefix}${slugPath(pathname)}`
  return target.pages[path] ? path : target.navigation[0].items[0].href
}

/** Turn a doc image path into a URL under the app's base, so a base change needs no content edits. */
export function imageUrl(src) {
  if (!src || /^([a-z]+:|\/\/)/i.test(src)) return src
  const rel = src.replace(/^\/claude-self-reflect\//, '').replace(/^\.?\//, '')
  return `${import.meta.env.BASE_URL}${rel}`
}
