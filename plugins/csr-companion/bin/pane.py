"""The CSR pane and the dreamer's memory, read from csr-engine.

Read-only against csr-engine, no model calls here. The pane is a page of paper in the terminal's own
type (every row takes a click), the session's gauges on top, then each section that has something to
report, in its own tint beside a pixel isometric icon:
  Touching now         code Claude edited in the last hour, and code it read there that others depend
                       on or your past sessions worked on.
  Decisions at risk    past sessions' work on code a later commit changed: what they concluded may not hold.
  Your rules           corrections you typed yourself in this repo (never Claude's own words).
  What if...           the questions CSR's dreamer asked while Claude worked.
  l: Changed since     every commit under your past sessions (Claude gets a Heads up there).

  pane.py --repo PATH --json                    status counts + rows, for the mod
  pane.py --repo PATH --watch [--sid ID] [--agent PANE]  full-screen pane: d main, l log, s summary, q quits;
      a click opens a row, a second click puts it in Claude's prompt (herdr pane PANE)
  pane.py --repo PATH --png DIR [--sid ID]    write the pane's pictures, for a look
  pane.py --repo PATH --note FILE [--sid ID]  what CSR remembers about FILE, as notes for the band
  pane.py --repo PATH --brief --sid ID        what CSR remembers near this session, for the dreamer
  pane.py --repo PATH --keep JSON             add a dream, or an answer to one, to the dream log
"""
import argparse
import base64
import bisect
import fcntl
import glob
import io
import json
import math
import os
import re
import select
import shutil
import signal
import sqlite3
import struct
import subprocess
import sys
import termios
import threading
import time
import tty
from collections import Counter
from datetime import datetime, timezone

HOME = os.environ.get('HOME', '')
DB = os.path.join(HOME, '.claude-self-reflect', 'csr-engine.db')
VERB = {'superseded_by': 'changed', 'anchor_obsolete': 'removed', 'anchor_reinstated': 'came back'}
NESTED = ('/.claude/worktrees/', '/.worktrees/')
CSR_TOOL = 'mcp__claude-self-reflect__'
try:
    import PIL  # noqa: F401  the pictures need Pillow; without it the pane draws text strips
    HAS_PIL = True
except ImportError:
    HAS_PIL = False
PIXELS = HAS_PIL and os.environ.get('TERM_PROGRAM', '') in ('herdr', 'kitty', 'ghostty', 'WezTerm')
SETUP = 'CSR is not set up yet: npx claude-self-reflect setup'
IDLE = 15 * 60  # a gap longer than this is drawn as a break, not as time
KEEP = os.path.join(HOME, '.claude-self-reflect', 'csr-companion')
DREAM_LOG = os.path.join(KEEP, 'dreams.jsonl')  # the mod's own: csr-engine's DB stays read-only
# Every prompt the dreamer writes for you carries this sentence (dreamer.ts askClaude); csr-engine's
# provenance guard matches it too. Such a prompt is the dreamer's words, never your voice.
DREAMER_SAID = "CSR's dreamer asked this while you worked"
PANES = os.path.join(KEEP, 'panes')  # <session>.json: the pid of the pane open beside that session
STATUS_LINES = os.path.join(KEEP, 'statusline')  # <session>.json: Claude's status line, moved into the pane


def epoch(ts):
    if not ts:
        return None
    text = str(ts).strip().replace(' ', 'T', 1)
    if text.endswith('Z'):
        text = text[:-1] + '+00:00'
    try:
        when = datetime.fromisoformat(text)
    except ValueError:
        return None
    if when.tzinfo is None:
        when = when.replace(tzinfo=timezone.utc)
    return when.timestamp()


def repo_root(path):
    out = subprocess.run(['git', '-C', path, 'rev-parse', '--show-toplevel'],
                         capture_output=True, text=True)
    return out.stdout.strip() if out.returncode == 0 else None


def connect():
    con = sqlite3.connect(f'file:{DB}?mode=ro', uri=True, timeout=5)
    con.execute('pragma busy_timeout=5000')
    return con


# ---------------------------------------------------------------- changes since past sessions

_SHIFTS = {}


def load_shifts(con, root):
    """The ledger's verdicts under root, read at most once a minute: every file's notes read them."""
    hit = _SHIFTS.get(root)
    if hit and time.time() - hit[0] < 60:
        return hit[1]
    _SHIFTS[root] = (time.time(), read_shifts(con, root))
    return _SHIFTS[root][1]


def read_shifts(con, root):
    floor = con.execute("select coalesce((select cast(value as integer) from meta "
                        "where key='witness_verdicts_v2_floor'), 0)").fetchone()[0]
    rows = con.execute("""
      select l.file, l.symbol, v.verdict, v.receipt_oid,
             coalesce(v.receipt_committed_at, v.created_at), v.id, v.witness_id
      from witness_verdicts v join witness_ledger l on l.id = v.witness_id
      where v.id > ?1 and v.receipt_oid is not null and trim(v.receipt_oid) != ''
        and l.file like ?2
        and v.id = (select max(v2.id) from witness_verdicts v2 where v2.witness_id = v.witness_id)
    """, (floor, root.rstrip('/') + '/%')).fetchall()
    shifts = {}
    for file, symbol, verdict, receipt, when, vid, wid in rows:
        rel = file[len(root) + 1:]
        if any(n in '/' + rel for n in NESTED):
            continue  # a nested worktree is its own repo
        key = (rel, symbol, verdict, receipt)
        shift = shifts.setdefault(key, dict(file=rel, path=file, symbol=symbol, verdict=verdict,
                                            verb=VERB.get(verdict, verdict), oid=receipt,
                                            date=(when or '')[:10], at=epoch(when), id=vid, wids=[]))
        shift['wids'].append(wid)
        shift['id'] = max(shift['id'], vid)
    ordered = sorted(shifts.values(), key=lambda s: (s['date'], s['id']), reverse=True)
    subjects = {}
    for shift in ordered:
        oid = shift['oid']
        if oid not in subjects:
            out = subprocess.run(['git', '-C', root, 'log', '-1', '--format=%s', oid],
                                 capture_output=True, text=True)
            subjects[oid] = out.stdout.strip() if out.returncode == 0 else ''
        shift['subject'] = subjects[oid]
    return floor, ordered


def title_of(prompt):
    text = re.sub(r'\[Pasted text #\d+[^\]]*\]', ' ', prompt or '')
    text = re.sub(r'https?://(\S+)', lambda m: m.group(1).split('/')[0] + '/…/' + m.group(1).rstrip('/').split('/')[-1]
                  if m.group(1).count('/') > 1 else m.group(1), text)
    return ' '.join(text.split())


_TITLES = {'mtime': None, 'first': {}}


def first_prompts():
    """Each session's first prompt that is not a slash command, from Claude Code's history."""
    path = os.path.join(HOME, '.claude', 'history.jsonl')
    try:
        mtime = os.path.getmtime(path)
    except OSError:
        return {}
    if _TITLES['mtime'] != mtime:
        first = {}
        with open(path, 'rb') as handle:
            for line in handle:
                try:
                    entry = json.loads(line)
                except ValueError:
                    continue
                sid, text = entry.get('sessionId'), (entry.get('display') or '').strip()
                if sid and text and not text.startswith('/') and sid not in first:
                    first[sid] = text
        _TITLES.update(mtime=mtime, first=first)
    return _TITLES['first']


def load_changes(con, shifts, session_id=None):
    """Changed since: the latest past conversation that shaped a symbol, then a commit that changed
    it after that conversation's last message. A commit that landed while the conversation was
    still going is its own work (or one it watched land), not news to it."""
    changes, ends = {}, {}

    def ended(conv):
        if conv not in ends:
            row = con.execute('select max(timestamp) from chunks where conversation_id=?', (conv,)).fetchone()
            ends[conv] = epoch(row[0]) if row and row[0] else None
        return ends[conv]

    for shift in shifts:
        at = shift['at']
        if at is None:
            continue
        found = []
        if shift['symbol']:
            found += con.execute('select session_id, created_at from episode_anchors where file=? and name=?',
                                 (shift['path'], shift['symbol'])).fetchall()
            found += con.execute('select last_conv_id, last_seen from code_nodes where file=? and name=?',
                                 (shift['path'], shift['symbol'])).fetchall()
        for wid in shift['wids']:
            found += con.execute('select c.conversation_id, c.timestamp from witness_chunk_bindings w '
                                 'join chunks c on c.id = w.chunk_id where w.witness_id=?', (wid,)).fetchall()
        talks = [(epoch(ts), conv) for conv, ts in found
                 if conv and epoch(ts) and epoch(ts) <= at and (ended(conv) or at) < at]
        if not talks:
            continue
        talked, conv = max(talks)
        talked = min(talked, ended(conv) or talked)  # a re-import can stamp a link after the talk ended
        change = changes.setdefault(conv, dict(conversation=conv, talked=talked, shifts=[]))
        change['talked'] = max(change['talked'], talked)
        change['shifts'].append(shift)
    out, titles = [], first_prompts()
    for conv, change in changes.items():
        row = con.execute('select first_prompt, first_ts from session_registry where session_id=?', (conv,)).fetchone()
        prompt = titles.get(conv) or (row[0] if row and row[0] and not row[0].startswith('/') else None)
        change['title'] = title_of(prompt) if prompt else f'session {conv[:8]}'
        change['started'] = epoch(row[1]) if row else None
        change['current'] = conv == session_id
        change['changed'] = max(s['at'] for s in change['shifts'])
        change['commits'] = len({s['oid'] for s in change['shifts']})
        out.append(change)
    return sorted(out, key=lambda d: (d['changed'], d['talked']), reverse=True)


def short_date(t):
    return datetime.fromtimestamp(t).strftime('%m-%d')


def quoted(text, width):
    text = ' '.join((text or '').split())
    return '\u201c' + (text if len(text) <= width else text[:width - 1].rstrip() + '\u2026') + '\u201d'


def main_checkout(root):
    out = subprocess.run(['git', '-C', root, 'rev-parse', '--path-format=absolute', '--git-common-dir'],
                         capture_output=True, text=True)
    common = out.stdout.strip()
    return os.path.realpath(os.path.dirname(common)) if out.returncode == 0 and common.endswith('/.git') else None


def has_commit(root, oid):
    return subprocess.run(['git', '-C', root, 'merge-base', '--is-ancestor', oid, 'HEAD'],
                          capture_output=True).returncode == 0


def notes_for(con, root, path, session_id=None):
    """Notes for one file, best first: a past session's work on it that a later commit changed,
    then a rule the person set for it. Each carries its receipts; no model calls."""
    rel = path[len(root) + 1:] if path.startswith(root + '/') else os.path.basename(path)
    name = os.path.basename(path)
    _, shifts = load_shifts(con, root)
    mine = [shift for shift in shifts if shift['path'] == path]
    main = main_checkout(root)
    if main and main != root:
        # A worktree: the dreamer judged the main checkout, so read the same file
        # there, and keep a change only if this worktree's history has its commit.
        _, theirs = load_shifts(con, main)
        mine += [shift for shift in theirs if shift['file'] == rel and has_commit(root, shift['oid'])]
    notes = []
    for change in load_changes(con, mine, session_id):
        if change['current']:
            continue  # this session's own work is not news to it
        latest = max(change['shifts'], key=lambda s: s['at'])
        same = [s for s in change['shifts'] if s['oid'] == latest['oid']]
        symbols = list(dict.fromkeys(s['symbol'] or name for s in same))
        verb = {'changed': 'changed', 'removed': 'was removed', 'came back': 'came back'}[latest['verb']]
        lead = f"`{symbols[0]}`" + (f" and {len(symbols) - 1} more" if len(symbols) > 1 else '')
        them = 'them' if len(symbols) > 1 else 'it'
        if latest['verb'] == 'changed' and len(symbols) > 1:
            verb = 'changed'
        talked, changed = short_date(change['talked']), latest['date'][5:]
        line = (f"{lead} in {name} {verb} in {latest['oid'][:7]} on {changed}, "
                f"after your {talked} session worked on {them}.")
        text = (f"On {talked} you asked {quoted(change['title'], 90)}. That session worked on "
                f"{', '.join(f'`{x}`' for x in symbols[:4])}{' and more' if len(symbols) > 4 else ''} in {rel}. "
                f"On {changed}, commit {latest['oid'][:7]} {quoted(latest['subject'], 70)} {verb.replace('was ', '')} "
                f"{them}. What that session concluded about {them} may no longer hold, so check the code, not the memory.")
        sketch = [f"{talked}  \u25cf your session  {quoted(change['title'], 34)}",
                  f"{changed}  \u25b2 {latest['oid'][:7]}  {', '.join(symbols[:3])}{' +' + str(len(symbols) - 3) if len(symbols) > 3 else ''}"]
        notes.append(dict(id=f"shift:{rel}:{latest['oid'][:12]}:{change['conversation'][:8]}", kind='shift', file=rel,
                          line=line, title=f"Your {talked} work on {symbols[0]} has changed since",
                          text=text, sketch=sketch, symbol=symbols[0], oid=latest['oid'],
                          conversation=change['conversation'], at=latest['at']))
    for when, quote, session in con.execute(
            "select ts, quote, session_id from intent_events where kind='correction' and file is not null "
            "and file != '' and (?1 = file or ?1 like '%/' || file) order by ts desc limit 3", (path,)).fetchall():
        if session == session_id:
            continue
        day = (epoch(when) and short_date(epoch(when))) or (when or '')[:10]
        notes.append(dict(id=f"rule:{rel}:{(when or '')[:19]}", kind='rule', file=rel,
                          line=f"On {day} you told the agent about {name}: {quoted(quote, 110)}",
                          title=f"You set a rule for {name}",
                          text=f"On {day}, in session {session[:8]}, you corrected the agent about {rel}: {quoted(quote, 240)} "
                               f"CSR keeps that correction; it has not been retracted since.",
                          sketch=[], symbol=None, oid=None, conversation=session, at=epoch(when) or 0))
    notes.sort(key=lambda n: (n['kind'] != 'rule', -n['at']))
    return notes


def reminded(con, floor):
    v2_start = con.execute('select min(created_at) from witness_verdicts where id > ?1', (floor,)).fetchone()[0]
    carried = con.execute("select count(*) from correction_deliveries "
                          "where delivered_at >= datetime('now', '-7 days')").fetchone()[0]
    told = con.execute("select count(*) from dream_deliveries where delivered_at >= datetime('now', '-7 days') "
                       "and delivered_at >= ?1", (v2_start or '9999',)).fetchone()[0]
    return carried, told


def load(root, limit, session_id=None):
    con = connect()
    try:
        floor, shifts = load_shifts(con, root)
        changes = load_changes(con, shifts, session_id)
        carried, told = reminded(con, floor)
        recalled = con.execute('select count(*) from retrieval_events where session_id=?',
                               (session_id or '',)).fetchone()[0]
    finally:
        con.close()
    dreams, look, chapter = load_dream_log(root), last_look(root), last_chapter(root, session_id)
    week = [d for d in dreams if d['at'] >= time.time() - 7 * 86400]
    return dict(repo=root, total=len(shifts), shifts=shifts[:limit], changes=changes, dreams_on=dreams_on(),
                recalled=recalled, 
                changes_total=len(changes), dreams=dreams, look=look, chapter=chapter, dreams_7d=len(week),
                dream_tokens_7d=sum(d.get('tokens') or 0 for d in week),
                corrections_carried_7d=carried, shifts_told_7d=told, reminded_7d=carried + told)


def kept_file(folder, session_id):
    return os.path.join(folder, f'{session_id}.json') if session_id and re.fullmatch(r'[\w-]+', session_id) else None


def claude_status(session_id, now):
    """Claude's status line as its command last drew it, kept for this pane while it is open; None when
    there is none, or it is a day old. Claude Code redraws it only when something happens, so an idle
    session's line is old and still right: the model and the context have not moved."""
    try:
        with open(kept_file(STATUS_LINES, session_id) or '', encoding='utf-8') as handle:
            kept = json.load(handle)
        line = str(kept.get('line') or '').strip('\n')
        return line if line and now - float(kept.get('at') or 0) < 86400 else None
    except (OSError, ValueError, TypeError, AttributeError):
        return None


# ---------------------------------------------------------------- the masthead's clocks

def claude_fields(session_id, now):
    """The raw fields the status line wrapper kept beside the line (prompt_cache, rate_limits,
    context_window); {} when there are none or they are a day old. Like the line, an idle session's
    fields are old and still right: the clocks inside them are absolute times."""
    try:
        with open(kept_file(STATUS_LINES, session_id) or '', encoding='utf-8') as handle:
            kept = json.load(handle)
        fields = kept.get('fields')
        return fields if isinstance(fields, dict) and now - float(kept.get('at') or 0) < 86400 else {}
    except (OSError, ValueError, TypeError, AttributeError):
        return {}


def seconds_of(value):
    """A time Claude Code gave as epoch seconds, or as a date and time; None when it is neither, or is not
    a time a clock could show (past the year 9999, before 1970)."""
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        value = float(value) / 1000 if value > 1e11 else float(value)  # milliseconds, in case
    elif isinstance(value, str):
        value = epoch(value)
    else:
        return None
    return value if value is not None and math.isfinite(value) and 0 <= value < 253402300000 else None


def percent_of(value):
    """A number as the status line gave it; None for anything else, a NaN or an infinity."""
    ok = isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)
    return value if ok else None


def cache_clock(cache, now):
    """The prompt cache as a clock: how long it stays warm, or how much the next turn re-reads once cold.
    (text, ink name) or None when the status line told nothing about it."""
    if not isinstance(cache, dict):
        return None
    expires, warm, cold_cost = seconds_of(cache.get('expires_at')), cache.get('warm'), percent_of(cache.get('recache_tokens_if_cold'))
    if warm is False or (expires is not None and expires <= now):
        return ('cache cold' + (f' · re-reads ~{short(cold_cost)}' if cold_cost and cold_cost > 0 else ''), 'turn')
    if expires is not None and expires - now > 86400:
        expires = None  # no prompt cache lives a day: that clock is wrong, so say only that it is warm
    if expires is None:
        return ('cache warm' + (f' · {cache["ttl"]}' if isinstance(cache.get('ttl'), str) else ''), None) if warm else None
    left = expires - now
    return (f'cache warm · {span(left)} left' if left >= 60 else 'cache warm · under 1m left', None if left >= 300 else 'dream')


def quota_pieces(limits, now):
    """The plan's two windows as "5h 34%"; above 70% with the time the window resets, in ink that warns."""
    pieces = []
    for key, label in (('five_hour', '5h'), ('seven_day', '7d')):
        window = (limits or {}).get(key) if isinstance(limits, dict) else None
        used = percent_of(window.get('used_percentage')) if isinstance(window, dict) else None
        if used is None:
            continue
        resets = seconds_of(window.get('resets_at'))
        if resets is not None and resets <= now:
            continue  # that window has rolled over since the status line last ran: the number is old
        text = f'{label} {round(used)}%'
        if used > 70 and resets is not None:
            try:
                when = datetime.fromtimestamp(resets)
                text += ' resets ' + when.strftime('%H:%M' if resets - now < 86400 else '%a %H:%M')
            except (OverflowError, OSError, ValueError):
                pass  # a reset time this machine cannot show: the percentage alone
        pieces.append((text, 'turn' if used >= 90 else 'dream' if used > 70 else None))
    return pieces


def masthead(fields, now):
    """The masthead's clock rows, each a list of (text, ink name) pieces: the cache clock, then the quotas.
    A row with nothing in it is left out; missing fields draw nothing."""
    fields = fields if isinstance(fields, dict) else {}
    clock = cache_clock(fields.get('prompt_cache'), now)
    return [row for row in ([clock] if clock else [], quota_pieces(fields.get('rate_limits'), now)) if row]


def fit_pieces(text, width):
    """The ' · '-joined pieces of text that fit width, the rightmost dropped first."""
    pieces = text.split(' · ')
    while len(pieces) > 1 and len(' · '.join(pieces)) > width:
        pieces.pop()
    return fit(' · '.join(pieces), width)


def mast_rows(rows, width):
    """The clock rows as lines for the dark masthead: pieces dropped right to left to fit, a piece's ink
    where it has one, a dim middle dot between."""
    hue = lambda name: '\x1b[38;2;%d;%d;%dm' % ON_DARK[name][:3]
    lines = []
    for pieces in rows or ():
        pieces = list(pieces)
        while len(pieces) > 1 and len(' · '.join(text for text, _ in pieces)) > width:
            pieces.pop()
        pieces[0] = (fit(pieces[0][0], width), pieces[0][1])
        dot = f' {hue("label")}·{OFF} '
        lines.append(' ' + dot.join((hue(name) if name else '') + text + OFF for text, name in pieces))
    return lines


# One compaction's cost, in input tokens: the post-compaction context written to cache at twice the price of
# reading, the summary written out at five times, and the context re-read once to be summarised. Each later
# turn then re-reads (context - floor) fewer tokens at a tenth of the price. The floor is this session's own
# median post-compaction size (statusline.js assumed 70k; this pane's sessions measure 17k to 25k).
SUMMARY_OUT = 4500  # tokens of summary text a compaction writes
PAYBACK_UNDER = 15  # a longer payback than this is not worth saying
FLOOR_GUESS = 20000  # post-compaction size when a session's own sizes are missing


def compaction_payback(sizes, context):
    """(turns to break even, this session's median post size, its median compaction seconds) for compacting
    now, from the session's (pre, post, ms) rows; None with no history, or when it would not pay back."""
    posts = sorted(p for _, p, _ in sizes if isinstance(p, (int, float)) and p > 0)
    if not posts or not context:
        return None
    floor = posts[len(posts) // 2] if len(posts) % 2 else (posts[len(posts) // 2 - 1] + posts[len(posts) // 2]) / 2
    saved = (context - floor) * 0.1
    if saved <= 0:
        return None
    turns = (floor * 2 + SUMMARY_OUT * 5 + context * 0.1) / saved
    if turns > PAYBACK_UNDER:
        return None
    spent = sorted(m for _, _, m in sizes if isinstance(m, (int, float)) and m > 0)
    return max(1, round(turns)), floor, spent[len(spent) // 2] / 1000 if spent else None


def payback_phrase(s):
    """"compact repays in ~6t" for the facts row, from this session's compaction rows and its context since
    the last one; '' when there is no compaction history or no payback."""
    sizes = s.get('compact_sizes') or []
    since = max(s.get('compacts') or [0])
    later = [r for r in s.get('replies') or [] if r[0] > since]
    found = compaction_payback(sizes, max(later)[1] if later else 0)  # the newest reply's context
    return f'compact repays in ~{found[0]}t' if found else ''


def dreams_on():
    """The mod's /config row, as settings keep it."""
    try:
        with open(os.path.join(HOME, '.claude', 'settings.json'), encoding='utf-8') as handle:
            configs = json.load(handle).get('pluginConfigs') or {}
    except (OSError, ValueError, AttributeError):
        return False
    return any(name.split('@')[0] == 'csr-companion' and ((config or {}).get('options') or {}).get('dreams') is True
               for name, config in configs.items())


def status_text(data):
    n = data['dreams_7d']
    dreams = f"{n} {'dream' if n == 1 else 'dreams'} this week" if data['dreams_on'] else 'dreams off'
    return f"☾ CSR remembered {data['recalled']}× this session · {dreams}"


def public(data):
    """The JSON the mod reads: no paths or witness ids, changes flattened."""
    def shift(s):
        return {k: s[k] for k in ('file', 'symbol', 'verb', 'oid', 'date', 'subject')}
    changes = [dict(conversation=c['conversation'], title=c['title'], current=c['current'],
                    talked=datetime.fromtimestamp(c['talked']).strftime('%Y-%m-%d'),
                    commits=c['commits'], shifts=[shift(s) for s in c['shifts']][:12])
               for c in data['changes'][:20]]
    dreams = [{k: d.get(k) for k in ('id', 'at', 'line', 'title', 'answer')} for d in data['dreams'][:20]]
    return dict({k: v for k, v in data.items() if k not in ('shifts', 'changes', 'dreams', 'look', 'chapter')},
                shifts=[shift(s) for s in data['shifts']], changes=changes, dreams=dreams)


# ---------------------------------------------------------------- the dreamer's log and memory

def keep(record):
    """One line in the dream log: a dream the mod showed, or what the person answered to it."""
    os.makedirs(KEEP, exist_ok=True)
    with open(DREAM_LOG, 'a', encoding='utf-8') as handle:
        handle.write(json.dumps(record, ensure_ascii=False) + '\n')


SETTLED = ('helpful', 'not_relevant', 'ask_claude', 'thought_of_it', 'got_it', 'dismiss', 'ignored', 'turn_off',
           'expired')
ADOPTED = ('helpful', 'ask_claude')  # a dream you took up: it gives its place in the chapter back
PER_CHAPTER = 5


def load_dream_log(root=None):
    """Dreams newest first, each with the answer that settled it (None while it is open)."""
    dreams = {}
    try:
        with open(DREAM_LOG, encoding='utf-8') as handle:
            lines = handle.read().splitlines()
    except OSError:
        return []
    for line in lines:
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get('kind') == 'dream' and record.get('id') and record.get('line'):
            dreams[record['id']] = dict(record, at=record.get('at') or 0, answer=None)
        elif record.get('kind') == 'answer' and record.get('id') in dreams and record.get('answer') in SETTLED:
            dream = dreams[record['id']]
            # "helpful" or "not relevant" says more than the dismiss that led to it
            if dream['answer'] in (None, 'dismiss', 'got_it', 'ignored') or record['answer'] in ('helpful', 'not_relevant'):
                dream['answer'] = record['answer']
    out = [d for d in dreams.values() if not root or not d.get('repo') or d['repo'] == root]
    return sorted(out, key=lambda d: d['at'], reverse=True)


LOOKED = dict(  # what came of a look, as the pane says it
    dream='it asked a new question',
    quiet="nothing new in CSR's memory near this work, so no look",
    none='nothing worth asking',
    ungrounded='its question stood on no other session, so it was dropped',
    repeat='its question had been asked before, so it was dropped',
    budget="it waits: a look now would pass its share of Claude's tokens",
    skip='it waits: backing off after a question you passed',
    spent="this chapter's dreams are used up; the next compaction starts a new one",
)


def last_chapter(root=None, session_id=None):
    """Where this session's chapter of dreams began, and how many it holds: the mod writes a line at the
    session's start, at each compaction, and after two hours without one. None before the first."""
    try:
        with open(DREAM_LOG, encoding='utf-8') as handle:
            lines = handle.read().splitlines()
    except OSError:
        return None
    for line in reversed(lines):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get('kind') == 'chapter' and record.get('session') == session_id \
                and (not root or record.get('repo') in (None, root)):
            return dict(at=float(record.get('at') or 0), of=int(record.get('of') or PER_CHAPTER))
    return None


def feed(dreams, chapter, session_id):
    """This session's dreams as the pane shows them: how many are left in the chapter, the star (the
    newest, if it is still open and came in this chapter), the adopted ones, and the passed rest."""
    mine = [d for d in dreams if d.get('session') == session_id]  # newest first
    start = chapter['at'] if chapter else 0
    of = chapter['of'] if chapter else PER_CHAPTER
    current = [d for d in mine if d['at'] >= start]
    adopted_here = sum(1 for d in current if d['answer'] in ADOPTED)
    star = mine[0] if mine and mine[0]['answer'] is None and mine[0]['at'] >= start else None
    kept = [d for d in mine if d['answer'] in ADOPTED]
    adopted = kept[:2]
    folded = [d for d in mine if d is not star and d not in adopted]  # newest first
    passed = [d for d in folded if d['answer'] not in ADOPTED]
    return dict(left=max(0, of + adopted_here - len(current)), of=of + adopted_here, star=star, adopted=adopted,
                passed=passed, folded=folded)


def last_look(root=None):
    """The dreamer's latest look near this repo, from the dream log: when, what came of it, and how many
    of Claude's steps until the next. None before its first."""
    try:
        with open(DREAM_LOG, encoding='utf-8') as handle:
            lines = handle.read().splitlines()
    except OSError:
        return None
    for line in reversed(lines):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get('kind') not in ('look', 'dream') or (root and record.get('repo') not in (None, root)):
            continue
        return dict(at=float(record.get('at') or 0), outcome=record.get('outcome') or 'dream',
                    every=record.get('every'))
    return None


# How the person talks to the agent: the dreamer asks in the same voice.
VOICE = (('can we', r'\bcan we\b'), ('what if', r'\bwhat if\b'), ('should we', r'\bshould we\b'),
         ('i want', r'\bi want\b'), ('did you consider/check', r'\bdid you (?:consider|check|think)\b'))
NEAR = re.compile(r"\b(ideally|what if|eventually|imagine|radical|north star|cornerstone|my vision|"
                  r"would be (?:great|nice|amazing)|have you (?:thought|considered)|did you consider|"
                  r"i wish|someday|long term|in (?:the )?future)\b", re.I)


def unit(blob):
    values = struct.unpack(f'{len(blob) // 4}f', blob)
    norm = sum(v * v for v in values) ** 0.5 or 1.0
    return [v / norm for v in values]


def voice_and_lines(con, session_id, limit=4, floor=0.40):
    """The person's asking habits, counted, and their past "what if" style lines that sit closest
    to this session: CSR's stored embeddings, this session's latest chunks against the chunk each
    line was said in. No model calls."""
    counts, lines = Counter(), []
    try:
        handle = open(os.path.join(HOME, '.claude', 'history.jsonl'), 'rb')
    except OSError:
        return counts, []
    with handle:
        for raw in handle:
            try:
                entry = json.loads(raw)
            except ValueError:
                continue
            text = (entry.get('display') or '').strip()
            if not text or text.startswith('/') or DREAMER_SAID in text:
                continue
            low = text.lower()
            for name, pattern in VOICE:
                if re.search(pattern, low):
                    counts[name] += 1
            sid = entry.get('sessionId') or ''
            if sid and sid != session_id:
                for part in re.split(r'(?<=[.?!])\s+|\s+-\s+|\n+', text):
                    part = part.strip()
                    if NEAR.search(part) and 30 <= len(part) <= 240:
                        lines.append((entry.get('timestamp', 0) / 1000, sid, entry.get('project') or '', part))
    now = [unit(blob) for (blob,) in con.execute(
        'select e.embedding from chunks c join chunk_embeddings e on e.chunk_id = c.id '
        'where c.conversation_id = ? order by c.timestamp desc limit 8', (session_id,))]
    if not now:
        return counts, []
    near = []
    for when, sid, project, part in lines:
        row = con.execute('select e.embedding from chunks c join chunk_embeddings e on e.chunk_id = c.id '
                          'where c.conversation_id = ? and instr(c.content, ?) > 0 limit 1',
                          (sid, part[:50])).fetchone()
        if row:
            vector = unit(row[0])
            score = max(sum(a * b for a, b in zip(vector, other)) for other in now)
            if score >= floor:
                near.append((score, when, sid, os.path.basename(project.rstrip('/')), part))
    near.sort(reverse=True)
    return counts, near[:limit]


BUILT = (('skill', re.compile(r'/\.claude/skills/([^/]+)/')),
         ('mod', re.compile(r'/\.claude/dev-mods/[^/]+/([^/]+)/')),
         ('plugin', re.compile(r'(?<!\.claude)/plugins/([^/]+)/')))


def described(folder, kind):
    """A built thing's own one-line description: a skill's SKILL.md, a mod's or plugin's plugin.json."""
    try:
        if kind == 'skill':
            with open(os.path.join(folder, 'SKILL.md'), encoding='utf-8') as handle:
                head = handle.read(4000)
            found = re.search(r'^description:\s*(.+)$', head, re.M)
            return found.group(1).strip().strip('"\'') if found else ''
        with open(os.path.join(folder, '.claude-plugin', 'plugin.json'), encoding='utf-8') as handle:
            return str(json.load(handle).get('description') or '')
    except (OSError, ValueError, AttributeError):
        return ''


def built_near(con, session_id, talk, most=3):
    """Things the person built in other sessions (a skill, a mod, a plugin) that you or Claude named in
    this session's latest words, newest first: the sessions that built each, and its own description.
    Read from the code anchors CSR keeps for each session; no model calls."""
    things = {}
    for sid, file, at in con.execute(
            "select session_id, file, max(created_at) from episode_anchors where session_id != ?1 and "
            "(file like '%/.claude/skills/%' or file like '%/dev-mods/%' or file like '%/plugins/%') "
            "group by session_id, file", (session_id,)):
        for kind, pattern in BUILT:
            found = pattern.search(file)
            if found:
                thing = things.setdefault((kind, found.group(1)), dict(kind=kind, name=found.group(1), sessions={},
                                                                       folder=file[:found.end()]))
                thing['sessions'][sid] = max(thing['sessions'].get(sid, ''), at or '')
                break
    if not things:
        return []
    talk = talk.lower()
    near = [t for t in things.values()
            if len(t['name']) >= 4 and re.search(r'(?<![\w-])' + re.escape(t['name'].lower()) + r'(?![\w-])', talk)]
    near.sort(key=lambda t: max(t['sessions'].values()), reverse=True)
    out = []
    for thing in near[:most]:
        latest = max(thing['sessions'], key=thing['sessions'].get)
        excerpt = ' '.join(described(thing['folder'], thing['kind']).split())
        out.append(dict(thing, latest=latest, at=thing['sessions'][latest], excerpt=excerpt))
    return out


def recent_files(session, now, window=1800, most=8):
    """The files Claude handled in the last half hour, newest first; the most handled when none."""
    recent = sorted((at, path) for path, at in session.last.items() if at and now - at <= window and path in session.files)
    return [path for _, path in reversed(recent)][:most] or [path for path, _ in session.files.most_common(most)]


def brief(con, root, session_id):
    """What CSR remembers near this session, with receipts, for the dreamer's prompt."""
    session = Session(session_id)
    session.read_transcript()
    touched = recent_files(session, time.time())
    out = []
    if touched:
        out.append('Files Claude handled lately, newest first: ' + ', '.join(
            f"{os.path.relpath(path, root) if path.startswith(root + '/') else '/'.join(path.split('/')[-3:])}"
            f" ({session.files[path]}×)" for path in touched) + '.')
    # Lines that start with "* " are what CSR knows about the work in hand: only they count as news.
    notes, seen, roots = [], set(), {}
    for path in touched[:6]:
        folder = os.path.dirname(path)
        if folder not in roots:
            owner = repo_root(folder) if os.path.isdir(folder) else None
            roots[folder] = owner and os.path.realpath(owner)
        if not roots[folder]:
            continue  # not in a git repo: no commits to have changed it
        for note in notes_for(con, roots[folder], path, session_id):
            if note['id'] not in seen:
                seen.add(note['id'])
                notes.append(note)
    changed = [n for n in notes if n['kind'] == 'shift'][:5]
    rules = [n for n in notes if n['kind'] == 'rule'][:3]
    if changed:
        out += ['', "Code that changed after one of the person's past sessions worked on it:"]
        out += [f"* {n['line']} [commit {n['oid'][:7]}, session {n['conversation'][:8]}]" for n in changed]
    if rules:
        out += ['', 'Rules the person set for these files:']
        out += [f"* {n['line']} [session {n['conversation'][:8]}]" for n in rules]
    built = built_near(con, session_id, ' '.join(words for _, words in session.talk[-60:]))
    if built:
        out += ['', 'Things the person built in other sessions that came up in this work:']
        for thing in built:
            others = len(thing['sessions'])
            out.append(f"* {thing['kind']} {thing['name']}: built over {others} {'session' if others == 1 else 'sessions'}, "
                       f"latest {thing['at'][5:10]} [session {thing['latest'][:8]}]")
            if thing['excerpt']:
                out.append(f"  {quoted(thing['excerpt'], 400)}")
    project = os.path.basename(root)
    corrections = [(ts, quote, sid) for ts, quote, sid, transcript, at in con.execute(
        "select ts, quote, session_id, transcript_path, byte_start from intent_events where kind = 'correction' "
        "and session_id != ?1 and (project = ?2 or project like ?2 || '-%') order by ts desc limit 200",
        (session_id, project)).fetchall() if typed_by_you(transcript, at, quote)][:3]  # yours: not Claude's, not pasted
    if corrections:
        out += ['', 'Corrections CSR recorded in this repo lately:']
        out += [f"- {short_date(epoch(ts)) if epoch(ts) else (ts or '')[:10]}: {quoted(quote, 200)} [session {sid[:8]}]"
                for ts, quote, sid in corrections]
    _, near = voice_and_lines(con, session_id)
    if near:
        out += ['', 'Lines the person wrote in past sessions that sit close to this work:']
        out += [f"- {short_date(when)} ({project}): {quoted(part, 220)} [session {sid[:8]}]"
                for _, when, sid, project, part in near]
    dreams = load_dream_log(root)[:8]
    if dreams:
        out += ['', 'Dreams already offered here, and what the person answered:']
        out += [f"- {short_date(d['at'])}: {quoted(d['line'], 200)} [{d['answer'] or 'no answer'}]" for d in dreams]
    return '\n'.join(out).strip()


# ---------------------------------------------------------------- what CSR remembered for you

EDITS = ('Edit', 'Write', 'MultiEdit', 'NotebookEdit')
NAMED = re.compile(r"(/(?:Users|home)/[\w./@+-]+)")  # an absolute path in a command Claude ran
ASK_KEYS = ('query', 'concept', 'topic', 'file_path', 'symbol', 'target', 'pattern', 'conversation_id')
_ABOUT = {}  # a past session or a stored reflection -> (what it was about, when, project), or None


def gist(text):
    """The first sentence of a stored story or request, in plain words."""
    text = text or ''
    request = re.search(r'##\s*User Request\s*\n(.+?)(?:\n\s*\n|\n##|$)', text, re.S)
    if request:
        text = request.group(1)
    text = text.split('\n---')[0]
    text = re.split(r'\s(?:Partially completed|Completed|Unresolved|Outcome)[.:]', text)[0]
    text = re.sub(r'`([^`]*)`', r'\1', text)
    text = re.sub(r'(?:/[\w.@-]+){2,}/([\w.@-]+)', r'\1', text)
    text = re.sub(r'\s*\([^()]*\)', '', text)  # asides in brackets: the gist reads without them
    text = re.sub(r'^(?:The|This) session\s+', '', ' '.join(text.split()))
    if re.fullmatch(r'[a-z0-9]+(?:-[a-z0-9]+)+', text):
        text = text.replace('-', ' ')  # a slug title: words
    first = re.split(r'(?<=[.!?])\s', text, maxsplit=1)[0].rstrip(' .')
    return first[:1].upper() + first[1:] if first else None


def ai_title(conversation):
    """Claude Code's own title for a session, from the last one its transcript holds."""
    found = glob.glob(os.path.join(HOME, '.claude', 'projects', '*', f'{conversation}.jsonl'))
    if not found:
        return None
    try:
        with open(found[0], 'rb') as handle:
            handle.seek(0, 2)
            handle.seek(max(0, handle.tell() - 262144))
            block = handle.read()
    except OSError:
        return None
    at = block.rfind(b'"type":"ai-title"')
    if at < 0:
        return None
    try:
        return json.loads(block[block.rfind(b'\n', 0, at) + 1:block.find(b'\n', at)]).get('aiTitle')
    except ValueError:
        return None


def session_about(con, conversation):
    if conversation in _ABOUT:
        return _ABOUT[conversation]
    seen = con.execute('select first_prompt, first_ts, last_ts, project from session_registry where session_id=?',
                       (conversation,)).fetchone()
    title = ai_title(conversation)
    title = title and gist(title)
    if not title:
        row = con.execute("select content from reflections where tags like ? and tags like '%session_story%' "
                          "order by (id like 'story_%'), timestamp desc limit 1", (f'%conv_{conversation}%',)).fetchone()
        title = row and gist(row[0])
    if not title:
        row = con.execute('select content from reflections where id=?', (f'extracted_v3_{conversation}',)).fetchone()
        title = row and gist(row[0])
    if not title and seen and seen[0] and not seen[0].lstrip().startswith(('[', '<', '/')):
        title = gist(seen[0])
    _ABOUT[conversation] = (title, epoch(seen[2] or seen[1]) if seen else None, seen[3] if seen else None) if title else None
    return _ABOUT[conversation]


def session_story(session_id):
    """This session's story as csr-engine last told it: its words, its revision, who wrote it and when.
    csr-engine retells it after each compaction and at the session's end."""
    if not session_id:
        return None
    con = connect()
    try:
        row = con.execute("select content, tags, timestamp from reflections where tags like ? and "
                          "tags like '%session_story%' order by timestamp desc limit 1",
                          (f'%conv_{session_id}%',)).fetchone()
    finally:
        con.close()
    if not row or not (row[0] or '').strip():
        return None
    try:
        tags = json.loads(row[1] or '[]')
    except ValueError:
        tags = []
    rev = next((int(t[10:]) for t in tags if t.startswith('story_rev_') and t[10:].isdigit()), 1)
    by = 'Haiku' if 'story_by_haiku' in tags else 'extract' if 'story_extracted' in tags else ''
    text = ' '.join(row[0].split())
    return dict(text=text, now=where_it_stands(text), rev=rev, by=by, at=epoch(row[2]))


def where_it_stands(story):
    """A story's ending, from the sentence that says where the work stands (its last two sentences
    when none says it): what the pane shows before a click opens the rest."""
    told = re.split(r'(?<=[.!?])\s+(?=[A-Z`])', story)
    at = next((i for i in range(len(told) - 1, -1, -1)
               if re.search(r'\b(?:stands?|stood|standing)\b', told[i], re.I)), max(0, len(told) - 2))
    return ' '.join(told[at:])


def memory_of(con, kind, memory, conversation, parent):
    """What one recalled memory was: its past session, or the stored reflection itself."""
    if kind == 'reflection':
        named = re.match(r'(?:extracted_v3|story)_([0-9a-f-]{36})$', memory)
        if not named:
            key = 'reflection:' + memory
            if key not in _ABOUT:
                row = con.execute('select content, timestamp from reflections where id=?', (memory,)).fetchone()
                _ABOUT[key] = (gist(row[0]), epoch(row[1]), None) if row and gist(row[0]) else None
            return key, _ABOUT[key]
        conversation = named.group(1)
    elif (conversation or '').startswith('agent-'):
        conversation = parent  # a subagent's chunk: the session that ran it
    if not conversation:
        return None, None
    return conversation, session_about(con, conversation)


def handoffs(times):
    """How many times CSR handed Claude something: one injection carries several chunks, all logged in
    the same second."""
    return len({int(t) for t in times if t})


def remembered_items(con, recalls, questions, session_id):
    """What CSR handed Claude this session, newest first: one row per past session or reflection, each
    with how many hand-offs carried it and whether Claude then opened it (a later CSR request naming
    that session), and each time Claude asked CSR itself."""
    items = {}
    for at, kind, memory, conversation, parent, _ in recalls:
        key, about = memory_of(con, kind, memory, conversation, parent)
        if not about or key == session_id:
            continue
        item = items.setdefault(key, dict(kind='recall', key=key, at=0, first=None, text=about[0], when=about[1],
                                          project=about[2], seconds=set(), opened=False))
        moment = epoch(at) or 0
        item['at'] = max(item['at'], moment)
        item['first'] = moment if item['first'] is None else min(item['first'], moment)
        if moment:
            item['seconds'].add(int(moment))
    for item in items.values():
        item['times'] = len(item.pop('seconds')) or 1
        if re.fullmatch(r'[0-9a-f-]{36}', item['key']):
            item['opened'] = any(when >= (item['first'] or 0) and item['key'][:8] in ask for when, _, ask in questions)
    for when, tool, ask in questions:
        items[('asked', when)] = dict(kind='asked', at=when, text=ask, tool=tool, times=1)
    return sorted(items.values(), key=lambda item: item['at'], reverse=True)


# ---------------------------------------------------------------- this session

INTERRUPTED = re.compile(r'\[Request interrupted by user')  # Claude Code's own marker, not words a person typed


class Session:
    """This session, read as it goes: the transcript from where it was left, CSR's rows fresh."""

    def __init__(self, session_id, path=None):
        self.id = session_id
        found = [path] if path else glob.glob(os.path.join(HOME, '.claude', 'projects', '*', f'{session_id}.jsonl'))
        self.path = found[0] if found else None
        self.offset = 0
        self.replies = {}  # message id -> (time, context tokens, output tokens)
        self.prompts, self.compacts, self.asked, self.questions = [], [], [], []
        self.compact_sizes = []  # (tokens before, tokens after, ms) of each compaction
        self.tools, self.files, self.edited = Counter(), Counter(), set()
        self.shell, self.last = Counter(), {}  # files a command named; when Claude last handled each
        self.talk = []  # (time, words) the person typed or Claude wrote, newest last; no tool output
        self.title = None
        self.summaries = []  # (time, text) of each compaction summary, oldest first
        self.typed = []  # (time, words) of each prompt the person typed, oldest first
        self.started = 0.0  # the first row's time
        self.said = None  # (time, text) of Claude's last words to the person
        self.turn_end = 0.0  # when Claude last finished a turn (stop_reason end_turn)
        self.carried = None  # the session before this one's threads, once read (see carried_threads)

    def read_transcript(self):
        if not self.path:
            return
        try:
            with open(self.path, 'rb') as handle:
                handle.seek(self.offset)
                block = handle.read()
        except OSError:
            return
        end = block.rfind(b'\n') + 1
        self.offset += end
        for line in block[:end].splitlines():
            try:
                entry = json.loads(line)
            except ValueError:
                continue
            kind, when = entry.get('type'), epoch(entry.get('timestamp'))
            self.started = self.started or when or 0.0
            if kind == 'assistant':
                message = entry.get('message') or {}
                if message.get('stop_reason') == 'end_turn' and when and not entry.get('isSidechain'):
                    self.turn_end = when
                usage = message.get('usage') or {}
                context = sum(usage.get(k) or 0 for k in
                              ('input_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens'))
                if message.get('id') and when:
                    self.replies[message['id']] = (when, context, usage.get('output_tokens') or 0,
                                                   usage.get('cache_creation_input_tokens') or 0)
                for part in message.get('content') or []:
                    if isinstance(part, dict) and part.get('type') == 'text' and when and not entry.get('isSidechain'):
                        self.talk.append((when, part.get('text') or ''))
                        self.said = self.talk[-1]
                    if isinstance(part, dict) and part.get('type') == 'tool_use':
                        self.tools[part.get('name')] += 1
                        path = (part.get('input') or {}).get('file_path')
                        if isinstance(path, str) and path.startswith('/'):
                            self.files[os.path.realpath(path)] += 1
                            self.last[os.path.realpath(path)] = when or 0
                            if part.get('name') in EDITS:
                                self.edited.add(os.path.realpath(path))
                        if part.get('name') == 'Bash':
                            for named in set(NAMED.findall(str((part.get('input') or {}).get('command') or ''))):
                                named = named.rstrip('.,;:')
                                self.shell[named] += 1
                                self.last[named] = when or 0
                        if str(part.get('name', '')).startswith(CSR_TOOL) and when:
                            self.asked.append(when)
                            args = part.get('input') or {}
                            ask = next((args[k] for k in ASK_KEYS if isinstance(args.get(k), str) and args[k].strip()), '')
                            ask = os.path.basename(ask) if ask.startswith('/') else ask
                            self.questions.append((when, part['name'][len(CSR_TOOL):], ' '.join(ask.split())))
            elif kind == 'user' and when and not entry.get('isMeta') and not entry.get('isCompactSummary'):
                content = (entry.get('message') or {}).get('content')
                texts = [content] if isinstance(content, str) else [
                    p.get('text', '') for p in content or [] if isinstance(p, dict) and p.get('type') == 'text']
                if any(t.strip() and not t.lstrip().startswith(('<', '/')) for t in texts):
                    self.prompts.append(when)
                    if not entry.get('isSidechain') and not INTERRUPTED.match('\n'.join(texts).lstrip()):
                        self.talk.append((when, own_words('\n'.join(texts))))
                        self.typed.append(self.talk[-1])
            elif kind == 'user' and when and entry.get('isCompactSummary'):
                content = (entry.get('message') or {}).get('content')
                text = content if isinstance(content, str) else '\n'.join(
                    str(p.get('text') or '') for p in content or [] if isinstance(p, dict) and p.get('type') == 'text')
                if text.strip():  # an empty one says nothing about what dropped out
                    self.summaries.append((when, text))
            elif kind == 'attachment' and when:
                queued = entry.get('attachment') or {}
                text = str(queued.get('prompt') or '').strip()
                if queued.get('type') == 'queued_command' and queued.get('humanTurn') and text \
                        and not text.startswith(('<', '/')):
                    self.prompts.append(when)
            elif kind == 'system' and entry.get('subtype') == 'compact_boundary' and when:
                self.compacts.append(when)
                meta = entry.get('compactMetadata') if isinstance(entry.get('compactMetadata'), dict) else {}
                self.compact_sizes.append((meta.get('preTokens'), meta.get('postTokens'), meta.get('durationMs')))
            elif kind == 'ai-title' and entry.get('aiTitle'):
                self.title = entry['aiTitle']
        del self.talk[:-200]

    def load(self):
        self.read_transcript()
        if self.carried is None and self.path:  # once: the session before this one is done being written
            self.carried = []
            threading.Thread(target=carry_into, args=(self,), daemon=True).start()
        con = connect()
        try:
            sid = self.id
            q = lambda sql: con.execute(sql, (sid,)).fetchall()
            learned = dict(
                chunks=[epoch(r[0]) for r in q('select created_at from chunks where conversation_id=?')],
                anchors=q('select created_at, name, file from episode_anchors where session_id=? order by created_at'),
                symbols=q('select count(*) from code_nodes where last_session_id=?')[0][0],
                turns=q('select ts, kind, quote from intent_events where session_id=? order by ts'),
                memories=q('select slug from memory_registry where origin_session_id=?'),
            )
            gave = dict(
                dreams=[epoch(r[0]) for r in q('select delivered_at from dream_deliveries where session_id=?')],
                recalls=[epoch(r[0]) for r in q('select retrieved_at from retrieval_events where session_id=?')],
                carried=[epoch(r[0]) for r in q('select delivered_at from correction_deliveries where target_session_id=?')],
            )
            recalls = q('select r.retrieved_at, r.memory_type, r.memory_id, c.conversation_id, p.source_conv_id, '
                        'c.timestamp from retrieval_events r left join chunks c on c.id = r.memory_id '
                        'left join chunk_provenance p on p.chunk_id = r.memory_id where r.session_id=? '
                        'order by r.retrieved_at')
            here = q('select project from session_registry where session_id=?')
            remembered = remembered_items(con, recalls, self.questions, self.id)
            sessions = con.execute('select count(*) from session_registry').fetchone()[0]
        finally:
            con.close()
        replies = sorted(self.replies.values())
        return dict(id=self.id, title=self.title, replies=replies, prompts=sorted(self.prompts),
                    compacts=sorted(self.compacts), compact_sizes=self.compact_sizes, asked=sorted(self.asked), tools=self.tools, learned=learned,
                    lost=lost_found(self.summaries, self.typed, self.carried or ()),
                    gave=gave, remembered=remembered, sessions=sessions, project=here[0][0] if here else None,
                    files=dict(self.files), edited=sorted(self.edited), shell=dict(self.shell), last=dict(self.last))


class Clock:
    """Session time with idle gaps squeezed out, so the strip shows work, not nights."""

    def __init__(self, stamps, now):
        stamps = sorted(t for t in stamps if t)
        self.starts, self.offsets, self.breaks = [], [], []
        active, last = 0.0, None
        for t in stamps + [now]:
            if last is None or t - last > IDLE:
                if last is not None:
                    active += 60
                    self.breaks.append(active)
                self.starts.append(t)
                self.offsets.append(active)
            else:
                active += t - last
            last = t
        self.total = max(active, 1.0)
        self.begin = stamps[0] if stamps else now

    def at(self, t):
        i = bisect.bisect_right(self.starts, t) - 1
        if i < 0:
            return 0.0
        nxt = self.offsets[i + 1] - 60 if i + 1 < len(self.offsets) else self.total
        return min(self.offsets[i] + (t - self.starts[i]), nxt) / self.total


# ---------------------------------------------------------------- pictures

def _linear(c):
    c /= 255
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def _encoded(c):
    c = c * 12.92 if c <= 0.0031308 else 1.055 * max(c, 0) ** (1 / 2.4) - 0.055
    return max(0, min(255, round(c * 255)))


def to_lab(rgb):
    r, g, b = (_linear(c) for c in rgb[:3])
    x, y, z = ((0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047, 0.2126 * r + 0.7152 * g + 0.0722 * b,
               (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883)
    f = lambda t: t ** (1 / 3) if t > 216 / 24389 else (24389 / 27 * t + 16) / 116
    return 116 * f(y) - 16, 500 * (f(x) - f(y)), 200 * (f(y) - f(z))


def from_lab(lab):
    light, a, b = lab
    fy = (light + 16) / 116
    g = lambda t: t ** 3 if t ** 3 > 216 / 24389 else (116 * t - 16) / (24389 / 27)
    x, y, z = g(fy + a / 500) * 0.95047, g(fy), g(fy - b / 200) * 1.08883
    return (_encoded(3.2406 * x - 1.5372 * y - 0.4986 * z), _encoded(-0.9689 * x + 1.8758 * y + 0.0415 * z),
            _encoded(0.0557 * x - 0.2040 * y + 1.0570 * z))


def blend(a, b, t):
    """a toward b by t, in CIELAB: even steps to the eye, no muddy middle."""
    return from_lab(tuple(p + (q - p) * t for p, q in zip(to_lab(a), to_lab(b))))


def contrast(a, b):
    lum = lambda rgb: sum(w * _linear(c) for w, c in zip((0.2126, 0.7152, 0.0722), rgb[:3]))
    hi, lo = sorted((lum(a), lum(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


def readable(ink, ground, ratio=4.5):
    """The ink, its hue kept, darkened (or lightened on a dark ground) until it reads as text there."""
    light, a, b = to_lab(ink)
    step = -1 if to_lab(ground)[0] > 50 else 1
    while contrast(from_lab((light, a, b)), ground) < ratio and 0 < light < 100:
        light += step
    return from_lab((light, a, b))


# pigment's inks (its nyt archetype; `pigment.py tokens`), the pane's only colours: a page of sheet paper
# in lamp black, each section one pigment for its icon and its marks, made readable on the sheet; the
# masthead in the inks pigment lights for its night ground.
PIGMENT = dict(lamp=(23, 25, 28), graphite=(94, 94, 89), pencil=(141, 140, 134), rule=(226, 225, 219),
               sheet=(251, 251, 249), night=(14, 15, 17), payne=(47, 58, 72), chalk=(238, 245, 250),
               indigo=(44, 90, 160), vermilion=(200, 68, 42), viridian=(46, 139, 106), ochre=(196, 146, 47),
               sepia=(111, 86, 66), indigo_lit=(58, 127, 224), viridian_lit=(36, 163, 114),
               vermilion_lit=(232, 88, 44), ochre_lit=(242, 182, 50), violet_lit=(154, 123, 232))
GROUND = PIGMENT['sheet']
INK = dict(you=PIGMENT['lamp'], ctx=readable(PIGMENT['indigo'], GROUND), learn=readable(PIGMENT['viridian'], GROUND),
           turn=readable(PIGMENT['vermilion'], GROUND), dream=readable(PIGMENT['ochre'], GROUND),
           recall=readable(PIGMENT['viridian'], GROUND), asked=readable(PIGMENT['violet_lit'], GROUND),
           sepia=PIGMENT['sepia'], rule=(0, 0, 0, 34), quiet=PIGMENT['graphite'], faint=PIGMENT['pencil'],
           hair=PIGMENT['rule'])
SECTION = dict(session='you', touching='ctx', risk='turn', rules='sepia', dreams='dream', memory='recall',
               changed='quiet', remembered='recall', kept='turn', story='ctx')
GLYPH = dict(session='◉', touching='‹›', risk='~', rules='◆', dreams='☾', memory='↑', changed='~', remembered='↑',
             kept='◆', story='§')  # where no pictures are drawn
SECTION['coach'], GLYPH['coach'] = 'ctx', '→'  # How Claude read you
DARK = (PIGMENT['payne'], PIGMENT['night'])  # the masthead's ground, from its first row (Claude's own status
# line) to its last (the gauges' legend), each row a step darker in CIELAB; payne sits clear of a dark
# terminal's own ground (herdr's is 40), so the masthead keeps its edge.
ON_DARK = dict(text=PIGMENT['chalk'], label=blend(PIGMENT['chalk'], PIGMENT['payne'], 0.45), you=PIGMENT['chalk'],
               ctx=PIGMENT['indigo_lit'], learn=PIGMENT['viridian_lit'], recall=PIGMENT['viridian_lit'],
               turn=PIGMENT['vermilion_lit'], dream=PIGMENT['ochre_lit'], asked=PIGMENT['violet_lit'],
               rule=(255, 255, 255, 40))
TINT = 0.15  # an opened row: its section's ink at this strength on the paper
HEAD = 0.10  # a section's heading row
ROW = 0.07  # its items, one tint for all of them; deep enough to read as a card, still under HEAD
STAR_DEPTH = 1.5  # What if…, the star: its tints this much deeper
SS = 2  # draw at twice the size, then shrink: PIL draws no anti-aliased edges


def cell_size():
    try:
        rows, cols, xpix, ypix = struct.unpack('HHHH', fcntl.ioctl(sys.stdout.fileno(), termios.TIOCGWINSZ, b'\0' * 8))
        if xpix and ypix and cols and rows:
            return max(6, xpix // cols) * 2, max(12, ypix // rows) * 2
    except OSError:
        pass
    return 16, 34


def canvas(cols, rows, cell):
    from PIL import Image, ImageDraw
    image = Image.new('RGBA', (cols * cell[0] * SS, rows * cell[1] * SS), (0, 0, 0, 0))
    return image, ImageDraw.Draw(image)


def shrink(image):
    from PIL import Image
    small = image.resize((image.width // SS, image.height // SS), Image.LANCZOS)
    out = io.BytesIO()
    small.save(out, 'PNG')
    return out.getvalue()


def session_png(s, cols, cell, now, grounds=None):
    """Three lanes over active session time: your prompts, Claude's context, CSR's memory at work; given
    the dark grounds of its three rows, each lane painted on its own, in the lighter inks."""
    INK = ON_DARK if grounds else globals()['INK']
    image, draw = canvas(cols, 3, cell)
    for i, ground in enumerate(grounds or ()):
        draw.rectangle([0, i * cell[1] * SS, image.width, (i + 1) * cell[1] * SS], fill=ground + (255,))
    w, lane = image.width, image.height / 3
    learned, gave = s['learned'], s['gave']
    clock = Clock(s['prompts'] + [r[0] for r in s['replies']], now)
    x = lambda t: 2 * SS + clock.at(t) * (w - 4 * SS)
    for i in (1, 2):
        draw.line([(0, i * lane), (w, i * lane)], fill=INK['rule'], width=SS)
    for b in clock.breaks:
        bx = 2 * SS + b / clock.total * (w - 4 * SS)
        for y in range(0, int(3 * lane), 6 * SS):
            draw.line([(bx, y), (bx, y + 2 * SS)], fill=(255, 255, 255, 60) if grounds else (0, 0, 0, 60), width=SS)
    for t in s['compacts']:
        draw.line([(x(t), lane), (x(t), 2 * lane)], fill=(255, 255, 255, 90) if grounds else (0, 0, 0, 90), width=SS)
    # you: one tick per prompt
    for t in s['prompts']:
        draw.line([(x(t), lane * 0.2), (x(t), lane * 0.8)], fill=INK['you'], width=2 * SS)
    # context: how full each reply's window was; a compaction drops it
    peak = max([r[1] for r in s['replies']] + [1])
    points = [(x(t), lane * 2 - 2 * SS - (ctx / peak) * (lane - 4 * SS)) for t, ctx, _, _ in s['replies']]
    if len(points) > 1:
        draw.polygon([(points[0][0], lane * 2)] + points + [(points[-1][0], lane * 2)], fill=INK['ctx'] + (70,))
        draw.line(points, fill=INK['ctx'], width=SS)
    # memory: what CSR learned from this session (bars from the floor), what it handed Claude
    # (ticks from the top), Claude asking it (dots), course changes it kept (diamonds)
    top, floor = 2 * lane, 3 * lane - 2 * SS
    bins = Counter(int(x(t) // (4 * SS)) for t in learned['chunks'] if t)
    most = max(bins.values(), default=1)
    for b, n in bins.items():
        h = max(SS, (n / most) * lane * 0.45)
        draw.rectangle([b * 4 * SS, floor - h, b * 4 * SS + 3 * SS, floor], fill=INK['learn'] + (130,))
    for t in gave['recalls'] + gave['carried']:
        draw.line([(x(t), top + lane * 0.1), (x(t), top + lane * 0.5)], fill=INK['recall'], width=SS)
    for t in gave['dreams']:
        r, cx, cy = 4 * SS, x(t), top + lane * 0.3
        draw.polygon([(cx, cy - r), (cx + r, cy + r * 0.8), (cx - r, cy + r * 0.8)], fill=INK['dream'])
    for t in s['asked']:
        r = 2.6 * SS
        draw.ellipse([x(t) - r, top + lane * 0.3 - r, x(t) + r, top + lane * 0.3 + r], fill=INK['asked'])
    for when, _, _ in learned['turns']:
        t, r = epoch(when), 4.5 * SS
        if t:
            cx, cy = x(t), top + lane * 0.5
            draw.polygon([(cx, cy - r), (cx + r, cy), (cx, cy + r), (cx - r, cy)], fill=INK['turn'])
    return shrink(image)


def kitty(png, row, col, cols, rows):
    out = [f'\x1b[{row + 1};{col + 1}H']
    data, first = base64.standard_b64encode(png), True
    while data:
        chunk, data = data[:4096], data[4096:]
        more = 1 if data else 0
        ctrl = f'a=T,f=100,q=2,C=1,c={cols},r={rows},m={more}' if first else f'm={more}'
        out.append(f'\x1b_G{ctrl};{chunk.decode()}\x1b\\')
        first = False
    return ''.join(out)


ICON = (5, 2)  # a section's icon: five cells by two rows, beside its heading and its first row
PAPER = (250, 249, 244)
PAGES = (226, 224, 216)


def mix(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a[:3], b[:3])) + (255,)


def iso_icon(name):
    """A section's icon in pixel art, seen isometric: pigment's nyt inks as flat faces lit from the upper
    left (the top lightest, the right face darkest), each part outlined in a dark of its own ink. Every
    corner sits on even x + y, so each edge is a clean two-across, one-down stair."""
    from PIL import Image, ImageChops, ImageDraw
    size = (44, 44)
    image = Image.new('RGBA', size, (0, 0, 0, 0))
    o = (22, 26)
    at = lambda x, y, z: (o[0] + x - y, o[1] + (x + y) // 2 - z)
    white, black = (255, 255, 255), (0, 0, 0)

    def part(ink, paint):
        layer = Image.new('RGBA', size, (0, 0, 0, 0))
        paint(ImageDraw.Draw(layer))
        alpha = layer.getchannel('A')
        grown = alpha
        for dx, dy in ((1, 0), (-1, 0), (0, 1), (0, -1)):
            grown = ImageChops.lighter(grown, ImageChops.offset(alpha, dx, dy))
        image.paste(mix(ink, black, 0.55), (0, 0) + size, ImageChops.subtract(grown, alpha))
        image.alpha_composite(layer)

    def faces(draw, x, y, z, dx, dy, dz, ink, top=None, left=None, right=None):
        poly = lambda pts, fill: draw.polygon([at(*p) for p in pts], fill=fill, outline=fill)
        poly([(x, y + dy, z), (x + dx, y + dy, z), (x + dx, y + dy, z + dz), (x, y + dy, z + dz)],
             left or mix(ink, white, 0.1))
        poly([(x + dx, y, z), (x + dx, y + dy, z), (x + dx, y + dy, z + dz), (x + dx, y, z + dz)],
             right or mix(ink, black, 0.25))
        poly([(x, y, z + dz), (x + dx, y, z + dz), (x + dx, y + dy, z + dz), (x, y + dy, z + dz)],
             top or mix(ink, white, 0.5))
        if not top:  # the light catches the top's two front edges
            draw.line([at(x, y + dy, z + dz), at(x + dx, y + dy, z + dz), at(x + dx, y, z + dz)],
                      fill=mix(ink, white, 0.78), width=1)

    def box(x, y, z, dx, dy, dz, ink, more=None, **colors):
        def paint(draw):
            faces(draw, x, y, z, dx, dy, dz, ink, **colors)
            if more:
                more(draw)
        part(ink, paint)

    def line(draw, a, b, fill):
        draw.line([at(*a), at(*b)], fill=fill, width=1)

    c, quiet = INK[SECTION[name]], INK['quiet']
    if name in ('touching', 'story'):  # Claude's open files, or the session's story: a sheet on its folder
        box(-2, -2, 0, 14, 12, 2, c)
        box(0, 0, 2, 10, 8, 2, c, top=PAPER + (255,), left=PAGES + (255,), right=mix(PAGES, black, 0.12),
            more=lambda d: [line(d, (2, 2, 4), (8, 2, 4), mix(c, white, 0.2)),
                            line(d, (4, 4, 4), (8, 4, 4), mix(c, white, 0.2)),
                            line(d, (2, 6, 4), (6, 6, 4), INK['turn'][:3] + (255,))])
    elif name in ('risk', 'changed'):  # work a later commit moved: a block slid half off its base
        if name == 'changed':
            box(0, 0, 0, 10, 10, 2, quiet)
            box(2, 0, 2, 10, 10, 2, INK['turn'])
            box(0, 0, 4, 10, 10, 2, quiet)
        else:
            box(0, 0, 0, 8, 8, 6, quiet)
            box(4, 0, 6, 6, 6, 6, c)
    elif name in ('rules', 'kept'):  # a rule book: its cover, its spine banded, its pages, a ribbon
        box(0, 0, 0, 14, 10, 4, c, left=mix(c, black, 0.1), right=PAGES + (255,),
            more=lambda d: [line(d, (14, 0, 2), (14, 10, 2), mix(PAGES, black, 0.15)),
                            line(d, (2, 10, 0), (2, 10, 4), mix(c, white, 0.45)),
                            line(d, (12, 10, 0), (12, 10, 4), mix(c, white, 0.45)),
                            line(d, (14, 6, 4), (14, 6, -1), INK['turn'][:3] + (255,))])
    elif name == 'dreams':  # a crescent over a plinth, a star by it
        box(0, 0, 0, 10, 10, 4, c)
        cx, cy = at(5, 5, 4)
        moon = mix(c, white, 0.3)

        def crescent(d):
            d.ellipse([cx - 6, cy - 14, cx + 3, cy - 5], fill=moon)
            d.ellipse([cx - 3, cy - 16, cx + 6, cy - 7], fill=(0, 0, 0, 0))
        part(c, crescent)
        part(c, lambda d: [d.point(p, fill=moon) for p in
                           ((cx + 6, cy - 11), (cx + 5, cy - 12), (cx + 7, cy - 12), (cx + 6, cy - 13))])
    else:  # memory, remembered: a tray of past sessions, one card risen out of it for Claude
        box(0, 0, 0, 12, 10, 5, c,
            more=lambda d: d.polygon([at(1, 1, 5), at(11, 1, 5), at(11, 9, 5), at(1, 9, 5)], fill=mix(c, black, 0.45)))
        part(c, lambda d: [d.polygon([at(2, 4, 5), at(10, 4, 5), at(10, 4, 13), at(2, 4, 13)], fill=PAPER + (255,)),
                           line(d, (4, 4, 11), (8, 4, 11), mix(c, white, 0.1)),
                           line(d, (4, 4, 9), (6, 4, 9), mix(c, white, 0.1))])
    return image.crop(image.getbbox())


_ICONS = {}
SHADOW = (2, 1)  # an icon's cast shadow: its outline this many pixel-art cells right and down
SHADOW_ALPHA = 56  # 22% black


def cast_shadow(icon):
    """The icon's outline in 22% black, hard-edged like the icon: the shadow a pixel-art drawing casts."""
    from PIL import Image
    shadow = Image.new('RGBA', icon.size, (0, 0, 0, 0))
    shadow.paste((0, 0, 0, SHADOW_ALPHA), mask=icon.getchannel('A').point(lambda a: 255 if a else 0))
    return shadow


def iso_png(name, cell, backs):
    """The icon at its five cells by two rows, its two rows' tints behind it, blown up by whole pixels."""
    key = (name, cell, backs)
    if key in _ICONS:
        return _ICONS[key]
    from PIL import Image
    w, h = ICON[0] * cell[0], ICON[1] * cell[1]
    image = Image.new('RGBA', (w, h), backs[0] + (255,))
    image.paste(backs[1] + (255,), (0, cell[1], w, h))
    icon = iso_icon(name)
    k = max(1, min((w - 4) // icon.width, (h - 4) // icon.height))
    icon = icon.resize((icon.width * k, icon.height * k), Image.NEAREST)
    x, y = (w - icon.width) // 2, (h - icon.height) // 2
    image.alpha_composite(cast_shadow(icon), (x + SHADOW[0] * k, y + SHADOW[1] * k))
    image.alpha_composite(icon, (x, y))
    out = io.BytesIO()
    image.save(out, 'PNG')
    _ICONS[key] = out.getvalue()
    return _ICONS[key]


# ---------------------------------------------------------------- the screen

DIM, BOLD, OFF = '\x1b[2m', '\x1b[1m', '\x1b[0m'
BLACK, GROUND_BACK = '\x1b]11;rgb:fb/fb/f9\x1b\\', '\x1b]111\x1b\\'  # the pane's own paper, and the terminal's back
MOUSE_ON, MOUSE_OFF = '\x1b[?1000h\x1b[?1006h', '\x1b[?1000l\x1b[?1006l'  # clicks, as SGR reports


def ink(name):
    r, g, b = INK[name][:3]
    return f'\x1b[38;2;{r};{g};{b}m'


def tint(name, strength=TINT):
    strength *= STAR_DEPTH if name == 'dreams' else 1
    return tuple(round(p + (i - p) * strength) for i, p in zip(INK[SECTION[name]][:3], GROUND))


def band(name, strength=TINT):
    r, g, b = tint(name, strength)
    return f'\x1b[48;2;{r};{g};{b}m'


FAINT = 3.0  # the meta type's floor: pencil read at 3:1 on paper, and stays there on the items' deeper tint
INK['faint'] = readable(INK['faint'], min((tint(name, ROW) for name in SECTION), key=lambda c: to_lab(c)[0]), FAINT)


COLOUR = re.compile(r'\x1b\[[0-9;]*m')


def ansi_fit(text, width):
    """text cut to width cells, its colours kept, an ellipsis where it was cut."""
    if len(COLOUR.sub('', text)) <= width:
        return text
    out, n, at = [], 0, 0
    for match in list(COLOUR.finditer(text)) + [None]:
        chunk = text[at:match.start()] if match else text[at:]
        if n + len(chunk) > width - 1:
            return ''.join(out) + chunk[:width - 1 - n] + '…' + OFF
        out.append(chunk)
        n += len(chunk)
        if not match:
            break
        out.append(match.group())
        at = match.end()
    return ''.join(out)


def ansi_wrap(text, width, rows):
    """A status line over as many rows as it needs, broken at its │ separators, colours kept."""
    lines, line = [], ''
    for part in text.split('│'):
        joined = line + '│' + part if line else part
        if line and len(COLOUR.sub('', joined).rstrip()) > width:
            lines.append(line.rstrip())
            line = part.lstrip(' ')
        else:
            line = joined
    lines.append(line.rstrip())
    return [ansi_fit(l, width) for l in lines if COLOUR.sub('', l).strip()][:rows]


def dark_steps(n):
    """n grounds stepping evenly, in CIELAB, from the masthead's first shade to its last."""
    top, bottom = DARK
    return [blend(top, bottom, i / max(1, n - 1)) for i in range(n)]


LIP_ROWS = 30  # the lip under the masthead shows when the pane is at least this tall


def masthead_lip(rows):
    """One bare row between the masthead and the first section, between their two grounds: the soft edge a
    shadow would give. None where the pane is too short to spare the row."""
    if rows < LIP_ROWS:
        return []
    return [tinted('', '\x1b[48;2;%d;%d;%dm' % blend(DARK[1], PAPER, 0.6))]


def on_dark(line, ground):
    """A masthead row on its dark ground, run to the pane's edge, light type laid again after each reset."""
    back, fg = '\x1b[48;2;%d;%d;%dm' % ground, '\x1b[38;2;%d;%d;%dm' % ON_DARK['text']
    return back + fg + line.replace(OFF, OFF + back + fg) + '\x1b[K' + OFF


def tinted(line, back):
    """The line on a background that runs to the pane's edge, laid again after each reset in it."""
    return back + line.replace(OFF, OFF + back) + '\x1b[K' + OFF


class Lines(list):
    """Lines for the screen, and what a click on each one acts on."""

    def __init__(self, rows=()):
        super().__init__(rows)
        self.acts = {}

    def put(self, rows, act=None):
        if act:
            self.acts.update({len(self) + i: act for i in range(len(rows))})
        self.extend(rows)
        return self


def one_line(text, most=700):
    return fit(' '.join(str(text).split()), most)


def fit_blocks(out, blocks, room, ui, tail=True):
    """A section's rows, (rows, act) newest first, under its heading and within its room. A row a click
    opened always shows: newer ones above it give way, said in a line, and it is cut to the room if it
    must be, its last line (what a second click does) kept."""
    at = next((k for k, (_, act) in enumerate(blocks) if act and act['key'] == ui.get('open')), None)
    first = 0
    while at is not None and first < at and len(out) + 1 + sum(len(rows) for rows, _ in blocks[first:at + 1]) > room:
        first += 1
    if first:
        out.put([' ' * INDENT + DIM + f'+{first} newer' + OFF])
    shown = first
    for k in range(first, len(blocks)):
        rows, act = blocks[k]
        more = len(blocks) - k - 1
        if k == at:
            if len(out) + len(rows) > room:
                rows = rows[:max(1, room - len(out) - 1)] + rows[-1:]
        elif len(out) + len(rows) + (1 if more and tail else 0) > room:
            break
        out.put(rows, act)
        shown += 1
    if tail and shown < len(blocks) and len(out) < room:
        out.put([' ' * INDENT + DIM + f'+{len(blocks) - shown} earlier' + OFF])
    return out


def card_edge(rows, name, indent):
    """An opened block as a card: each row on the deep tint with a left border cell in the section's ink,
    just left of the text; the bottom row steps back to the items' tint (page_lines tints it as a plain row)."""
    edge = ink(SECTION[name]) + '▌' + OFF
    lines = [row[:indent - 1] + edge + row[indent:] for row in rows]
    return [tinted(line, band(name)) for line in lines[:-1]] + lines[-1:]


def opened(rows, name, detail, width, ui, key, indent=None):
    """A row a click opened: its whole text, what more CSR knows, and what a second click does."""
    sent = ui.get('sent')
    if sent and sent[0] == key:
        hint = "✓ now in Claude's prompt" if sent[1] else "✗ couldn't reach Claude's pane"
    else:
        hint = "↳ click again → Claude's prompt"
    indent = INDENT if indent is None else indent
    room = width - indent
    more = [' ' * indent + DIM + line + OFF for text in detail for line in wrap(text, room, 2)][:4]
    last = ' ' * indent + ink(SECTION[name]) + fit(hint, room) + OFF
    return card_edge(rows + more + [last], name, indent)


STAR = ink('asked') + '☾' + OFF  # CSR's mark, as in the band above Claude's prompt
MARK = {'changed': '\x1b[33m~', 'removed': '\x1b[31m-', 'came back': '\x1b[32m+'}
ANSWER_MARK = dict(helpful='\x1b[32m✓', ask_claude='\x1b[36m→', thought_of_it='\x1b[2m=', not_relevant='\x1b[31m✗',
                   got_it='\x1b[35m·', dismiss='\x1b[2m·', ignored='\x1b[2m·', turn_off='\x1b[2m○',
                   expired='\x1b[2m·')
ANSWER_WORD = dict(helpful='helpful', ask_claude='asked Claude', thought_of_it="you'd thought of it",
                   not_relevant='not relevant', got_it='read', dismiss='dismissed', ignored='passed',
                   turn_off='turned dreams off', expired='folded at a compaction')


def fit(text, width):
    return text if len(text) <= width else text[:max(1, width - 1)] + '…'


def short(n):
    for size, unit in ((1e9, 'B'), (1e6, 'M'), (1e3, 'k')):
        if n >= size:
            value = n / size
            return (f'{value:.1f}'.rstrip('0').rstrip('.') if value < 10 else f'{value:.0f}') + unit
    return str(int(n))


def ago(seconds):
    days = seconds / 86400
    return f'{int(seconds // 3600)}h' if days < 1 else f'{int(days)}d'


def span(seconds):
    hours, minutes = int(seconds // 3600), int(seconds % 3600 // 60)
    return f'{hours}h{minutes:02d}m' if hours else f'{minutes}m'


def from_when(t, now):
    """How long ago a remembered session was, the way you would say it."""
    if not t:
        return ''
    days = (now - t) / 86400
    if days < 1:
        return 'earlier today'
    if days < 2:
        return 'yesterday'
    if days < 14:
        return f'{int(days)} days ago'
    if days < 60:
        return f'{int(days // 7)} weeks ago'
    return f'{int(days // 30)} months ago'


def stamp(t, now, before=None, lit=''):
    """An entry's time: "now" for the last two minutes, HH:MM today, MM-DD earlier, blank when the
    entry above it already says it."""
    if now - t < 120:
        label = 'now'
    else:
        when = datetime.fromtimestamp(t)
        label = when.strftime('%H:%M') if when.date() == datetime.fromtimestamp(now).date() else when.strftime('%m-%d')
    return ('', '') if label == before else (label, lit if label == 'now' else '')


def wrap(text, width, rows):
    words, lines, line = text.split(), [], ''
    for word in words:
        if line and len(line) + 1 + len(word) > width:
            lines.append(line)
            line = ''
        line = f'{line} {word}' if line else word
    lines.append(line)
    if len(lines) > rows:
        lines = lines[:rows]
        lines[-1] = fit(lines[-1] + ' …', width)
    return [fit(l, width) for l in lines]


INDENT = 8  # "HH:MM ↑ "


def entry(when, mark, text, tail, width, rows=2):
    """One entry: its time and coloured mark, the text over at most `rows` lines, a dim tail at its end.
    `when` is (text, ink): "now" is drawn in the mark's colour, a repeated time not at all."""
    room = width - INDENT
    lines = wrap(text, room, rows)
    if tail:
        if len(lines[-1]) + 1 + len(tail) <= room:
            lines[-1] = f'{lines[-1]} {DIM}{tail}{OFF}'
        elif len(lines) < rows:
            lines.append(DIM + fit(tail, room) + OFF)
        else:
            lines[-1] = f'{fit(lines[-1], max(4, room - len(tail) - 1))} {DIM}{tail}{OFF}'
    label, lit = when if isinstance(when, tuple) else (when, '')
    head = (lit + BOLD if lit else DIM) + label.ljust(6) + OFF
    return [head + mark + OFF + ' ' + lines[0]] + [' ' * INDENT + line for line in lines[1:]]


GUTTER = 6  # a section's text starts here, its icon beside


def heading(name, title, right, width):
    """A section's head: its icon's place (its mark where the terminal draws no pictures), its title in
    its ink, a count at the right."""
    lead = ' ' + ('' if PIXELS else GLYPH[name]).ljust(GUTTER - 2) + ' '
    room = width - GUTTER
    gap = room - len(title) - len(right) - 1
    if gap < 1:
        right, gap = '', room - len(title) - 1
    voice = ink(SECTION[name]) if name == 'dreams' else ink('quiet')  # one accent on the page: What if…
    return lead + voice + BOLD + fit(title, room - 1) + OFF + ' ' * max(1, gap) + ink('quiet') + right + OFF


LANE = 9  # the lanes' labels, then their picture


def lanes_legend(s, width, dark=False):
    hue = lambda name: '\x1b[38;2;%d;%d;%dm' % (ON_DARK if dark else INK)[name][:3]
    parts = [(hue('learn') + '▮', f"{short(len(s['learned']['chunks']))} learned"),
             (hue('recall') + '↑', f"{handoffs(s['gave']['recalls']) + len(s['gave']['carried'])} handed to Claude"),
             (hue('asked') + '●', f"{len(s['asked'])} asked")]
    while sum(2 + len(text) + 2 for _, text in parts) - 2 > width and len(parts) > 1:
        parts.pop()
    words = hue('text') if dark else ink('quiet')
    return '  '.join(f"{mark}{OFF} {words}{text}{OFF}" for mark, text in parts)


def recall_act(item, now):
    """What a click on a remembered past session hands Claude: the gist, and where the rest is."""
    when = from_when(item['when'], now) if item.get('when') else ''
    where = ' · '.join(part for part in (when, item.get('project')) if part)
    if item['key'].startswith('reflection:'):
        send = f"CSR remembered: “{item['text']}” ({where})." if where else f"CSR remembered: “{item['text']}”."
        detail = [f"A note CSR stored{' ' + when if when else ''}; handed to Claude {item['times']}× this session."]
    else:
        send = (f"CSR remembered a past session: “{item['text']}” ({where}). "
                f"The rest of it: csr_reflect_on_past(\"conv_{item['key']}\")")
        detail = [f"Session {item['key'][:8]} · {where} · handed to Claude {item['times']}× this session."]
    return dict(key='recall:' + item['key'], send=one_line(send), detail=detail)












_WHO = {}  # (transcript, byte) -> whether the words there are yours


NOT_TYPED = re.compile(r'<(pasted_content|system-reminder|command-[\w-]+|local-command-[\w-]+)\b[^>]*>.*?</\1>', re.S)
NOT_TYPED_TAIL = re.compile(r'<(?:pasted_content|system-reminder)\b[^>]*>.*', re.S)  # unclosed: to the end


def own_words(text):
    """What you typed yourself in a prompt: blocks you pasted (often another agent's words), injected
    tags and quoted lines taken out."""
    text = NOT_TYPED_TAIL.sub(' ', NOT_TYPED.sub(' ', text))
    return '\n'.join(line for line in text.splitlines() if not line.lstrip().startswith('>'))


def typed_by_you(transcript, at, quote=None):
    """Whether the words at this byte of a transcript are a prompt you typed: a person's message in the
    main session, not Claude briefing a subagent, not a tool's output, not a headless run; and, given the
    quote, whether it stands in your own words there, not in a block you pasted."""
    if (transcript, at, quote) in _WHO:
        return _WHO[transcript, at, quote]
    yours = False
    if '/subagents/' not in transcript:
        try:
            with open(transcript, 'rb') as handle:
                start = at
                while start > 0:  # back to the start of the line the words are on
                    step = min(start, 1 << 16)
                    handle.seek(start - step)
                    cut = handle.read(step).rfind(b'\n')
                    if cut != -1:
                        start = start - step + cut + 1
                        break
                    start -= step
                handle.seek(start)
                entry = json.loads(handle.readline())
            content = (entry.get('message') or {}).get('content')
            text = content if isinstance(content, str) else ' '.join(
                p.get('text', '') for p in content or [] if isinstance(p, dict) and p.get('type') == 'text')
            yours = (entry.get('type') == 'user' and not entry.get('isSidechain') and not entry.get('isMeta')
                     and entry.get('entrypoint', 'cli') == 'cli' and entry.get('userType', 'external') == 'external'
                     and bool(text.strip()) and not text.lstrip().startswith('<'))
            if yours and quote:
                squash = lambda words: ' '.join(words.split())
                yours = squash(quote)[:80] in squash(own_words(text))
        except (OSError, ValueError, AttributeError):
            yours = False
    _WHO[transcript, at, quote] = yours
    return yours


def short_path(rel, parts=3):
    return '/'.join(rel.split('/')[-parts:])


# ---------------------------------------------------------------- coaching: how Claude read you

QUOTE = r'[“"]([^”"]{1,120})[”"]'
TYPO = re.compile(QUOTE + r'\s+(?:is|was)\s+most\s+likely\s+a\s+typo\s+for\s+' + QUOTE, re.I)
UNCLEAR = re.compile(QUOTE + r'\s*\(unclear\)', re.I)
SAID_AS = re.compile(QUOTE + r'\s*[—–-]*\s*(?:interpreted|read)\s+as:?\s*(.+)', re.I)
MARKED = re.compile(r'Interpreted as:|Read as:|\(unclear\)|most likely a typo for', re.I)  # a compaction summary's
FIRST_WORDS = re.compile(r'checking what [“"][^”"]+[”"] maps to|I[\'’]ll interpret [“"][^”"]+[”"] as|'
                         r'reading (?:this|it) as|most plausibly refers to|typo for', re.I)  # Claude's first sentence
INTERPRET = re.compile(r'I[\'’]ll interpret ' + QUOTE + r' as\s+(.+)', re.I)
_READINGS = {}  # transcript -> where the last read stopped, what it found, whether a prompt awaits Claude's words


def reading_in(line, marks):
    """How Claude read you in one line, or None: the words, what it read them as, the line trimmed.
    Parsed when the line says it in a shape the pane knows; otherwise the whole line, for a click to open."""
    line = ' '.join(line.split()).lstrip('-*• ').strip()
    if not marks.search(line):
        return None
    for pattern, shape in ((TYPO, lambda m: (m[1], m[2])), (INTERPRET, lambda m: (m[1], m[2].split('. ')[0].rstrip('.'))),
                           (SAID_AS, lambda m: (m[1], m[2].split('. ')[0].rstrip('.'))),
                           (UNCLEAR, lambda m: (m[1], ''))):
        found = pattern.search(line)
        if found:
            words, read = shape(found)
            return dict(words=words.strip(), read=read.strip(), line=line)
    return dict(words='', read='', line=line)


def how_claude_read(session_id, most=2):
    """This session's newest readings, newest first: lines of its compaction summaries that say how Claude
    read your words (Interpreted as: · Read as: · "(unclear)" · most likely a typo for), and the first
    sentence of a reply that says so. Read on from where the last look stopped; no model calls."""
    found = glob.glob(os.path.join(HOME, '.claude', 'projects', '*', f'{session_id}.jsonl')) \
        if session_id and re.fullmatch(r'[\w-]+', session_id) else []
    if not found:
        return []
    state = _READINGS.setdefault(found[0], dict(offset=0, found=[], waiting=False))
    try:
        if os.path.getsize(found[0]) < state['offset']:  # written anew: read it again from the top
            state.update(offset=0, found=[], waiting=False)
        with open(found[0], 'rb') as handle:
            handle.seek(state['offset'])
            block = handle.read()
    except OSError:
        return []
    end = block.rfind(b'\n') + 1
    state['offset'] += end
    for raw in block[:end].splitlines():
        summary = b'"isCompactSummary":true' in raw
        typed = b'"type":"user"' in raw and b'"tool_use_id"' not in raw
        if not (summary or typed or (state['waiting'] and b'"type":"assistant"' in raw)):
            continue
        try:
            entry = json.loads(raw)
        except ValueError:
            continue
        message = entry.get('message') if isinstance(entry, dict) else None
        if not isinstance(message, dict):  # a line of some other shape: not a turn
            continue
        when = epoch(entry.get('timestamp')) or 0
        content = message.get('content')
        texts = [content] if isinstance(content, str) else [
            p['text'] for p in content if isinstance(p, dict) and p.get('type') == 'text' and isinstance(p.get('text'), str)
        ] if isinstance(content, list) else []
        if entry.get('type') == 'user' and entry.get('isCompactSummary'):
            for line in '\n'.join(texts).splitlines():
                read = reading_in(line, MARKED)
                if read and not any(x['line'] == read['line'] for x in state['found']):
                    state['found'].append(dict(read, when=when, source='compaction'))
        elif entry.get('type') == 'user' and not entry.get('isMeta') and not entry.get('isSidechain'):
            state['waiting'] = any(t.strip() and not t.lstrip().startswith(('<', '/')) for t in texts)
        elif entry.get('type') == 'assistant' and not entry.get('isSidechain') and any(t.strip() for t in texts):
            state['waiting'] = False
            first = re.split(r'(?<=[.!?])\s|\n', next(t for t in texts if t.strip()).strip(), maxsplit=1)[0]
            read = reading_in(first, FIRST_WORDS)
            if read:
                state['found'].append(dict(read, when=when, source='reply'))
    del state['found'][:-20]
    return state['found'][::-1][:most]


def reading_items(readings, now):
    """The "How Claude read you" rows: “dknr” → read as “done” where it parsed, else the marked line
    trimmed. A click opens the whole line; a second puts the check in Claude's prompt."""
    items = []
    for r in readings:
        words, read = r['words'], r['read']
        if words and read:
            title, send = f'“{words}” → read as “{read}”', f'Earlier you read my “{words}” as “{read}”.'
        elif words:
            title, send = f'“{words}” → read as unclear', f'Earlier you read my “{words}” as unclear.'
        else:
            title, send = r["line"], f'Earlier you read me this way: “{r["line"].rstrip(".")}”.'
        where = 'in a compaction summary' if r.get('source') == 'compaction' else 'in a reply'
        items.append(dict(key=f"read:{r['when']}:{(words or r['line'])[:24]}", title=title, lines=2,
                          meta=' · '.join(x for x in (short_date(r['when']) if r['when'] else '', where) if x),
                          detail=[r['line']], send=one_line(f'{send} Check that reading before relying on it.')))
    return items


# ---------------------------------------------------------------- coaching: terms you say again

CLAUDE_MD = os.path.join(HOME, '.claude', 'CLAUDE.md')
COACH_KEPT = os.path.join(KEEP, 'coach.json')  # transcript -> (mtime, terms Claude looked up there)
LOOKED_IN = ('/.claude/', 'claude.md', 'agent_docs', '/agents', '/skills', 'memory')  # where Claude looks a word up
LOOKUP_WINDOW = 6  # tool calls after a prompt in which a lookup counts as about it
COACH_SESSIONS = 60
COACH_BUDGET = 12  # seconds a run may spend reading transcripts; the next run goes on from the kept ones
PLAIN = set('''the and for that this with from have what when your you are was but not can will all any how why who
    its it's our out has had does did get got let just into over then than them they there their here more most
    some such only also very been being would could should about after before again once make made want need like
    use used using see look now new one two add fix run file files code claude memory agent agents skill skills
    session sessions build test tests work works thing things please yes yeah okay done message messages name text'''.split())
TERM = re.compile(r"[a-z][a-z0-9_-]{2,}")


def mentions(text, term):
    return re.search(r'(?<![\w-])' + re.escape(term) + r'(?![\w-])', text, re.I) is not None


def quoted_in(asked, term, pattern=None):
    """Whether a lookup names the word on its own, in quotes (grep -ril "astra") or as the whole search
    pattern, not as a piece of some longer word or path."""
    if isinstance(pattern, str) and pattern.strip().lower() in (term, f'\\b{term}\\b'):
        return True
    return re.search(r'["\'`](?:-\w+\s+)?' + re.escape(term) + r'["\'`]', asked, re.I) is not None


def terms_looked_up(path):
    """The words of your prompts that Claude then looked up in your memory files, CLAUDE.md, agent docs,
    agents or skills within its next few tool calls, in one transcript: {word: the sentence you said it in}."""
    out, prompt, calls = {}, {}, LOOKUP_WINDOW
    with open(path, 'rb') as handle:
        for raw in handle:
            if b'"type":"user"' in raw and b'"tool_use_id"' not in raw and b'"isMeta":true' not in raw \
                    and b'"isSidechain":true' not in raw:
                try:
                    entry = json.loads(raw)
                except ValueError:
                    continue
                message = entry.get('message') if isinstance(entry, dict) else None
                if not isinstance(message, dict):  # a line of some other shape: not a prompt
                    continue
                content = message.get('content')
                text = own_words(content if isinstance(content, str) else ' '.join(
                    p['text'] for p in content if isinstance(p, dict) and p.get('type') == 'text'
                    and isinstance(p.get('text'), str)) if isinstance(content, list) else '')
                if entry.get('isCompactSummary') or not text.strip() or text.lstrip().startswith(('<', '/')):
                    continue
                prompt = {t: text for t in TERM.findall(text.lower()) if t not in PLAIN}
                calls = 0
            elif calls < LOOKUP_WINDOW and prompt and b'"type":"tool_use"' in raw and b'"isSidechain":true' not in raw:
                seen = raw.count(b'"type":"tool_use"')
                calls += seen
                if not any(mark.encode() in raw.lower() for mark in LOOKED_IN):
                    continue
                try:
                    entry = json.loads(raw)
                except ValueError:
                    continue
                message = entry.get('message') if isinstance(entry, dict) else None
                parts = message.get('content') if isinstance(message, dict) else None
                for part in parts if isinstance(parts, list) else []:
                    if not isinstance(part, dict) or part.get('type') != 'tool_use':
                        continue
                    args = part.get('input') if isinstance(part.get('input'), dict) else {}
                    asked = ' '.join(str(args.get(k) or '') for k in ('pattern', 'command', 'query', 'file_path', 'path'))
                    if not any(mark in asked.lower() for mark in LOOKED_IN):
                        continue
                    for term, said in prompt.items():
                        if term not in out and quoted_in(asked, term, args.get('pattern')):
                            sentence = next((s for s in re.split(r'(?<=[.!?])\s+|\n', said) if mentions(s, term)), said)
                            out[term] = ' '.join(sentence.split())[:90]
    return out


def coach_terms(now=None, newest=COACH_SESSIONS):
    """Words you have said in 2 or more of your newest sessions that Claude looked up in your memory each
    time, and that your global CLAUDE.md does not define yet, as notes for the band. Worked out off the hot
    path: a transcript is read once, kept by its modified time."""
    now = now or time.time()
    try:
        with open(COACH_KEPT) as handle:
            kept = json.load(handle)
    except (OSError, ValueError):
        kept = {}
    kept = {p: v for p, v in kept.items() if isinstance(v, list) and len(v) == 2 and isinstance(v[1], dict)} \
        if isinstance(kept, dict) else {}  # a kept file of another shape is read again, not trusted
    paths = sorted(glob.glob(os.path.join(HOME, '.claude', 'projects', '*', '*.jsonl')),
                   key=lambda p: -(mtime(p) or 0))[:newest]
    started, sessions = time.time(), {}
    for path in paths:
        stamp_ = mtime(path)
        if path not in kept or kept[path][0] != stamp_:
            if time.time() - started > COACH_BUDGET:
                continue
            try:
                kept[path] = [stamp_, terms_looked_up(path)]
            except OSError:
                continue
        for term, said in kept[path][1].items():
            sessions.setdefault(term, []).append(said)
    try:
        os.makedirs(KEEP, exist_ok=True)
        with open(COACH_KEPT, 'w') as handle:
            json.dump({p: v for p, v in kept.items() if p in paths}, handle)
    except OSError:
        pass
    try:
        with open(CLAUDE_MD, encoding='utf-8') as handle:
            defined = handle.read()
    except OSError:
        defined = ''
    notes = []
    for term, said in sorted(sessions.items(), key=lambda kv: -len(kv[1])):
        if len(said) < 2 or mentions(defined, term):
            continue
        notes.append(dict(
            id=f'term:{term}', kind='term', file='', symbol=None, oid=None, conversation='', sketch=[], at=now * 1000,
            line=f'You said “{term}” in {len(said)} sessions and Claude looked it up each time. Define it once?',
            title=f'Define “{term}” once',
            text=f'- {term}: said in {len(said)} sessions, each time Claude looked it up (e.g. “{said[0]}”). '
                 f'Edit this line to say what you mean.'))
    return notes[:3]


def page_main(data, s, now, ui=None):
    """What a senior architect wants beside the agent, the star first, each shown only with something in it:
      Story             where the session stands, as csr-engine last told it (each compaction retells it)
      What if…          the dreamer's question, its reasoning shown, answered from here; idle, one folded row
      Lost and found    threads a compaction summary carried once and then dropped, each a click from Claude
      Handed to Claude  what CSR's memory gave Claude this session: past sessions and notes
      How Claude read you  where Claude read a short word of yours one way, so you can check it
      Your rules        corrections you typed yourself in this repo
    Above them, the session's gauges (and the compaction strip while one runs): your prompts, Claude's context and CSR's memory at work. Code that
    changed since your past sessions stays one key away (l)."""
    root = data['repo']
    con = connect()
    try:
        projects = {p for p in (s.get('project'), os.path.basename(root)) if p}
        marks = ','.join('?' * len(projects))
        rules = []  # only your own words: the detector also hears Claude, to itself and to its subagents
        for when, quote, session, transcript, at in con.execute(
                f"select ts, quote, session_id, transcript_path, byte_start from intent_events where project in "
                f"({marks}) and kind = 'correction' order by ts desc limit 200", tuple(projects)).fetchall():
            if len(rules) >= 3:
                break
            if not typed_by_you(transcript, at, quote):
                continue
            day = short_date(epoch(when)) if epoch(when) else (when or '')[:10]
            here = ' · this session' if session == s['id'] else ''
            rules.append(dict(key=f'rule:{when}:{quote[:30]}', title=quote, meta=f'you said · {day}{here}',
                              detail=[f'Your words, session {session[:8]}. CSR hands them to Claude when the work comes near.'],
                              send=one_line(f"A standing rule from {day}: “{quote}”")))
    finally:
        con.close()

    ui = ui or {}
    fed = feed(data['dreams'], data.get('chapter'), s.get('id'))
    dreams = []
    starred = fed['star'] is not None
    look = data.get('look')

    def dream_send(d, reasons):
        return one_line(f"{DREAMER_SAID}: “{d['line']}” Its reasoning: {d.get('title') or ''}. {' '.join(reasons[:3])}")

    def reasons_of(d):
        return [re.sub(r'^[*-]\s*', '', line) for line in (d.get('text') or '').splitlines() if line.strip()]

    if starred:  # the star: its question whole, its reasoning shown, its answers a click away
        d = fed['star']
        reasons = reasons_of(d)
        dreams.append(dict(key='dream:' + d['id'], dream=d['id'], title=d['line'], lines=3,
                           body=[d.get('title') or ''] + reasons[:1], answers=True, detail=reasons[1:3],
                           send=dream_send(d, reasons)))
    # Every other dream, adopted or passed, folded into one row; when the dreamer last looked sits in the
    # heading. One click opens the list, each dream still a click from Claude.
    earlier = sorted(fed['adopted'] + fed['folded'], key=lambda d: d['at'], reverse=True)
    if earlier:
        is_open = 'passed' in (ui.get('folds') or ())
        took = len(earlier) - len(fed['passed'])
        dreams.append(dict(key='fold:passed', quiet=True, lines=1,
                           title=f"{'⌄' if is_open else '›'} " + ' · '.join(
                               x for x in (f"{took} adopted" if took else '',
                                           f"{len(fed['passed'])} passed" if fed['passed'] else '') if x),
                           detail=[], send=''))
        if is_open:
            for d in earlier:
                reasons = reasons_of(d)
                dreams.append(dict(key='dream:' + d['id'], title=d['line'], lines=1,
                                   meta=' · '.join(x for x in (stamp(d['at'], now)[0], ANSWER_WORD.get(d['answer'], 'not answered')) if x),
                                   detail=[d.get('title') or ''] + reasons[:2], send=dream_send(d, reasons)))

    # Only what Claude used: a hand-off it then opened, at most two rows. The rest, and the questions
    # Claude asked CSR itself, stay behind one row with the counts, which opens to the list.
    handed, passive, queries = [], [], []
    for item in s.get('remembered') or []:
        if item['kind'] == 'recall':
            where = ' · '.join(x for x in (from_when(item.get('when'), now), item.get('project'),
                                           f"{item['times']}×" if item['times'] > 1 else '') if x)
            gist = re.sub(r'\*\*|(?:^|\s)#+\s*', ' ', item['text']).strip()  # a note's markdown, as words
            if item.get('opened') and len(handed) < 2:
                handed.append(dict(recall_act(item, now), title=gist, meta='opened · ' + where))
            else:
                passive.append(f"{gist} · {where}" if where else gist)
        else:
            queries.append(item['text'])
    given = handoffs(s['gave']['recalls']) + len(s['gave']['carried']) if s.get('gave') else 0
    opened = sum(1 for item in s.get('remembered') or [] if item.get('opened'))
    if given or queries:
        handed.append(dict(key='handed:all', quiet=True, lines=1,
                           title=' · '.join(x for x in (f"{given} handed" if given else '',
                                                        f"{opened} opened" if opened else '',  # a zero would measure the format
                                                        f"Claude asked {len(queries)}×" if queries else '') if x),
                           detail=(passive[:4] + [f'Claude asked: {q}' for q in queries[:2]]) or ['Nothing left to list.'],
                           send=one_line('CSR handed you these this session: ' + '; '.join(passive[:5]))))

    clock = Clock(s['prompts'] + [r[0] for r in s['replies']], now)
    facts = [f"{len(s['prompts'])} prompts", span(clock.total)]
    facts += [phrase for phrase in [payback_phrase(s)] if phrase]
    if s['compacts']:
        facts.append(f"{len(s['compacts'])} compacts")
    story, told = data.get('story'), []
    if story:
        told.append(dict(key='story', title=story['now'], open_title=story['text'], lines=6, open_lines=30, detail=[],
                         send=one_line(f"CSR's story of this session, rev {story['rev']}: {story['text']}")))
    if not data.get('dreams_on'):
        right = 'off'
    elif not fed['left']:
        right = '0 left · next compact'
    elif look and not starred:
        when = stamp(look['at'], now)[0]
        right = f"looked {'just now' if when == 'now' else when} · {fed['left']} left"
    else:
        right = f"{fed['left']} of {fed['of']} left"
    sections = [
        dict(name='story', title='Story', items=told, empty='',
             right=' · '.join(x for x in (story and f"rev {story['rev']}", story and story['by'],
                                          story and story['at'] and stamp(story['at'], now)[0]) if x)),
        dict(name='dreams', title='What if…', right=right, items=dreams, empty=''),
        dict(name='remembered', title='Handed to Claude', right='', items=handed, empty=''),
        dict(name='coach', title='How Claude read you', right='', items=reading_items(data.get('readings') or [], now),
             empty=''),
        dict(name='rules', title='Your rules', right='', items=rules, empty=''),
    ]
    sections[2:2] = lost_sections(s, now)
    if starred:  # a question waiting on you comes before the story
        sections[:2] = sections[1::-1]
    return dict(head=dict(facts=' · '.join(facts), session=s if s.get('learned') else None,
                          status=data.get('claude_line'), mast=masthead(data.get('claude_fields'), now)),
                quiet='Nothing to report yet. A section shows here once CSR has something for it.',
                sections=sections)


def page_log(data):
    """The l view: every commit that changed code your past sessions worked on."""
    commits = {}
    for shift in data['shifts']:
        commits.setdefault((shift['date'], shift['oid']), []).append(shift)
    items = []
    for (date, oid), group in commits.items():
        files = {}
        for shift in group:
            files.setdefault(shift['file'], []).append(shift)
        named = ', '.join(f"{short_path(file, 2)} ({', '.join(c['symbol'] or 'whole file' for c in changes[:4])})"
                          for file, changes in list(files.items())[:3])
        items.append(dict(key=f'commit:{oid}', title=group[0]['subject'] or oid[:7],
                          meta=f"{oid[:7]} · {date[5:]} · {len(files)} {'file' if len(files) == 1 else 'files'}",
                          detail=[f"{short_path(file, 2)}: {' '.join((MARK_WORD.get(c['verb'], '') + (c['symbol'] or 'whole file')) for c in changes[:6])}"
                                  for file, changes in list(files.items())[:4]],
                          send=one_line(f"Since our past sessions, commit {oid[:7]} “{group[0]['subject']}” ({date}) "
                                        f"changed code we worked on: {named}.")))
    return dict(head=dict(kicker='CSR · changed since your sessions', title='Commits under your past work',
                          facts='When Claude opens one of these files, CSR gives it a Heads up.'),
                quiet='Nothing changed under past conversations here.',
                sections=[dict(name='changed', title='Changed since', right=str(data['total']), items=items,
                               empty='Nothing changed under past conversations here.')])


MARK_WORD = {'changed': '~', 'removed': '−', 'came back': '+'}

ITEM = GUTTER  # an item's text starts under its section's title


def item_rows(item, name, width, is_open):
    """One item: its title on up to two lines (all of it once opened), its body where it has one (the star
    dream's reasoning, always shown), then what CSR knows about it, quiet, with a flag first in the risk ink."""
    room = width - ITEM
    title = item.get('open_title', item['title']) if is_open else item['title']
    title = wrap(title.replace('`', ''), room, item.get('open_lines', 6) if is_open else item.get('lines', 2))
    voice = ink('quiet') if item.get('quiet') else ink('you')  # lamp black: the page's own type
    rows = [' ' * ITEM + voice + line + OFF for line in title]
    for k, text in enumerate(item.get('body') or ()):
        voice = ink('quiet') + (BOLD if k == 0 else '')
        rows += [' ' * ITEM + voice + line + OFF for line in wrap(text, room, 1 if k == 0 else 2)]
    flag, meta = item.get('flag') or '', item.get('meta') or ''
    text = fit(' · '.join(x for x in (flag, meta) if x), room)
    line = ink('faint') + text
    if flag and text.startswith(flag):
        line = ink('turn') + flag + ink('faint') + text[len(flag):]
    return rows + [' ' * ITEM + line + OFF] if text else rows


ANSWERS = (('helpful', '✓ helpful'), ('thought_of_it', '= thought of it'),
           ('not_relevant', '✗ not relevant'), ('ask_claude', '→ ask Claude'))


def answer_rows(item, width, ui):
    """The star dream's answers, two to a row; a click on either half answers it. Each row is its own
    block, so a click knows the dream and, by its column, the answer."""
    if not item.get('answers'):
        return []
    half = (width - ITEM) // 2
    rows = []
    for pair in (ANSWERS[:2], ANSWERS[2:]):
        text, spans = ' ' * ITEM, []
        for k, (answer, label) in enumerate(pair):
            start = ITEM + k * half
            spans.append((start, start + half, answer))
            text += ink(SECTION['dreams']) + BOLD + label[0] + OFF + ink('quiet') + fit(label[1:], half - 2).ljust(half - 1) + OFF
        rows.append(([text], dict(key=item['key'] + ':answers', dream=item['dream'], send=item['send'], spans=spans)))
    answered = ui.get('answered')
    if answered and answered[0] == item['dream']:
        said = ANSWER_WORD.get(answered[1], answered[1])
        rows = [([' ' * ITEM + ink('faint') + fit(f'✓ kept: {said}', width - ITEM) + OFF], None)]
    return rows


def page_lines(page, width, rows, ui):
    """The page in the terminal's type on paper: its facts and the session's gauges, then each section
    with something to report, its rows in its own tint (every other one a shade deeper), a strip of paper
    between. Every section keeps its heading and first row; the room left goes to the earlier ones first."""
    head, s = page['head'], page['head'].get('session')
    label = '\x1b[38;2;%d;%d;%dm' % ON_DARK['label']
    top = [' ' + line for line in ansi_wrap(head.get('status') or '', width - 1, 2)]  # Claude's status line
    top[:0] = compact_rows(head.get('compacting'), width)  # while a compaction runs
    if head.get('title'):
        top.append(' ' + BOLD + fit(head['title'], width - 1) + OFF)
    top.append(' ' + fit_pieces(head['facts'], width - 1))
    top += mast_rows(head.get('mast'), width - 1)  # the cache clock and the plan's quotas
    lanes = None
    if s:
        if PIXELS and width - LANE > 8:
            lanes = len(top)
            top += [' ' + label + name.ljust(LANE - 1) + OFF for name in ('you', 'context', 'memory')]
        top.append('  ' + lanes_legend(s, width - 2, dark=True))
    grounds = dark_steps(len(top))  # one gradient, top row to bottom
    out = Lines([on_dark(line, ground) for line, ground in zip(top, grounds)])
    out.heads = []
    out.lanes = None if lanes is None else (lanes, grounds[lanes:lanes + 3])  # the gauges' row, their grounds
    out.put(masthead_lip(rows))
    built = []
    for section in page['sections']:
        blocks = []
        for item in section['items']:
            is_open = ui.get('open') == item['key']
            rows_ = item_rows(item, section['name'], width, is_open)
            if is_open:
                rows_ = opened(rows_, section['name'], item.get('detail') or [], width, ui, item['key'], indent=ITEM)
            blocks.append((rows_, item))
            blocks += answer_rows(item, width, ui) + lost_rows(item, width, ui, is_open)
        if blocks:
            built.append((section, blocks, 2 + len(blocks[0][0]), 2 + sum(len(rows_) for rows_, _ in blocks)))
    if not built:
        return out.put([''] + [' ' + ink('faint') + l + OFF for l in wrap(page.get('quiet') or '', width - 2, 3)])
    left = rows - 1 - len(out) - sum(base for *_, base, _ in built)
    for section, blocks, base, want in built:
        room = base + max(0, min(want - base, left))
        left -= room - base
        name = section['name']
        part = Lines([heading(name, section['title'], section['right'], width)])
        fit_blocks(part, blocks, room - 1, ui, tail=False)
        shade = []
        for i, line in enumerate(part):  # the heading a shade deeper than its items, which share one tint
            back = tint(name, HEAD if i == 0 else ROW)
            opened_row = line.startswith(band(name))
            shade.append(tint(name) if opened_row else back)
            part[i] = line if opened_row else tinted(line, '\x1b[48;2;%d;%d;%dm' % back)
        out.put([''])
        out.heads.append((len(out), name, (shade[0], shade[1] if len(shade) > 1 else GROUND)))
        start = len(out)
        out.extend(part)
        out.acts.update({start + i: act for i, act in part.acts.items()})
    return out


def draw_page(page, cols, rows, ui):
    """The page on paper in the terminal's own type, so every row takes a click; pictures only for the
    gauges and the section icons, which no click needs."""
    width = max(24, cols - 1)
    lines = page_lines(page, width, rows, ui)
    pictures = []
    if PIXELS:
        cell = cell_size()
        s = page['head'].get('session')
        if s and lines.lanes is not None:
            row, grounds = lines.lanes
            pictures.append((row, LANE, width - LANE, 3, session_png(s, width - LANE, cell, time.time(), grounds)))
        pictures += [(row, 0, ICON[0], ICON[1], iso_png(name, cell, backs))
                     for row, name, backs in lines.heads if row + ICON[1] <= rows - 1]
    return lines, pictures


def draw_main(data, s, cols, rows, now, ui=None):
    if s is None:
        s = dict(id=None, title=None, prompts=[], replies=[], compacts=[], gave=dict(recalls=[], carried=[], dreams=[]),
                 remembered=[], sessions=0, project=None, files={}, edited=[], shell={}, last={})
    return draw_page(with_compacting(page_main(data, s, now, ui), data), cols, rows, ui or {})


def draw_log(data, cols, rows, ui=None):
    return draw_page(page_log(data), cols, rows, ui or {})


# ---------------------------------------------------------------- compaction: the timer, what CSR keeps, the summary

COMPACTING = os.path.join(KEEP, 'compacting')  # <session>.json: {at, trigger}, written by the mod while a compaction runs
COMPACT_TIMES = os.path.join(KEEP, 'compaction-times.json')  # transcript -> [mtime, [seconds each compaction took]]
PROJECTS = os.path.join(HOME, '.claude', 'projects')
MARKER_LIFE = 10 * 60  # a marker older than this is from a compaction that never reported its end
NEWEST = 60  # transcripts read for how long a compaction usually takes
FRACTIONS = ('', '▏', '▎', '▍', '▌', '▋', '▊', '▉')  # eighths of a cell, so the bar moves smoothly
READ_AS = re.compile(r'interpreted as:|read as:|\(unclear\)|typo for', re.I)  # where a summary says how it read you
_SUMMARIES = {}  # transcript -> dict(offset, found, count): read from where it was left
_USUAL = {}  # (projects, cache) -> (when read, the usual range): the transcripts are listed once in five minutes


def compacting_since(session_id, now, folder=None):
    """The compaction running now: dict(at, trigger) from the mod's marker, or None when none runs. A marker
    older than ten minutes is left over from one that never reported its end."""
    try:
        with open(kept_file(folder or COMPACTING, session_id) or '', encoding='utf-8') as handle:
            kept = json.load(handle)
        at = float(kept.get('at') or 0)
    except (OSError, ValueError, TypeError, AttributeError):
        return None
    at = at / 1000 if at > 1e11 else at  # seconds; a clock in milliseconds is read as such
    if not at or now - at > MARKER_LIFE:
        return None
    return dict(at=at, trigger=str(kept.get('trigger') or ''))


def percentile(values, share):
    """The share (0 to 1) of sorted values, between the two nearest ones."""
    at = (len(values) - 1) * share
    low = int(at)
    return values[low] + (values[min(low + 1, len(values) - 1)] - values[low]) * (at - low)


def compaction_seconds(projects=None, cache=None, newest=NEWEST):
    """Seconds each compaction took, from the compact_boundary rows (compactMetadata.durationMs) of the
    newest transcripts, sorted. A transcript is read again only when its mtime moved; the rest come from the cache."""
    projects, cache = projects or PROJECTS, cache or COMPACT_TIMES
    try:
        with open(cache, encoding='utf-8') as handle:
            kept = json.load(handle)
    except (OSError, ValueError):
        kept = {}
    found = []
    for path in glob.glob(os.path.join(projects, '*', '*.jsonl')):
        modified = mtime(path)
        if modified is not None:
            found.append((modified, path))
    current, seconds = {}, []
    for modified, path in sorted(found, reverse=True)[:newest]:
        entry = kept.get(path)
        if not (isinstance(entry, list) and len(entry) == 2 and entry[0] == modified):
            took = []
            try:
                with open(path, 'rb') as handle:
                    for line in handle:
                        if b'"compact_boundary"' not in line:
                            continue
                        try:
                            row = json.loads(line)
                        except ValueError:
                            continue
                        ms = ((row.get('compactMetadata') or {}).get('durationMs')) if row.get('subtype') == 'compact_boundary' else None
                        if isinstance(ms, (int, float)) and ms > 0:
                            took.append(ms / 1000)
            except OSError:
                continue
            entry = [modified, took]
        current[path] = entry
        seconds += entry[1]
    if current != kept:
        try:
            os.makedirs(os.path.dirname(cache), exist_ok=True)
            with open(cache + '.tmp', 'w', encoding='utf-8') as handle:
                json.dump(current, handle)
            os.replace(cache + '.tmp', cache)
        except OSError:
            pass
    return sorted(seconds)


def compaction_usual(seconds):
    """How long a compaction usually takes: dict(low, median, high, n) with the 10th and 90th percentile
    around the median; None with fewer than three to go on."""
    if len(seconds) < 3:
        return None
    return dict(low=percentile(seconds, 0.1), median=percentile(seconds, 0.5), high=percentile(seconds, 0.9), n=len(seconds))


def compaction_facts(con, session_id, story, dreams_on):
    """What CSR keeps from the part being folded, one to three lines, each with a number behind it: no model."""
    lines = []
    chunks = con.execute('select count(*) from chunks where conversation_id=?', (session_id,)).fetchone()[0]
    if chunks:
        lines.append(f"CSR keeps {chunks:,} chunk{'s' if chunks != 1 else ''} of this session")
    if story and story.get('rev'):
        lines.append(f"story rev {story['rev']} → {story['rev'] + 1} after this")
    if dreams_on:
        lines.append(f"a new chapter of dreams after this ({PER_CHAPTER} looks)")
    return lines


def compacting_info(session_id, now, data, folder=None, projects=None, cache=None):
    """What the pane shows while a compaction runs: dict(elapsed, trigger, usual, facts), or None when none runs."""
    marker = compacting_since(session_id, now, folder)
    if not marker:
        return None
    info = dict(elapsed=int(max(0, now - marker['at'])), trigger=marker['trigger'], usual=None, facts=[])
    hit = _USUAL.get((projects, cache))
    if hit and now - hit[0] < 300:
        info['usual'] = hit[1]
    else:
        try:
            info['usual'] = compaction_usual(compaction_seconds(projects, cache))
            _USUAL[(projects, cache)] = (now, info['usual'])
        except Exception:  # a transcript that cannot be read costs the range, never the pane
            pass
    try:
        con = connect()
        try:
            info['facts'] = compaction_facts(con, session_id, data.get('story'), bool(data.get('dreams_on')))
        finally:
            con.close()
    except Exception:
        pass
    return info


def clock_text(seconds):
    seconds = int(seconds)
    return f'{seconds // 60}:{seconds % 60:02d}'


def compaction_bar(elapsed, high, width):
    """A bar of block characters filled by how far elapsed has come toward the usual longest time."""
    cells = max(0.0, min(1.0, elapsed / high)) * width if high else 0.0
    full = int(cells)
    if full >= width:
        return '█' * width
    return ('█' * full + FRACTIONS[int((cells - full) * 8)]).ljust(width, '░')


def compact_rows(info, width):
    """The strip at the top of the page while a compaction runs: the timer against the usual time, a bar, and
    the facts under it. Rows for the dark masthead."""
    if not info:
        return []
    usual, elapsed = info.get('usual'), info['elapsed']
    over = bool(usual) and elapsed > usual['high']
    note = ('taking longer than usual' if over else f"usually {clock_text(usual['low'])} to {clock_text(usual['high'])}") if usual else ''
    kind = ' (auto)' if info.get('trigger') == 'auto' else ''
    words = ' · '.join(x for x in (f'Compacting{kind}', clock_text(elapsed), note) if x)
    if len(words) > width - 1 and note:  # a narrow pane: the range goes on a row of its own
        words = f'Compacting{kind} · {clock_text(elapsed)}'
        rows = [' ' + BOLD + fit(words, width - 1) + OFF, ' ' + fit(note, width - 1)]
    else:
        rows = [' ' + BOLD + fit(words, width - 1) + OFF]
    if usual:
        hue = '\x1b[38;2;%d;%d;%dm' % ON_DARK['turn' if over else 'ctx'][:3]
        rows.append(' ' + hue + compaction_bar(elapsed, usual['high'], max(8, min(40, width - 2))) + OFF)
    return rows + [' ' + DIM + fit(line, width - 1) + OFF for line in (info.get('facts') or [])[:3]]


def with_compacting(page, data):
    """The page with the compaction strip in its head, for the masthead to draw."""
    page['head']['compacting'] = data.get('compacting')
    return page


def compaction_ended(pane, running):
    """True on the frame after a compaction's marker is gone, so the pane redraws at once."""
    was, pane.compacting = getattr(pane, 'compacting', False), bool(running)
    return was and not running


def compaction_gone(pane, folder=None):
    """True between frames once the marker of the compaction the pane is showing is gone: the watch loop
    then draws at once, not on the next two-second tick."""
    if not getattr(pane, 'compacting', False) or not pane.session:
        return False
    return not os.path.exists(kept_file(folder or COMPACTING, pane.session.id) or '')


def row_text(entry):
    content = (entry.get('message') or {}).get('content')
    if isinstance(content, str):
        return content
    return '\n'.join(p.get('text', '') for p in content or [] if isinstance(p, dict) and p.get('type') == 'text')


def newest_summary(path):
    """The newest compaction summary in a session's transcript (the isCompactSummary row): dict(text, at,
    count, top). Read from where it was left; the same dict comes back until a newer one lands, so the
    page keeps its scroll."""
    if not path:
        return None
    state = _SUMMARIES.setdefault(path, dict(offset=0, found=None, count=0))
    try:
        if os.path.getsize(path) < state['offset']:  # rewritten: start again
            state.update(offset=0, found=None, count=0)
        with open(path, 'rb') as handle:
            handle.seek(state['offset'])
            for line in handle:
                if not line.endswith(b'\n'):
                    break
                state['offset'] += len(line)
                if b'isCompactSummary' not in line:
                    continue
                try:
                    entry = json.loads(line)
                except ValueError:
                    continue
                if entry.get('isCompactSummary') and row_text(entry).strip():
                    state['count'] += 1
                    state['found'] = dict(text=row_text(entry), at=epoch(entry.get('timestamp')), count=state['count'], top=0)
    except OSError:
        pass
    return state['found']


def summary_lines(text, width):
    """The summary wrapped to the width, its blank lines and indents kept: (line, marked) pairs, marked where
    the summary says how it read you (Interpreted as: / Read as: / (unclear) / typo for)."""
    rows = []
    for raw in text.replace('**', '').splitlines():
        if not raw.strip():
            if rows and rows[-1][0]:
                rows.append(('', False))
            continue
        indent = ' ' * min(len(raw) - len(raw.lstrip()), 8)
        marked = bool(READ_AS.search(raw))
        rows += [(indent + piece, marked) for piece in wrap(raw.strip(), max(8, width - len(indent)), 10 ** 6)]
    return rows


def draw_summary(data, cols, rows, ui=None):
    """The s page: this session's newest compaction summary, wrapped, the read-as lines in the accent ink."""
    width = max(24, cols - 1)
    summary = data.get('summary')
    body, facts = [], 'No compaction summary yet.'
    room = max(1, rows - 1 - 2)
    if summary:
        body = summary_lines(summary['text'], width - 1)
        summary['top'] = max(0, min(summary['top'], len(body) - room))
        when = datetime.fromtimestamp(summary['at']).strftime('%m-%d %H:%M') if summary['at'] else ''
        shown = f"lines {summary['top'] + 1}-{min(len(body), summary['top'] + room)} of {len(body)}"
        marks = sum(1 for _, marked in body if marked)
        facts = ' · '.join(x for x in (when, f"summary {summary['count']}" if summary['count'] > 1 else '', shown,
                                       f"{marks} read-as" if marks else '', f"{len(summary['text']):,} chars") if x)
    top = [' ' + BOLD + 'Compaction summary' + OFF, ' ' + fit(facts, width - 1)]
    out = Lines([on_dark(line, ground) for line, ground in zip(top, dark_steps(len(top)))])
    start = summary['top'] if summary else 0
    for line, marked in body[start:start + room]:
        out.append(' ' + (ink('turn') if marked else ink('you')) + line + OFF)
    return out, []


def summary_keys(pane, keys):
    """The summary page's keys: s opens it, s or Esc goes back (d does, in the loop), j and k scroll; True when the
    screen must redraw."""
    if 's' in keys and pane.view != 'summary':
        pane.view, pane.open = 'summary', None
        return True
    if pane.view != 'summary':
        return False
    if keys == '\x1b' or 's' in keys:
        pane.view = 'main'
        return True
    summary = (pane.data or {}).get('summary')
    step = 3 if any(k in keys for k in ('j', '\x1b[b')) else -3 if any(k in keys for k in ('k', '\x1b[a')) else 0
    if ' ' in keys:
        step = 12
    if summary and step:
        summary['top'] = max(0, summary['top'] + step)
        return True
    return False


THREAD_GROUPS = (('waiting', 'Waiting on you', 'waiting'), ('lost', 'Went quiet', 'quiet'),
                 ('carried', 'From the session before', 'before'), ('open', 'Still open', 'open'),
                 ('faded', 'Showed once', 'once'), ('answered', 'Guessed done', 'guessed'),
                 ('later', 'Set aside for later', 'later'), ('drop', 'Dropped', 'dropped'))


def thread_group(t):
    if t['hidden']:
        return t['hidden']
    if t['state'] in ('lost', 'open') and t['waiting']:
        return 'waiting'
    return t['state']


def draw_threads(data, s, cols, rows, ui, top=0):
    """The f page: every thread Lost and found knows, grouped, nothing hidden; each says why it sits where
    it does. A click opens one and shows its answers, as on the main page."""
    width = max(24, cols - 1)
    every = (s or {}).get('lost') or []
    groups = {name: [t for t in every if thread_group(t) == name] for name, *_ in THREAD_GROUPS}
    facts = f"{len(every)} threads: " + ' · '.join(f"{len(groups[name])} {short}" for name, _, short in THREAD_GROUPS
                                                   if groups[name]) if every else 'No compaction summary yet.'
    head = [' ' + BOLD + 'Lost and found · every thread' + OFF] + [' ' + line for line in wrap(facts, width - 1, 3)]
    out = Lines([on_dark(line, ground) for line, ground in zip(head, dark_steps(len(head)))])
    body = []  # (line, act)
    for name, title, _ in THREAD_GROUPS:
        if not groups[name]:
            continue
        body.append(('', None))
        body.append((' ' + ink('turn') + BOLD + title + OFF + ink('faint') + f'  {len(groups[name])}' + OFF, None))
        for t in groups[name]:
            asks = lost_asks(t)
            item = dict(key='lost:' + t['key'], lost=t['key'], top=False, asks=asks, send=asks['side'])
            act = dict(key=item['key'], send=asks['side'])
            for line in wrap(t['text'], width - ITEM, 3):
                body.append((' ' * ITEM + ink('you') + line + OFF, act))
            body.append((' ' * ITEM + ink('faint') + fit(t.get('why') or '', width - ITEM) + OFF, act))
            if ui.get('open') == item['key']:
                for line in t['detail'][:3]:
                    body.append((' ' * ITEM + ink('quiet') + fit(line, width - ITEM) + OFF, act))
                for rows_, answer in lost_rows(item, width, ui, True):
                    body += [(line, answer) for line in rows_]
    room = max(1, rows - 1 - len(out))
    top = max(0, min(top, len(body) - room))
    for line, act in body[top:top + room]:
        out.put([line], act)
    return out, [], top


def threads_keys(pane, keys):
    """The f page's keys: f opens it, f or Esc goes back, j and k scroll; True when the screen must redraw."""
    if 'f' in keys and pane.view != 'threads':
        pane.view, pane.open, pane.threads_top = 'threads', None, 0
        return True
    if pane.view != 'threads':
        return False
    if keys == '\x1b' or 'f' in keys:
        pane.view, pane.open = 'main', None
        return True
    step = 3 if any(k in keys for k in ('j', '\x1b[b')) else -3 if any(k in keys for k in ('k', '\x1b[a')) else 0
    if ' ' in keys:
        step = 12
    if step:
        pane.threads_top = max(0, pane.threads_top + step)
        return True
    return False


TALL = 600  # the rows a scrolled main page is laid out in: every section at its whole length
WHEEL = 3  # rows one wheel notch or one j/k moves


def window(lines, pictures, rows, top):
    """The rows of a page laid out tall that the screen shows from top, their clicks and pictures moved
    with them; a picture the edge would cut is left out."""
    room = rows - 1
    top = max(0, min(top, len(lines) - room))
    shown = Lines(lines[top:top + room])
    shown.acts = {row - top: act for row, act in lines.acts.items() if top <= row < top + room}
    kept = [(row - top, col, w, h, png) for row, col, w, h, png in pictures if 0 <= row - top and row - top + h <= room]
    return shown, kept, top


def compact_key(data):
    """What redraws the pane for a compaction: the timer's second and facts, the summary page's place."""
    summary = data.get('summary')
    return [data.get('compacting'), summary and [summary['count'], summary['top']]]


def legend(view, width, held=None):
    """The bottom row: its keys in lamp black, what each does in pencil; a click on it does the first.
    While an edit to the pane's own file does not compile, the row says so and the old pane runs on."""
    if held:
        return ' ' + ink('turn') + fit(f'edit not loaded: {held}', width - 1) + OFF
    keys = [('l', 'changes since your sessions'), ('s', 'compaction summary'), ('f', 'every lost thread'), ('q', 'close')] \
        if view == 'main' else [('d', 'back')] + ([('j/k', 'scroll')] if view in ('summary', 'threads') else []) + [('q', 'close')]
    marks = '' if view in ('main', 'summary', 'threads') else '~ changed  - removed  + back   '
    words = [(key, said) for key, said in keys]
    while len(marks) + sum(len(k) + len(w) + 4 for k, w in words) > width and len(words[0][1]) > 7:
        words[0] = (words[0][0], words[0][1].rsplit(' ', 1)[0])
    if len(marks) + sum(len(k) + len(w) + 4 for k, w in words) > width:  # still too wide: 's summary'
        words = [words[0]] + [(key, said.rsplit(' ', 1)[-1]) for key, said in words[1:]]
    sep = '   ' if len(marks) + sum(len(k) + len(w) + 4 for k, w in words) <= width else '  '  # still tight: closer
    shown = sep.join(f'{BOLD}{ink("you")}{key}{OFF}{ink("faint")} {said}{OFF}' for key, said in words)
    return ' ' + ink('faint') + marks + OFF + shown


def herdr(*argv):
    """Run herdr's CLI; its answer, or None when it is not there or says no."""
    binary = os.environ.get('HERDR_BIN_PATH') or shutil.which('herdr')
    if not binary:
        return None
    try:
        done = subprocess.run([binary, *argv], capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        return None
    return done.stdout if done.returncode == 0 else None


def agent_pane(given):
    """Claude's pane: the one the mod named, else this pane's neighbour on the left."""
    if given:
        return given
    me = os.environ.get('HERDR_PANE_ID')
    said = me and herdr('pane', 'neighbor', '--pane', me, '--direction', 'left')
    try:
        found = json.loads(said or '{}').get('result', {})
    except ValueError:
        return None
    return (found.get('pane') or {}).get('pane_id') or found.get('pane_id')


def focus_back():
    """Keys stay Claude's: a click lands here, so hand the focus back to the pane on the left."""
    me = os.environ.get('HERDR_PANE_ID')
    if me:
        herdr('pane', 'focus', '--pane', me, '--direction', 'left')


class Pane:
    def __init__(self, root, limit, session_id, agent=None):
        self.root, self.limit, self.agent = root, limit, agent
        self.session = Session(session_id) if session_id else None
        self.view, self.data, self.loaded, self.shown = 'main', None, 0.0, None
        self.open, self.sent, self.acts = None, None, {}  # the row a click opened; what went to Claude
        self.answered = None  # (dream id, answer, when) the pane kept for a dream, until it is read
        self.folds = set()  # folded rows opened with a click
        self.held = None  # why an edit to this file was not loaded, until one is
        self.threads_top = 0  # the f page's first line shown
        self.main_top = 0  # the main page's first line shown, once scrolled or a row is opened
        self.anchor = None  # (row key, screen row) a click just opened: it stays where it was clicked

    def frame(self, force):
        cols, rows = shutil.get_terminal_size((60, 30))
        now = time.time()
        try:
            if self.data is None or now - self.loaded > 60:
                self.data, self.loaded = load(self.root, self.limit, self.session and self.session.id), now
            self.data['dreams'] = load_dream_log(self.root)  # the mod appends while we watch
            self.data['look'] = last_look(self.root)
            self.data['chapter'] = last_chapter(self.root, self.session and self.session.id)
            self.data['claude_line'] = claude_status(self.session and self.session.id, now)
            self.data['story'] = session_story(self.session and self.session.id)
            self.data['claude_fields'] = claude_fields(self.session and self.session.id, now)
            self.data['compacting'] = compacting_info(self.session and self.session.id, now, self.data)
            if self.view == 'summary':
                self.data['summary'] = newest_summary(self.session and self.session.path)
            force = compaction_ended(self, self.data['compacting']) or force  # the marker went: redraw at once
            self.data['readings'] = how_claude_read(self.session and self.session.id)
            s = self.session.load() if self.session and self.view in ('main', 'threads') else None
        except Exception as error:  # the DB is busy or missing: say so, keep the pane alive
            said = SETUP if not os.path.exists(DB) else f'could not read csr-engine: {error}'
            sys.stdout.write(f"\x1b_Ga=d,d=A\x1b\\\x1b[H\x1b[2J{BOLD}CSR{OFF}\n{DIM}"
                             + '\n'.join(wrap(said, cols - 1, 4)) + f"{OFF}\n")
            sys.stdout.flush()
            return
        if self.sent and now - self.sent[2] > 6:  # the "in Claude's prompt" line has been read
            self.open = None if self.open == self.sent[0] else self.open
            self.sent = None
        if self.answered and now - self.answered[2] > 6:
            self.answered = None
        ui = dict(open=self.open, sent=self.sent, answered=self.answered, folds=self.folds)
        sig = json.dumps([self.view, cols, rows, self.data['total'], self.data['dreams_on'], self.open, self.sent,
                          self.answered,
                          self.data.get('claude_line'), masthead(self.data.get('claude_fields'), now),
                          [(d['id'], d['answer']) for d in self.data['dreams']], self.data.get('look'),
                          self.data.get('chapter'), sorted(self.folds), self.data.get('story'), self.held,
                          compact_key(self.data), self.data.get('readings'), self.threads_top, self.main_top,
                          s and [(t['key'], t['hidden'], t['state']) for t in s['lost']],
                          s and [len(s['replies']), len(s['prompts']), len(s['learned']['chunks']), s['title'],
                                 len(s['learned']['turns']), len(s['gave']['recalls']), len(s['remembered'])]])
        if sig == self.shown and not force:
            return
        self.shown = sig
        if self.view == 'main' and (self.main_top or self.open):  # laid out whole, the screen a window on it
            lines, pictures = draw_main(self.data, s, cols, TALL, now, ui)
            if self.anchor:
                row = min((r for r, act in lines.acts.items() if act.get('key') == self.anchor[0]), default=None)
                self.main_top = max(0, row - self.anchor[1]) if row is not None else self.main_top
                self.anchor = None
            lines, pictures, self.main_top = window(lines, pictures, rows, self.main_top)
        elif self.view == 'main':
            lines, pictures = draw_main(self.data, s, cols, rows, now, ui)
        elif self.view == 'summary':
            lines, pictures = draw_summary(self.data, cols, rows, ui)
        elif self.view == 'threads':
            lines, pictures, self.threads_top = draw_threads(self.data, s, cols, rows, ui, self.threads_top)
        else:
            lines, pictures = draw_log(self.data, cols, rows, ui)
        self.acts = {row: act for row, act in getattr(lines, 'acts', {}).items() if row < rows - 1}
        self.acts[rows - 1] = dict(key='view:log' if self.view == 'main' else 'view:main')
        lines = lines[:rows - 1]
        whole = any(h >= rows for _, _, _, h, _ in pictures)  # a paper page draws its own legend
        screen = ['\x1b_Ga=d,d=A\x1b\\', '\x1b[H\x1b[2J', '\n'.join(lines)]
        if not whole:
            screen += [f'\x1b[{rows};1H', legend(self.view, cols - 1, self.held)]
        screen += [kitty(png, r, c, w, h) for r, c, w, h, png in pictures if r + h <= rows]
        screen.append(f'\x1b[{rows};{cols}H')
        sys.stdout.write(''.join(screen))
        sys.stdout.flush()

    def scroll(self, step):
        """The wheel, or j and k: the page in view moves, never past its top."""
        if self.view == 'summary' and (self.data or {}).get('summary'):
            self.data['summary']['top'] = max(0, self.data['summary']['top'] + step)
        elif self.view == 'threads':
            self.threads_top = max(0, self.threads_top + step)
        elif self.view == 'main':
            self.main_top = max(0, self.main_top + step)

    def click(self, row, col=0):
        """One click opens a row; a second click on it puts it in Claude's prompt, unsent. On a dream's
        answers, the half clicked is the answer: kept in the dream log, and "ask Claude" also typed in."""
        act = self.acts.get(row)
        if act and act.get('lost') and act.get('spans'):
            return lost_click(self, act, col)
        span = act and next((answer for start, end, answer in act.get('spans') or () if start <= col < end), None)
        if span:
            keep(dict(kind='answer', id=act['dream'], answer=span, at=time.time(), by='pane'))
            if span == 'ask_claude':
                agent = agent_pane(self.agent)
                went = bool(agent) and herdr('pane', 'send-text', agent, act['send']) is not None
                self.sent = (act['key'], went, time.time())
            self.answered = (act['dream'], span, time.time())
        elif not act or act.get('spans'):
            self.open = None
        elif act['key'].startswith('view:'):
            self.view, self.open = act['key'][5:], None
        elif act['key'].startswith('fold:'):
            self.folds ^= {act['key'][5:]}
        elif act['key'] != self.open:
            self.open, self.sent = act['key'], None
            self.anchor = (act['key'], row) if self.view == 'main' else None
        else:
            agent = agent_pane(self.agent)
            went = bool(agent) and herdr('pane', 'send-text', agent, act['send']) is not None
            self.sent = (act['key'], went, time.time())
        focus_back()


# ---------------------------------------------------------------- lost and found

LOST_LOG = os.path.join(KEEP, 'lost-found.jsonl')  # drop / later, one line each, keyed by the thread's text
SECTION['lost'], GLYPH['lost'] = 'turn', '?'  # its ink (vermilion) and its mark where no pictures are drawn
SAME_THREAD = 0.5  # two items are one thread when this share of their words match
CARRIED = ('Pending Tasks', 'Optional Next Step')  # the summary sections that name what is still owed
NUMBERED = re.compile(r"^(?:#+\s*)?\**\d+\.\s+[A-Z][A-Za-z /&,'-]{2,50}:?\**\s*$")
BULLET = re.compile(r'^(\s*)(?:[-*•]|\d+[.)])\s+(.*)$')
WAITING = re.compile(r"\b(awaiting|waiting (?:on|for) (?:you|the user)|wait for the user|needs? (?:your|their|the user'?s) "
                     r"(?:go|ok|approval|call|decision)|approval|the user'?s (?:answer|go|pick|picks|decision|decisions|call|pasted)|"
                     r"the user needs? to|asks? the user)\b", re.I)
OWED = re.compile(r"\b(wait|waiting|await|awaiting|confirm|verify|check|update|run|fix|build|built|install|merge|push|pushed|publish|ask|"
                  r"decide|need|needs|needing|not (?:yet )?(?:started|built|applied|pushed|done)|pending|todo|next|after|then|"
                  r"before|still|should|must|owed|deferred|proposal|proposals|proposed|open question|without|never)\b", re.I)
BOOKKEEPING = re.compile(r"^(?:(?:send|write|give|post|deliver)\b.{0,40}\b(?:final|brief|short|settle)\b.{0,30}\b(?:reply|report|summary|message)|"
                         r"(?:brief|short|final)\b.{0,20}\b(?:reply|report|summary)|report\b.*\bto the user|"
                         r"(?:tell|let) the user\b|report\b|confirm the background|responding to\b|(?:the )?messages after this summary)", re.I)
GENERIC = {'session', 'claude', 'pane', 'csr', 'mod', 'code', 'make', 'need', 'thing', 'work', 'just', 'like', 'what',
           'should', 'also', 'more', 'some', 'here', 'there', 'them', 'they', 'it', 'can', 'did', 'does', 'how', 'out',
           'see', 'get', 'got', 'let', 'use', 'way', 'go', 'yes', 'ok', 'okay', 'please', 'thank', 'thanks', 'good'}
APPROVED = re.compile(r"^\W*(?:go|yes|yep|ok|okay|install|proceed|approved?|do it|ship it|push it)\b|\bis a go\b", re.I)
REMINDERS = re.compile(r'reminders from csr', re.I)  # what prep-compact asks the summary to keep: never a task
IGNORED = {'the', 'and', 'for', 'with', 'then', 'that', 'this', 'when', 'its', 'are', 'was', 'from', 'into', 'not', 'has',
           'have', 'will', 'any', 'all', 'one', 'new', 'now', 'yet', 'but', 'you', 'your', 'user'}


def plain(text):
    """A summary line without its markdown."""
    return ' '.join(re.sub(r'\*\*|__|`', '', text).split())


def stem(word):
    """A word without its common ending, so "installing" and "install" count as one."""
    for end in ('ing', 'ed', 'es', 's'):
        if len(word) > len(end) + 3 and word.endswith(end):
            return word[:-len(end)]
    return word


def words_of(text):
    return {stem(w) for w in re.findall(r'[a-z0-9_]+', text.lower()) if len(w) > 2 and w not in IGNORED}


def same_words(a, b):
    """One thread in two wordings: most of all their words match, or most of the shorter one's do."""
    if not (a and b):
        return False
    shared = len(a & b)
    return shared / len(a | b) >= SAME_THREAD or (shared >= 3 and shared / min(len(a), len(b)) >= 0.6)


def summary_section(text, name):
    """The lines of one numbered section ("7. Pending Tasks:") of a compaction summary, up to the next."""
    lines, found = [], False
    for line in text.splitlines():
        if found and REMINDERS.search(line):
            break  # prep-compact's reminders: what you set aside, not what is owed
        if NUMBERED.match(line.strip()) and not line.startswith(' '):
            if found:
                break
            found = name.lower() in line.lower()
        elif found:
            lines.append(line)
    return lines


def section_items(lines):
    """A section's top-level bullets, each with the bullets nested under it: [(head, [nested])]. Without
    bullets, each paragraph is one item."""
    marks = [(len(m.group(1)), m.group(2)) for m in (BULLET.match(line) for line in lines) if m]
    if not marks:
        paragraphs = [plain(p) for p in re.split(r'\n\s*\n', '\n'.join(lines))]
        return [(p, []) for p in paragraphs if p]
    top, items = min(indent for indent, _ in marks), []
    for line in lines:
        m = BULLET.match(line)
        if m and len(m.group(1)) <= top:
            items.append([m.group(2), []])
        elif m and items:
            items[-1][1].append(m.group(2))
        elif line.strip() and items:  # a wrapped line belongs to the bullet above
            if items[-1][1]:
                items[-1][1][-1] += ' ' + line.strip()
            else:
                items[-1][0] += ' ' + line.strip()
    return [(plain(head), [plain(n) for n in nested]) for head, nested in items]


def summary_items(text):
    """What a compaction summary says is still owed: Pending Tasks, then the Optional Next Step, the pure
    bookkeeping (a reply to send, a report to give) left out."""
    items = []
    for name in CARRIED:
        for head, nested in section_items(summary_section(text, name)):
            if head.endswith(':') and nested:  # a label: what it names is under it
                head, nested = f"{head} {'; '.join(n.rstrip(';.') for n in nested[:2])}", nested[2:]
            if head and not BOOKKEEPING.match(head) and OWED.search(head):
                items.append((head, nested))
    return items


def replies_to(words, thread_words):
    """A prompt is about a thread when it names something the thread names, past the words every prompt
    here uses."""
    return any(len(w) > 3 and w not in GENERIC for w in words_of(words or '') & thread_words)


def hhmm(t):
    return datetime.fromtimestamp(t).strftime('%m-%d %H:%M') if t else ''


def lost_threads(summaries, typed=()):
    """The threads across a session's compaction summaries [(time, text)], ranked: lost and waiting on you,
    lost, open and waiting, open, then the guesses. A thread is open while the newest summary still carries
    it and lost once it dropped out; the same thread in two summaries is two items sharing most of their
    words. Two guesses rank a lost thread low, never hide it (f lists every thread with why): answered, when
    it waited on you and one of your next two prompts said go and named it; faded, when it showed in one
    summary only and never waited on you."""
    summaries = sorted(summaries)
    threads = []
    for index, (when, text) in enumerate(summaries):
        for head, nested in summary_items(text):
            words = words_of(head)
            best = max(threads, key=lambda t: len(words & t['words']) / len(words | t['words']) if words | t['words'] else 0,
                       default=None)
            if best is None or not same_words(words, best['words']):
                best = dict(text=head, first=when, words=words, first_words=' '.join(sorted(words)), carried=set())
                threads.append(best)
            best.update(text=head, nested=nested, words=words, last=when)
            best['carried'].add(index)
    import hashlib
    out = []
    for t in threads:
        state = 'open' if max(t['carried']) == len(summaries) - 1 else 'lost'
        waiting = bool(WAITING.search(' '.join([t['text']] + t['nested'])))
        after = [(when, words) for when, words in sorted(typed) if when > t['last']][:2]
        said = next(((when, words) for when, words in after
                     if APPROVED.search(words or '') and replies_to(words, t['words'])), None)
        why = 'in the newest summary' if state == 'open' else f"quiet since {hhmm(t['last'])}"
        if state == 'lost' and waiting and said:
            state, why = 'answered', f"guessed done {hhmm(said[0])}: you said “{fit(one_line(said[1]), 60)}”"
        elif state == 'lost' and not waiting and len(t['carried']) < 2:
            state, why = 'faded', 'in one summary, asked you nothing'
        rank = {'lost': 0 if waiting else 1, 'open': 2 if waiting else 3, 'faded': 3}.get(state, 4)
        out.append(dict(text=t['text'], detail=t['nested'], first_seen=t['first'], last_seen=t['last'],
                        times=len(t['carried']), state=state, waiting=waiting, rank=rank, why=why,
                        key=hashlib.sha1(t['first_words'].encode()).hexdigest()[:12]))
    return sorted(out, key=lambda t: (t['rank'], -t['last_seen']))


def lost_log():
    """What you did with each thread: key -> (drop or later, when). The last line for a key wins."""
    said = {}
    try:
        with open(LOST_LOG, encoding='utf-8') as handle:
            for line in handle:
                try:
                    r = json.loads(line)
                    said[r['key']] = (r['answer'], float(r['at']))
                except (ValueError, KeyError, TypeError):
                    continue
    except OSError:
        pass
    return said


def keep_lost(key, answer, at):
    os.makedirs(KEEP, exist_ok=True)
    with open(LOST_LOG, 'a', encoding='utf-8') as handle:
        handle.write(json.dumps(dict(key=key, answer=answer, at=at)) + '\n')


def lost_found(summaries, typed=(), carried=()):
    """The threads, each marked hidden: "drop" for good, "later" until the next compaction. A thread set
    aside for later is also the one thing prep-compact hands the summary, as a reminder. The session
    before this one adds its threads, less those this session's own summaries already carry."""
    threads, said = lost_threads(summaries, typed), lost_log()
    own = [words_of(t['text']) for t in threads]
    threads = sorted(threads + [c for c in carried if not any(same_words(words_of(c['text']), w) for w in own)],
                     key=lambda t: (t['rank'], -t['last_seen']))
    newest = max((when for when, _ in summaries), default=0)
    for t in threads:
        answer, at = said.get(t['key'], (None, 0))
        t['hidden'] = answer if answer == 'drop' or (answer == 'later' and at > newest) else None
        t['later'] = answer == 'later'
    return threads


CARRY_DAYS = 7  # a session last written longer ago than this before one began is not its restart
ASKS_YOU = re.compile(r"\?\s*$|\b(?:say go|when you say|your go|needs? your|want me to|should i|shall i|do you want|"
                      r"which (?:one|do you)|approve|your call|your pick)\b", re.I)
CARRY_DIR = 'carried'  # KEEP/carried/<session>.json: what the pane read, for --lost


def previous_session(path, started):
    """The session before this one in the same project: the newest transcript last written before this one
    began (a session still running beside this one is not it), within a week, started by a person at the
    terminal, not by `claude -p`."""
    if not path or not started:
        return None
    folder, mine, found = os.path.dirname(path), os.path.basename(path), []
    try:
        names = os.listdir(folder)
    except OSError:
        return None
    for name in names:
        other = os.path.join(folder, name)
        if not name.endswith('.jsonl') or name == mine:
            continue
        try:
            written = os.stat(other).st_mtime
        except OSError:
            continue
        if started - CARRY_DAYS * 86400 < written <= started:
            found.append((written, other))
    for _, other in sorted(found, reverse=True)[:6]:
        try:
            with open(other, 'rb') as handle:
                head = handle.read(1 << 16)
        except OSError:
            continue
        if re.search(rb'"entrypoint"\s*:\s*"cli"', head):
            return other
    return None


def last_ask(text):
    """The last thing Claude's words asked of the person, or None: a question, or an offer waiting on a go."""
    tail = ' '.join(str(text or '').split())[-900:]
    asks = [s.strip() for s in re.split(r'(?<=[.?!])\s+', tail) if ASKS_YOU.search(s.strip())]
    return asks[-1] if asks else None


def stopped_at(prev):
    """What the session before this one left after its last summary, which no summary holds: a prompt
    Claude was still answering when it stopped, else Claude's last question, else the last prompt typed
    after that summary. One thread, its detail the last prompts and Claude's last words."""
    since = prev.summaries[-1][0] if prev.summaries else 0
    after = [(when, words) for when, words in prev.typed if when > since and (words or '').strip()]
    last = prev.typed[-1] if prev.typed else None
    ask = last_ask(prev.said[1]) if prev.said else None
    if last and last[0] > prev.turn_end:
        text, waiting, rank = f"Cut off before Claude finished answering: “{fit(one_line(last[1]), 200)}”", True, 0
    elif ask and (not last or prev.said[0] > last[0]):
        text, waiting, rank = f"Claude asked last, no answer came: “{fit(ask, 200)}”", True, 0
    elif after:
        text, waiting, rank = f"Last asked before it ended: “{fit(one_line(after[-1][1]), 200)}”", False, 3
    else:
        return None
    detail = [f"{hhmm(when)} you: {fit(one_line(words), 160)}" for when, words in after[-3:]]
    if prev.said:
        detail.append(f"{hhmm(prev.said[0])} Claude: {fit(one_line(prev.said[1]), 200)}")
    import hashlib
    ended = max([prev.said[0] if prev.said else 0, last[0] if last else 0])
    return dict(text=text, detail=detail, first_seen=after[0][0] if after else ended, last_seen=ended, times=0,
                state='carried', waiting=waiting, rank=rank, came_from=(prev.id or '')[:8],
                why=f"session {(prev.id or '')[:8]}, after its last summary; ended {hhmm(ended)}",
                key=hashlib.sha1(f'stopped {prev.id}'.encode()).hexdigest()[:12])


def carried_threads(path, started):
    """What a restart would lose, read from the session before this one, no model: the threads its newest
    summary carried or dropped with no answer, plus what came after that summary (stopped_at), which is
    the newest work and the likeliest to be unanswered. A session that never compacted still gives that."""
    before = previous_session(path, started)
    if not before:
        return []
    prev = Session(os.path.basename(before)[:-6], path=before)
    prev.read_transcript()
    out = []
    for t in lost_threads(prev.summaries, prev.typed):
        if t['state'] in ('open', 'lost'):
            out.append(dict(t, state='carried', rank=0 if t['waiting'] else 1, came_from=prev.id[:8],
                            why=f"session {prev.id[:8]}, {'open' if t['state'] == 'open' else 'quiet'} when it ended"))
    tail = stopped_at(prev)
    return out + ([tail] if tail else [])


def carry_into(session):
    """In the background, once per pane: the session before this one's threads, kept for --lost too."""
    try:
        session.carried = carried_threads(session.path, session.started)
        os.makedirs(os.path.join(KEEP, CARRY_DIR), exist_ok=True)
        with open(os.path.join(KEEP, CARRY_DIR, f'{session.id}.json.tmp'), 'w', encoding='utf-8') as handle:
            json.dump(session.carried, handle)
        os.replace(os.path.join(KEEP, CARRY_DIR, f'{session.id}.json.tmp'), os.path.join(KEEP, CARRY_DIR, f'{session.id}.json'))
    except Exception:  # a carry that fails leaves this session's own threads, as before
        session.carried = session.carried or []


def read_carried(session_id):
    try:
        with open(os.path.join(KEEP, CARRY_DIR, f'{session_id}.json'), encoding='utf-8') as handle:
            got = json.load(handle)
        return got if isinstance(got, list) else []
    except (OSError, ValueError):
        return []


def lost_json(session_id):
    """--lost: the threads of this session a person has not dropped, for the pane's own tools and the
    compaction hook."""
    session = Session(session_id) if session_id else None
    if session:
        session.read_transcript()
    carried = read_carried(session_id) if session else []
    return [t for t in lost_found(session.summaries if session else [], session.typed if session else [], carried)
            if t['hidden'] != 'drop']


def lost_asks(thread):
    """The two things a thread can be asked to become, as the words typed into Claude's prompt (unsent)."""
    quiet = f"Lost and found: “{fit(thread['text'], 240)}” went quiet after {datetime.fromtimestamp(thread['last_seen']).strftime('%H:%M')}."
    return dict(
        side=one_line(f"{quiet} Revisit it as a side task: finish or close it in a few steps without derailing the "
                      f"current work, then tell me in one line."),
        sub=one_line(f"{quiet} Hand it to a background subagent with a five-part spec (objective, files, interfaces, "
                     f"constraints, verification), keep working on the current task, and report its result when it lands."))


def lost_sections(s, now):
    """The Lost and found section: the top three threads that went quiet or wait on you, once there is any."""
    every = (s or {}).get('lost') or []
    shown = [t for t in every if not t['hidden'] and t['rank'] < 3]
    when = lambda t: datetime.fromtimestamp(t).strftime('%m-%d %H:%M')
    items = []
    for k, t in enumerate(shown[:3]):
        asks = lost_asks(t)
        meta = ' · '.join(x for x in (
            f"from {t['came_from']}" if t['state'] == 'carried' else
            'quiet ' + stamp(t['last_seen'], now)[0] if t['state'] == 'lost' else 'open',
            f"{t['times']}×" if t['times'] else '') if x)
        items.append(dict(key='lost:' + t['key'], title=t['text'], lines=2, meta=meta, lost=t['key'], top=k == 0,
                          flag='waiting on you' if t['waiting'] else '', asks=asks, send=asks['side'],
                          detail=[t.get('why') or f"First appeared {when(t['first_seen'])}, last {when(t['last_seen'])}, "
                                  f"carried through {t['times']} {'compaction' if t['times'] == 1 else 'compactions'}."]
                          + t['detail'][:3]))
    if not items:
        return []
    return [dict(name='lost', title='Lost and found', right=f'{len(shown)} quiet · f all {len(every)}', items=items, empty='')]


LOST_ASKS = (('side', '→ side task'), ('sub', '→ subagent'), ('drop', '✗ drop'), ('later', 'later'))


def lost_rows(item, width, ui, is_open):
    """A thread's answers under its row (the top thread's always, any other's once opened), two to a row;
    each row is its own block, so a click knows the thread and, by its column, the answer."""
    if not item.get('lost') or not (item.get('top') or is_open):
        return []
    sent = ui.get('sent')
    if sent and sent[0] == item['key']:
        said = "✓ now in Claude's prompt" if sent[1] else "✗ couldn't reach Claude's pane"
        return [([' ' * ITEM + ink('faint') + fit(said, width - ITEM) + OFF], None)]
    half = (width - ITEM) // 2
    rows = []
    for pair in (LOST_ASKS[:2], LOST_ASKS[2:]):
        text, spans = ' ' * ITEM, []
        for k, (answer, label) in enumerate(pair):
            spans.append((ITEM + k * half, ITEM + (k + 1) * half, answer))
            text += ink('turn') + BOLD + label[0] + OFF + ink('quiet') + fit(label[1:], half - 2).ljust(half - 1) + OFF
        rows.append(([text], dict(key=f"{item['key']}:{pair[0][0]}", lost=item['lost'], item=item['key'],
                                  asks=item['asks'], spans=spans)))
    return rows


def lost_click(pane, act, col):
    """A click on a thread's answers: drop and later are kept in the log; the two asks are typed into
    Claude's prompt, unsent."""
    pick = next((answer for start, end, answer in act['spans'] if start <= col < end), None)
    if pick in ('drop', 'later'):
        keep_lost(act['lost'], pick, time.time())
    elif pick:
        agent = agent_pane(pane.agent)
        went = bool(agent) and herdr('pane', 'send-text', agent, act['asks'][pick]) is not None
        pane.sent = (act['item'], went, time.time())
    focus_back()


def mtime(path):
    try:
        return os.stat(path).st_mtime_ns
    except OSError:
        return None


def compiles(path):
    """None when the file compiles, else why not, short: what a hot reload checks before it runs an edit."""
    try:
        with open(path, encoding='utf-8') as handle:
            compile(handle.read(), path, 'exec')
    except SyntaxError as error:
        return f'{type(error).__name__} line {error.lineno}'
    except (OSError, ValueError) as error:
        return str(error)
    return None


def watch(root, limit, session_id, agent=None):
    pane = Pane(root, limit, session_id, agent)
    source = os.path.realpath(__file__)
    loaded = seen = mtime(source)
    reload = False
    redraw = {'now': True}
    signal.signal(signal.SIGWINCH, lambda *_: redraw.update(now=True))
    for ending in (signal.SIGTERM, signal.SIGHUP):  # closed or killed: still clear up below
        signal.signal(ending, lambda *_: sys.exit(0))
    mine = kept_file(PANES, session_id)
    if mine:  # while this file names a live pid, Claude's status lines draw here, not under its prompt
        os.makedirs(PANES, exist_ok=True)
        with open(mine, 'w', encoding='utf-8') as handle:
            json.dump(dict(pid=os.getpid()), handle)
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd) if os.isatty(fd) else None
    if saved:
        tty.setcbreak(fd)
    sys.stdout.write('\x1b]0;CSR\x07\x1b[?25l' + BLACK + (MOUSE_ON if saved else ''))
    try:
        last = 0.0
        while True:
            # hot reload: an edit to this file, once it has stood still a tick and compiles, runs in place
            now_seen = mtime(source)
            if now_seen != loaded and now_seen == seen:
                pane.held, loaded = compiles(source), now_seen
                if pane.held is None:
                    reload = True
                    break
                redraw['now'] = True
            seen = now_seen
            redraw['now'] = redraw['now'] or compaction_gone(pane)  # a compaction ended: draw it now
            if redraw['now'] or time.time() - last > 2:  # a frame costs ~15 ms; it draws only what changed
                pane.frame(redraw['now'])
                redraw['now'], last = False, time.time()
            try:
                ready, _, _ = select.select([sys.stdin] if saved else [], [], [], 1.0)
            except InterruptedError:
                continue
            if not ready:
                continue
            data = os.read(fd, 1024).decode('utf-8', 'replace')
            for button, x, y, kind in re.findall(r'\x1b\[<(\d+);(\d+);(\d+)([Mm])', data):
                if kind == 'M' and int(button) == 0:  # a left press; releases pass
                    pane.click(int(y) - 1, int(x) - 1)
                    redraw['now'] = True
                elif kind == 'M' and int(button) in (64, 65):  # the wheel, up or down
                    pane.scroll(WHEEL if int(button) == 65 else -WHEEL)
                    redraw['now'] = True
            keys = re.sub(r'\x1b\[<[\d;]*[Mm]', '', data).lower()
            if 'q' in keys:
                break
            if keys == '\x1b':
                pane.open, redraw['now'] = None, True
            if summary_keys(pane, keys) or threads_keys(pane, keys):
                redraw['now'] = True
            elif pane.view == 'main' and any(k in keys for k in ('j', 'k', ' ', '\x1b[a', '\x1b[b')):
                pane.scroll(-WHEEL if any(k in keys for k in ('k', '\x1b[a')) else 4 * WHEEL if ' ' in keys else WHEEL)
                redraw['now'] = True
            for key in keys:
                if key in ('d', 'l'):
                    pane.view, pane.open = ('main' if key == 'd' else 'log'), None
                    redraw['now'] = True
    except KeyboardInterrupt:
        pass
    finally:
        if mine and not reload:  # only while it names this pane; a reload keeps the pid, so keeps the file
            try:
                with open(mine, encoding='utf-8') as handle:
                    if json.load(handle).get('pid') == os.getpid():
                        os.remove(mine)
            except (OSError, ValueError, AttributeError):
                pass
        if saved:
            termios.tcsetattr(fd, termios.TCSADRAIN, saved)
        sys.stdout.write('\x1b_Ga=d,d=A\x1b\\' + (MOUSE_OFF if saved else '') + GROUND_BACK + '\x1b[?25h\n')
        sys.stdout.flush()
    if reload:  # the same pid in the same pane: herdr and the band above Claude's prompt still find it
        os.execv(sys.executable, [sys.executable] + (['-I'] if sys.flags.isolated else []) + [source] + sys.argv[1:])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--repo', required=True)
    parser.add_argument('--limit', type=int, default=120)
    parser.add_argument('--sid', help='the Claude Code session id (herdr takes --session for itself)')
    parser.add_argument('--agent', help="Claude's herdr pane, where a second click on a row puts it")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--json', action='store_true')
    mode.add_argument('--watch', action='store_true')
    mode.add_argument('--png')
    mode.add_argument('--note', help='a file Claude is reading or editing')
    mode.add_argument('--brief', action='store_true', help="CSR's memory near this session, for the dreamer")
    mode.add_argument('--lost', action='store_true', help="threads that went quiet or wait on you, from this session's compaction summaries")
    mode.add_argument('--keep', help='a dream or an answer, as JSON, for the dream log')
    mode.add_argument('--coach', action='store_true', help='words you said in 2+ sessions that Claude looked up, as band notes')
    args = parser.parse_args()
    if args.coach:
        print(json.dumps(coach_terms()))
        return
    if args.keep:
        record = json.loads(args.keep)
        if record.get('kind') in ('dream', 'look', 'chapter'):
            record['repo'] = repo_root(args.repo)
        keep(record)
        print('{}')
        return
    if args.note:
        path = os.path.realpath(args.note)
        root = repo_root(os.path.dirname(path))
        if not root:
            print('[]')
            return
        con = connect()
        try:
            print(json.dumps(notes_for(con, os.path.realpath(root), path, args.sid)))
        finally:
            con.close()
        return
    if args.lost:
        print(json.dumps(lost_json(args.sid)))
        return
    root = repo_root(args.repo)
    if args.brief:
        if not root or not args.sid:
            return
        con = connect()
        try:
            print(brief(con, os.path.realpath(root), args.sid))
        finally:
            con.close()
        return
    if args.json:
        if not root or not os.path.exists(DB):
            print(json.dumps(dict(repo=None, total=0, shifts=[], changes=[], changes_total=0, dreams=[], dreams_on=False,
                                  recalled=0, dreams_7d=0, dream_tokens_7d=0, reminded_7d=0,
                                  status=None if not root else '☾ ' + SETUP)))
            return
        data = load(root, args.limit, args.sid)
        data['status'] = status_text(data)
        print(json.dumps(public(data)))
    elif args.png:
        now, cell = time.time(), (16, 34)
        data = load(root, args.limit, args.sid)
        data.update(look=last_look(root), chapter=last_chapter(root, args.sid), story=session_story(args.sid),
                    claude_fields=claude_fields(args.sid, now), readings=how_claude_read(args.sid))
        os.makedirs(args.png, exist_ok=True)
        if args.sid:
            s = Session(args.sid).load()
            open(os.path.join(args.png, 'session.png'), 'wb').write(session_png(s, 40, cell, now))
            lines, _ = draw_main(data, s, 46, 30, now)
            open(os.path.join(args.png, 'screen.txt'), 'w', encoding='utf-8').write('\n'.join(lines) + '\n')
        print(args.png)
    else:
        if not root:
            print('not a git repository:', args.repo)
            return
        watch(root, args.limit, args.sid, args.agent)


if __name__ == '__main__':
    main()
