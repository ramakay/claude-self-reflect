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
  pane.py --repo PATH --watch [--sid ID] [--agent PANE]  full-screen pane: d main, l log, s summary, f threads,
      i session info, r reads now (the legend counts down to the next read), q quits;
      a click opens a row, a second click puts it in Claude's prompt (herdr pane PANE)
  pane.py --repo PATH --png DIR [--sid ID]    write the pane's pictures, for a look
  pane.py --repo PATH --note FILE [--sid ID]  what CSR remembers about FILE, as notes for the band
  pane.py --repo PATH --brief --sid ID        what CSR remembers near this session, for the dreamer
  pane.py --repo PATH --keep JSON             add a dream, or an answer to one, to the dream log
  pane.py --repo PATH --compact-plan --sid ID --ctx N   the payback of compacting now at N tokens, for the mod
  pane.py --repo PATH --compact-usual         how long your own /compact takes, kept for the compaction card
"""
import argparse
import base64
import bisect
import fcntl
import glob
import importlib.util
import io
import json
import math
import os
import plistlib
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
import traceback
import tty
from collections import Counter, namedtuple
from datetime import datetime, timezone

HOME = os.environ.get('HOME', '')


def sibling(name):
    """A module that ships beside this script, loaded by its path: python3 -I keeps the script's folder off sys.path."""
    spec = importlib.util.spec_from_file_location(name, os.path.join(os.path.dirname(os.path.realpath(__file__)), f'{name}.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


private = sibling('private')  # every write below goes through it: folders 0700, files 0600, credentials redacted
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
FORMER_DREAM_LOG = os.path.join(HOME, '.claude-self-reflect', 'csr-shifts', 'dreams.jsonl')  # the mod's earlier name
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


def csr_project(path):
    """csr-engine's project name for a folder (resolve_project_from_cwd): the folder after 'projects',
    else the folder's own name."""
    parts = [part for part in path.split('/') if part]
    for i, part in enumerate(parts[:-1]):
        if part == 'projects':
            return parts[i + 1]
    return parts[-1] if parts else None


def remembered_root(path):
    """A folder git does not know, when CSR has chunks from its project: Claude's transcripts are kept
    per folder, not per repo, so such a folder has history all the same."""
    if not path or not os.path.isdir(path) or not os.path.exists(DB):
        return None
    folder = os.path.realpath(path)
    project = csr_project(folder)
    if not project:
        return None
    try:
        con = connect()
        try:
            known = con.execute('select 1 from chunks where project_name=? limit 1', (project,)).fetchone()
        finally:
            con.close()
    except sqlite3.Error:
        return None
    return folder if known else None


def pane_root(path):
    """The folder the pane reads: its git repo, else the folder itself when CSR remembers it."""
    return repo_root(path) or remembered_root(path)


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
    if not repo_root(root):
        return floor, []  # a folder CSR remembers but git does not: no commits to have changed anything
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


def load_changes(con, shifts, session_id=None, before=None):
    """Changed since: the latest past conversation that shaped a symbol, then a commit that changed
    it after that conversation's last message. A commit that landed while the conversation was
    still going is its own work (or one it watched land), not news to it.

    `before`, a time (the session's last compaction), also lets this session's own work count when it
    was done before then: the context that saw it is gone, so the commit is a receipt the dreamer can
    stand on. Only the dreamer's brief passes it; a band note never does."""
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
                 if conv and epoch(ts) and epoch(ts) <= at
                 and ((ended(conv) or at) < at or (before and conv == session_id and epoch(ts) < before))]
        if not talks:
            continue
        talked, conv = max(talks)
        if conv != session_id:
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
        change['earlier'] = before if change['current'] and before else None  # this session, before its compaction
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


def notes_for(con, root, path, session_id=None, before=None):
    """Notes for one file, best first: a past session's work on it that a later commit changed,
    then a rule the person set for it. Each carries its receipts; no model calls. `before`: see
    load_changes (the dreamer's brief only)."""
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
    for change in load_changes(con, mine, session_id, before):
        if change['current'] and not change['earlier']:
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
        if change['earlier']:
            line = (f"{lead} in {name} {verb} in {latest['oid'][:7]} on {changed}, after this session worked on {them} "
                    f"before its {datetime.fromtimestamp(change['earlier']).strftime('%m-%d %H:%M')} compaction.")
        text = (f"On {talked} you asked {quoted(change['title'], 90)}. That session worked on "
                f"{', '.join(f'`{x}`' for x in symbols[:4])}{' and more' if len(symbols) > 4 else ''} in {rel}. "
                f"On {changed}, commit {latest['oid'][:7]} {quoted(latest['subject'], 70)} {verb.replace('was ', '')} "
                f"{them}. What that session concluded about {them} may no longer hold, so check the code, not the memory.")
        sketch = [f"{talked}  \u25cf your session  {quoted(change['title'], 34)}",
                  f"{changed}  \u25b2 {latest['oid'][:7]}  {', '.join(symbols[:3])}{' +' + str(len(symbols) - 3) if len(symbols) > 3 else ''}"]
        notes.append(dict(id=f"shift:{rel}:{latest['oid'][:12]}:{change['conversation'][:8]}", kind='shift', file=rel,
                          line=line, title=f"Your {talked} work on {symbols[0]} has changed since",
                          text=text, sketch=sketch, symbol=symbols[0], oid=latest['oid'],
                          conversation=change['conversation'], at=latest['at'], earlier=bool(change['earlier'])))
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


def fit_pieces(text, width):
    """The ' · '-joined pieces of text that fit width, the rightmost dropped first."""
    pieces = text.split(' · ')
    while len(pieces) > 1 and len(' · '.join(pieces)) > width:
        pieces.pop()
    return fit(' · '.join(pieces), width)


def _env_int(name, default):
    try:
        return int(os.environ.get(name, default))
    except ValueError:
        return default


CTX_AMBER = max(1, _env_int('CC_CTX_AMBER_TOKENS', 100_000))  # the status line's own thresholds and env names
CTX_RED = max(1, _env_int('CC_CTX_RED_TOKENS', 400_000))  # also the bar's full scale: never 0, it divides
CTX_HOT = max(1, _env_int('CC_CTX_HOT_TOKENS', 200_000))  # the status line's middle step: the vertical tape reds from here
FLOOR_ROWS = 5  # the status line's FLOOR_ROWS: the floor is the median of this many newest compactions
LAUNCH_AGENTS = os.path.join(HOME, 'Library', 'LaunchAgents')
DAEMON_LOCK = os.path.join(HOME, '.claude-self-reflect', 'daemon.lock')
SETTINGS = os.path.join(HOME, '.claude', 'settings.json')
SESSIONS = os.path.join(HOME, '.claude', 'sessions')  # Claude Code: <pid>.json per live session
_RECORD = {}  # session id -> the <pid>.json path last found for it


def fail_open(call, empty, *args):
    """call(*args), or empty when it raises: a source that cannot be read draws as if it were absent."""
    try:
        return call(*args)
    except Exception:
        return empty


def kept_status(session_id, now):
    """(fields, at) the wrapper kept for this session; ({}, None) when there are none or they are a day old.
    Like the line, an idle session's fields are old and still right: the clocks inside them are absolute."""
    try:
        with open(kept_file(STATUS_LINES, session_id) or '', encoding='utf-8') as handle:
            kept = json.load(handle)
        fields, at = kept.get('fields'), float(kept.get('at') or 0)
        return (fields, at) if isinstance(fields, dict) and now - at < 86400 else ({}, None)
    except (OSError, ValueError, TypeError, AttributeError):
        return {}, None


def claude_fields(session_id, now):
    """The raw fields the status line wrapper kept beside the line (prompt_cache, rate_limits,
    context_window); {} when there are none or they are a day old."""
    return kept_status(session_id, now)[0]


_LIMITS = {}  # kept file -> (mtime, size, at, rate_limits): a file is parsed again only when it changes


def account_limits(now, folder=None):
    """rate_limits from the newest KEEP/statusline/*.json (by its 'at') written within a day; {} when there
    are none. The quotas are the account's, so a session that has no status line yet still shows them."""
    folder = folder or STATUS_LINES
    try:
        names = os.listdir(folder)
    except OSError:
        return {}
    best = None
    for name in names:
        if not name.endswith('.json'):
            continue
        path = os.path.join(folder, name)
        try:
            stat = os.stat(path)
            hit = _LIMITS.get(path)
            if not hit or hit[:2] != (stat.st_mtime_ns, stat.st_size):
                with open(path, encoding='utf-8') as handle:
                    kept = json.load(handle)
                limits = (kept.get('fields') or {}).get('rate_limits')
                hit = (stat.st_mtime_ns, stat.st_size, float(kept.get('at') or 0), limits if isinstance(limits, dict) else None)
                _LIMITS[path] = hit
        except (OSError, ValueError, TypeError, AttributeError):
            continue
        if hit[3] and now - hit[2] < 86400 and (best is None or hit[2] > best[0]):
            best = (hit[2], hit[3])
    return best[1] if best else {}


def ctx_now(fields, kept_at, s):
    """(tokens, size): the newest s['main'] reply when its time > (kept_at or 0) + 5, else
    fields.context_window.total_input_tokens, or the sum of current_usage's input, cache-read and
    cache-create tokens. (None, size) when there is neither. size is context_window_size, or None."""
    window = (fields or {}).get('context_window') if isinstance(fields, dict) else None
    window = window if isinstance(window, dict) else {}
    size = percent_of(window.get('context_window_size'))
    size = size if size and size > 0 else None
    main = (s or {}).get('main') or []
    if main:
        when, tokens = max(main)
        if when > (kept_at or 0) + 5 and tokens:
            return tokens, size
    tokens = percent_of(window.get('total_input_tokens'))
    if tokens is None and isinstance(window.get('current_usage'), dict):
        use = window['current_usage']
        tokens = sum(percent_of(use.get(k)) or 0 for k in ('input_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens'))
    return (tokens if tokens and tokens > 0 else None), size


def median_of(values):
    values = sorted(values)
    if not values:
        return None
    mid = len(values) // 2
    return values[mid] if len(values) % 2 else (values[mid - 1] + values[mid]) / 2


TTL = re.compile(r'\s*(\d+(?:\.\d+)?)\s*([smhd])\s*')
CACHE_LOW = 600  # seconds of cache left under which its tape turns amber


def ttl_seconds(text):
    """The cache's lifetime from the status line's ttl ('1h', '5m'); None when it says nothing a clock can use."""
    match = TTL.fullmatch(text) if isinstance(text, str) else None
    if not match:
        return None
    seconds = float(match.group(1)) * dict(s=1, m=60, h=3600, d=86400)[match.group(2)]
    return seconds if 0 < seconds <= 86400 else None


def cache_info(cache, now):
    """The prompt cache for the masthead: dict(text, sev, long, cold, left, ttl) - how long it stays warm, or
    what the next turn re-reads once it is cold; '--' when the status line told nothing about it. left and
    ttl, the seconds still warm and the cache's lifetime, are what its tape drains; None when unknown."""
    none = dict(text='CACHE --', sev='normal', long='--', cold=False, left=None, ttl=None)
    if not isinstance(cache, dict):
        return none
    expires, warm, cold_cost = seconds_of(cache.get('expires_at')), cache.get('warm'), percent_of(cache.get('recache_tokens_if_cold'))
    if warm is False or (expires is not None and expires <= now):
        cost = f' ~{short(cold_cost)}' if cold_cost and cold_cost > 0 else ''
        return dict(none, text='CACHE COLD' + cost, sev='amber', long='COLD' + (f' · re-reads{cost}' if cost else ''),
                    cold=True)
    if expires is not None and expires - now > 86400:
        expires = None  # no prompt cache lives a day: that clock is wrong, so say only that it is warm
    if expires is None:
        if not warm:
            return none
        ttl = one_line(cache['ttl'], 8) if isinstance(cache.get('ttl'), str) and cache['ttl'].strip() else ''
        return dict(none, text=f"CACHE {ttl or 'WARM'}", sev='normal', long='WARM' + (f' · ttl {ttl}' if ttl else ''))
    left = expires - now
    lifetime = ttl_seconds(cache.get('ttl'))
    clock = dict(left=left, ttl=max(lifetime, left) if lifetime else None)
    if left < 60:
        return dict(none, text='CACHE <1m', sev='amber', long='WARM · under 1m left', **clock)
    return dict(none, text=f'CACHE {span(left)}', sev='normal' if left >= 300 else 'amber', long=f'WARM · {span(left)} left',
                **clock)


def reset_label(resets, now):
    """When a window resets: the time alone when that is today (local date), else with the weekday."""
    when = datetime.fromtimestamp(resets)
    return when.strftime('%H:%M' if when.date() == datetime.fromtimestamp(now).date() else '%a %H:%M')


WINDOWS = {'5H': 5 * 3600, '7D': 7 * 86400}  # each plan window's length: its pace is how much of it has gone


def pace_of(code, resets, now):
    """The share of a plan window already gone at now, 0..1: where spending evenly would have used it to.
    None when the window or its reset is unknown, or the reset is further off than the window is long."""
    length = WINDOWS.get(code)
    if not length or not isinstance(resets, (int, float)) or resets != resets or resets <= now:
        return None
    left = resets - now
    return None if left > length else 1 - left / length


def quota_slot(code, window, now):
    """One plan window for the masthead: dict(code, text, sev, used, resets, stale, pace). A window that rolled
    over since the status line last ran is '--' in amber (it was a value and is invalid now); one never
    reported is '--' in the normal ink. Never a stale number."""
    used = percent_of(window.get('used_percentage')) if isinstance(window, dict) else None
    if used is None:
        return dict(code=code, text=f'{code} --', sev='normal', used=None, resets=None, stale=False, pace=None)
    resets = seconds_of(window.get('resets_at'))
    if resets is not None and resets <= now:
        return dict(code=code, text=f'{code} --', sev='amber', used=None, resets=resets, stale=True, pace=None)
    text = f'{code} {round(used)}%'
    if used > 70 and resets is not None:
        try:
            text += ' ↻' + reset_label(resets, now)
        except (OverflowError, OSError, ValueError):
            pass  # a reset time this machine cannot show: the percentage alone
    return dict(code=code, text=text, sev='red' if used >= 90 else 'amber' if used > 70 else 'normal',
                used=used, resets=resets, stale=False, pace=pace_of(code, resets, now))


# One compaction's cost, in input tokens at the full price: the post-compaction context written to cache at twice
# the price (the 1-hour rate), the summary written out at five times, and the context re-read once at a tenth to
# be summarised. Each later request then re-reads (context - floor) fewer tokens at a tenth of the price: a
# request is one API call, and a turn makes many (17 a turn here, 10-12). The floor is this session's own median
# context on the first request after a compaction: 61k to 89k over 67 compactions here (median 65k), where
# compact_boundary's postTokens, the summary alone, reads 16k to 26k. That summary is what the compaction writes
# out: postTokens, measured (the old 4,500 guess left 87k out of the cost; 10-12 20:53 it was 21,730).
SUMMARY_GUESS = 21700  # tokens of summary a compaction writes when this session has measured none
PAYBACK_UNDER = 15  # a payback longer than this many turns is not worth saying
BAND = (180_589, 269_178)  # compact.ts BAND: low and high, the p10 and p50 of the owner's own compactions
PAYBACK_FROM = BAND[0]  # under the band CSR never offers to compact, so no ♻ (10-12: ♻ 9t at 95k read as advice)
FLOOR_GUESS = 65000  # post-compaction context when a session's own are missing
TURNS_MEASURED = 10  # requests a turn: the mean of this many of the session's newest whole turns


def compaction_cost(floor, context, summary=SUMMARY_GUESS):
    """What one compaction at this context costs, in input tokens at the full price."""
    return floor * 2 + summary * 5 + context * 0.1


def request_saving(floor, context):
    """What each request after a compaction at this context saves, in input tokens at the full price."""
    return (context - floor) * 0.1


def payback_requests(floor, context, summary=SUMMARY_GUESS):
    """Requests until compacting at this context pays back, as the status line works it out; None when it would
    never (the context is at or under the floor)."""
    saved = request_saving(floor, context)
    if saved <= 0:
        return None
    return compaction_cost(floor, context, summary) / saved


def requests_per_turn(main, prompts, last=TURNS_MEASURED):
    """Main-chain requests a turn: the mean over the newest `last` whole turns that made one (a turn runs from one
    prompt to the next; the running one is not whole), or None under two. The status line counts the same."""
    times, turns = sorted(t for t, _ in main or ()), sorted(prompts or ())
    counts = [bisect.bisect_left(times, b) - bisect.bisect_left(times, a) for a, b in zip(turns, turns[1:])]
    counts = [n for n in counts if n > 0][-last:]
    return sum(counts) / len(counts) if len(counts) >= 2 else None


def payback_turns(requests, per_turn):
    """The payback in turns at this session's requests a turn; unmeasured, each request is taken as a turn (the
    cautious reading: it never says sooner than it knows)."""
    return requests / per_turn if per_turn else requests


def payback_prompts(requests, per_turn):
    """The payback in your prompts: the API calls it takes over this session's calls a prompt, rounded up (a prompt
    under way counts whole); None while the calls a prompt are unmeasured. 10-12: "2 of 14 requests to pay back,
    no clue still": a prompt is the unit the owner counts in."""
    if requests is None or not per_turn:
        return None
    return max(1, math.ceil(requests / per_turn))


def calls_text(n):
    """'1 API call', '14 API calls'."""
    return f"{n} API {'call' if n == 1 else 'calls'}"


def payback_text(requests, per_turn):
    """'pays back in ~14 API calls (~2 prompts)': the mod's plan, offer and notice say it the same way."""
    if requests is None:
        return ''
    said = f'pays back in ~{calls_text(max(1, int(requests + 0.5)))}'
    prompts = payback_prompts(requests, per_turn)
    return f"{said} (~{prompts} {'prompt' if prompts == 1 else 'prompts'})" if prompts else said


def median_last(values, rows=FLOOR_ROWS):
    """(median, True) of the last `rows` positive numbers in values, rounded as JavaScript does; (None, False)."""
    kept = sorted([v for v in values or () if isinstance(v, (int, float)) and not isinstance(v, bool) and v > 0][-rows:])
    if not kept:
        return None, False
    mid = len(kept) // 2
    return (kept[mid] if len(kept) % 2 else int((kept[mid - 1] + kept[mid]) / 2 + 0.5)), True


def floor_of(sizes):
    """(tokens, measured): the median of the last FLOOR_ROWS measured posts (p > 0) in compact_sizes, else
    (FLOOR_GUESS, False). The status line takes the same last few, and rounds as JavaScript does."""
    posts = [row[1] if isinstance(row, (tuple, list)) and len(row) > 1 else row for row in sizes or ()]
    floor, measured = median_last(posts)
    return (floor, True) if measured else (FLOOR_GUESS, False)


def summary_of(summaries):
    """(tokens, measured): the median of this session's last FLOOR_ROWS compact_boundary postTokens, else
    (SUMMARY_GUESS, False)."""
    size, measured = median_last(summaries)
    return (size, True) if measured else (SUMMARY_GUESS, False)


def turn_ends(main, prompts):
    """[(prompt time, context)]: the context at each turn's last main-chain reply, a turn running from one
    prompt to the next; a turn with no reply has none."""
    turns = sorted(prompts or ())
    main = sorted(main or ())
    times, ends = [r[0] for r in main], []
    for i, t in enumerate(turns):
        low = bisect.bisect_left(times, t)
        high = bisect.bisect_left(times, turns[i + 1]) if i + 1 < len(turns) else len(main)
        if high > low:
            ends.append((t, main[high - 1][1]))
    return ends


def ctx_trend(main, prompts, compacts, ends=None):
    """Tokens per turn: the context at each turn's end since the newest compaction; the median of the last five
    turn-to-turn steps, or None under three turns. ends: turn_ends(main, prompts), when already worked out."""
    since = max(compacts or [0])
    ends = [c for t, c in (turn_ends(main, prompts) if ends is None else ends) if t > since]
    if len(ends) < 3:
        return None
    return median_of([b - a for a, b in zip(ends, ends[1:])][-5:])


def csr_plist_switches(agents):
    """(key, value, where) for every variable a csr-engine LaunchAgent sets; the caller keeps only CSR_NO_*."""
    out = []
    for path in sorted(glob.glob(os.path.join(agents, '*csr-engine*.plist'))):
        try:
            with open(path, 'rb') as handle:
                env = plistlib.load(handle).get('EnvironmentVariables')
        except Exception:
            continue  # a plist that is not XML raises ExpatError, which is no ValueError: any one that fails is skipped
        out += [(k, v, 'daemon') for k, v in (env.items() if isinstance(env, dict) else ())]
    return out


_STS = {}  # (agents, lock, settings) -> (when read, result)
QUIET_SWITCHES = ('RATIFICATION',)  # set off on purpose and never said: 10-12, "dont care about ratification"


def csr_status(now, agents=LAUNCH_AGENTS, lock=DAEMON_LOCK, settings=SETTINGS):
    """What CSR runs without: dict(sev, inop, daemon). inop = [(label, value, sev)]: a daemon with no live pid
    (amber), then every CSR_NO_* switch set in a LaunchAgent plist (where 'daemon') or in settings.json's env
    (where 'hooks'), advisory, but QUIET_SWITCHES. Only keys named CSR_NO_* are ever read out. Cached for a minute."""
    key = (agents, lock, settings)
    hit = _STS.get(key)
    if hit and 0 <= now - hit[0] < 60:
        return hit[1]
    inop, daemon = [], dict(up=False, pid=None, since=None)
    try:
        with open(lock, encoding='utf-8') as handle:
            pid = int(handle.read().strip())
        if pid <= 1:
            raise ValueError(pid)
        try:
            os.kill(pid, 0)
        except PermissionError:
            pass  # alive, and not ours
        stamp_ns = mtime(lock)
        daemon = dict(up=True, pid=pid, since=stamp_ns / 1e9 if stamp_ns else None)
    except (OSError, ValueError, OverflowError):
        pass  # no lock, a garbled or absurd one, or no such process (ProcessLookupError is an OSError)
    if not daemon['up']:
        inop.append(('DAEMON', 'DOWN · no live pid in daemon.lock', 'amber'))
    switches = csr_plist_switches(agents)
    try:
        with open(settings, encoding='utf-8') as handle:
            env = json.load(handle).get('env')
        switches += [(k, v, 'hooks') for k, v in (env.items() if isinstance(env, dict) else ())]
    except (OSError, ValueError, AttributeError):
        pass
    for name, value, where in switches:
        if isinstance(name, str) and re.fullmatch(r'CSR_NO_[A-Z0-9_]+', name) and name[7:] not in QUIET_SWITCHES \
                and str(value).strip().lower() not in ('', '0', 'false'):
            inop.append((name[7:], f'INHIBITED · {name}={one_line(value, 24)} ({where})', 'advisory'))
    sev = 'amber' if any(row[2] == 'amber' for row in inop) else 'advisory' if inop else None
    result = dict(sev=sev, inop=inop, daemon=daemon)
    _STS[key] = (now, result)
    return result


def compacting_slot(info, bar=True, rng=True):
    """(text, sev) of the payback's slot while a compaction runs: ♻ m:ss (♻A m:ss for an auto compaction), as
    column mode says it, then a bar against the usual time and the usual range. Amber once it is past the usual
    longest, else advisory."""
    usual, elapsed = info.get('usual'), info['elapsed']
    text = ('♻A ' if info.get('trigger') == 'auto' else '♻ ') + clock_text(elapsed)
    if usual and bar:
        text += ' ' + compaction_bar(elapsed, usual['high'], 8)
        if rng:
            text += f" {clock_text(usual['low'])}–{clock_text(usual['high'])}"
    return text, 'amber' if usual and elapsed > usual['high'] else 'advisory'


def payback_slot(floor, measured, tokens, summary, per_turn):
    """dict(text, sev, forms) of the masthead's payback in words, never codes (10-12: "14r", "2t", no clue): the
    prompts it takes, '♻ ~2 prompts', then the API calls, '♻ 14 calls' ('♻ ~14 calls' on a guessed floor), the
    form the row has room for (forms, longest first; text the first); calls alone while the session's calls a
    prompt are unmeasured; '♻ >15 prompts' past PAYBACK_UNDER. The ink: amber at a prompt or less, advisory to
    three; never red, which the context's own size says. Under PAYBACK_FROM, no text: the status line hides its ♻
    there too."""
    if tokens < PAYBACK_FROM:
        return dict(text='', sev='normal', forms=[])
    requests = payback_requests(floor, tokens, summary)
    turns = None if requests is None else payback_turns(requests, per_turn)
    if turns is None or turns > PAYBACK_UNDER:
        forms = [f"♻ >{PAYBACK_UNDER} {'prompts' if per_turn else 'calls'}"]
        return dict(text=forms[0], sev='normal', forms=forms)
    n, prompts = max(1, int(requests + 0.5)), payback_prompts(requests, per_turn)
    calls = f"♻ {'' if measured else '~'}{n} {'call' if n == 1 else 'calls'}"
    forms = ([f"♻ ~{prompts} {'prompt' if prompts == 1 else 'prompts'}"] if prompts else []) + [calls]
    return dict(text=forms[0], sev='amber' if turns <= 1 else 'advisory' if turns <= 3 else 'normal', forms=forms)


def gauges(fields, kept_at, s, now, limits=None, sts=None, compacting=None):
    """Every number the masthead draws, with its severity:
      ctx=dict(tokens, size, sev)      sev normal (under CTX_AMBER, or no data), amber, red (CTX_RED)
      floor=dict(tokens, measured)     floor_of(compact_sizes)
      trend=int|None                   tokens per turn since the newest compaction
      turns=[int]                      the context at each of the last SPARK_MAX turns' ends, oldest first
      payback=dict(text, sev)          ♻ turns to break even (♻<1t under one; ♻ 14r, in requests, before this
                                       session has two whole turns); the timer while a compaction runs
      cache=dict(text, sev, long)      the prompt cache
      quotas=[dict(code, text, sev, used, ...)]   riskier first; '--' last
      sts, compacting                  as given
    limits: account_limits(now), used only when fields has no rate_limits."""
    fields = fields if isinstance(fields, dict) else {}
    s = s or {}
    tokens, size = ctx_now(fields, kept_at, s)
    sev = 'normal' if tokens is None or tokens < CTX_AMBER else 'amber' if tokens < CTX_RED else 'red'
    floor, measured = floor_of(s.get('compact_sizes'))
    if compacting:
        payback = dict(zip(('text', 'sev'), compacting_slot(compacting)))
    elif tokens is None:
        payback = dict(text='♻ --', sev='normal')
    else:
        payback = payback_slot(floor, measured, tokens, summary_of(s.get('summary_sizes'))[0],
                               requests_per_turn(s.get('main'), s.get('prompts')))
    rates = fields.get('rate_limits')
    rates = rates if isinstance(rates, dict) and rates else (limits if isinstance(limits, dict) else {})
    quotas = [quota_slot(code, rates.get(key), now) for code, key in (('5H', 'five_hour'), ('7D', 'seven_day'))]
    quotas.sort(key=lambda q: -q['used'] if q['used'] is not None else 1)  # stable: 5H before 7D on a tie
    ends = turn_ends(s.get('main'), s.get('prompts'))
    return dict(ctx=dict(tokens=tokens, size=size, sev=sev), floor=dict(tokens=floor, measured=measured),
                trend=ctx_trend(s.get('main'), s.get('prompts'), s.get('compacts'), ends),
                turns=[c for _, c in ends][-SPARK_MAX:], payback=payback,
                cache=cache_info(fields.get('prompt_cache'), now), quotas=quotas,
                sts=sts or dict(sev=None, inop=[]), compacting=compacting)


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
    private.append(DREAM_LOG, private.dumps(record, ensure_ascii=False) + '\n', root=KEEP)


SETTLED = ('helpful', 'not_relevant', 'ask_claude', 'thought_of_it', 'got_it', 'dismiss', 'ignored', 'turn_off',
           'expired', 'sent', 'rejected')
ADOPTED = ('helpful', 'ask_claude', 'sent')  # a dream you took up: it gives its place in the chapter back
# "sent": the dream reached Claude as a prompt (the mod logs it on prompt.submit). It says more than a dismiss,
# a typing-past or the box being filled, and less than your own verdict on it.
# "rejected": you sent it on and said no to it, beside it or in your next prompt; the record keeps your words
# ("said"). 10-12 19:53 such a dream was logged sent, as if adopted.
PER_CHAPTER = 5


def load_dream_log(root=None, path=None):
    """Dreams newest first, each with the answer that settled it (None while it is open) and, where you said why,
    your words ("said")."""
    dreams = {}
    try:
        with open(path or DREAM_LOG, encoding='utf-8') as handle:
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
            # "helpful" or "not relevant" says more than the dismiss that led to it; your no, more than the sending
            if dream['answer'] in (None, 'dismiss', 'got_it', 'ignored') or record['answer'] in ('helpful', 'not_relevant', 'rejected') \
                    or (record['answer'] == 'sent' and dream['answer'] in ('ask_claude', 'expired')):
                dream['answer'] = record['answer']
                dream['said'] = one_line(str(record['said']), 300) if record.get('said') else None
    out = [d for d in dreams.values() if not root or not d.get('repo') or d['repo'] == root]
    return sorted(out, key=lambda d: d['at'], reverse=True)


def sample_dream():
    """The dream you took up last, from any folder, the mod's earlier log too: what the pane shows
    while dreams are off. One you marked helpful beats one you only handed to Claude."""
    taken = {}
    for path in dict.fromkeys((DREAM_LOG, FORMER_DREAM_LOG)):
        for dream in load_dream_log(path=path):
            if dream['answer'] in ('helpful', 'ask_claude', 'sent'):
                taken.setdefault(dream['id'], dream)
    if not taken:
        return None
    best = max(taken.values(), key=lambda d: (d['answer'] == 'helpful', d['at']))
    return dict(line=best['line'], at=best['at'], answer=best['answer'],
                project=os.path.basename(best.get('repo') or '') or None)


LOOKED = dict(  # what came of a look, as the pane says it
    dream='it asked a new question',
    quiet="nothing new in CSR's memory near this work, so no look",
    none='nothing worth asking',
    ungrounded='its question stood on no other session, so it was dropped',
    repeat='its question had been asked before, so it was dropped',
    decided='its question offered another way to what you just decided, so it was dropped',
    stale='its only reason was a line of yours older than 30 days, so it was dropped',
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


# Text in the prompt history that the person did not type: a workflow harness's computed task, a side agent's
# note or the dreamer's question passed on, Claude Code's own markers. On 10-09 two such lines reached the brief
# and the dreamer quoted them as "you said" (sessions 0450a37e, 51a3c211).
# What CSR or a side agent wrote and the person forwarded is cut by its own grammar (own_words, strip_forwards), the
# person's words beside it kept.
NOT_YOURS = re.compile(r'^\s*\[(?:Workflow harness|Request interrupted|CSR)|not typed by this session', re.I)


def past_lines(path, session_id):
    """From the prompt history: the person's asking habits, counted, and their "what if" style lines from
    other sessions, in their own words only (pasted blocks and quoted lines out, harness and side-agent text
    never)."""
    counts, lines = Counter(), []
    try:
        handle = open(path, 'rb')
    except OSError:
        return counts, []
    with handle:
        for raw in handle:
            try:
                entry = json.loads(raw)
            except ValueError:
                continue
            said = entry.get('display') or ''
            if NOT_YOURS.search(said):
                continue
            text = own_words(said).strip()
            if not text or text.startswith('/'):
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
    return counts, lines


def voice_and_lines(con, session_id, limit=4, floor=0.40):
    """The person's asking habits, counted, and their past "what if" style lines that sit closest
    to this session: CSR's stored embeddings, this session's latest chunks against the chunk each
    line was said in. No model calls."""
    counts, lines = past_lines(os.path.join(HOME, '.claude', 'history.jsonl'), session_id)
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
    # The six files asked about are the session's newest files in git, not the last half hour's: on 10-09/12
    # 135 of 144 files in the half-hour slots were screenshots, /tmp, memory notes or a mod's copy, none in git.
    notes, seen, roots = [], set(), {}

    def root_of(path):
        folder = os.path.dirname(path)
        if folder not in roots:
            owner = repo_root(folder) if os.path.isdir(folder) else None
            roots[folder] = owner and os.path.realpath(owner)
        return roots[folder]
    tracked = [path for path in sorted(session.files, key=lambda p: session.last.get(p, 0), reverse=True) if root_of(path)]
    compacted = [at for at in session.compacts if at]
    before = max(compacted) if compacted else None  # this session's work before it counts as past
    for path in tracked[:6]:
        for note in notes_for(con, root_of(path), path, session_id, before):
            if note['id'] not in seen:
                seen.add(note['id'])
                notes.append(note)
    changed = [n for n in notes if n['kind'] == 'shift'][:5]
    rules = [n for n in notes if n['kind'] == 'rule'][:3]
    if changed:
        out += ['', "Code that changed after one of the person's past sessions, or this one before its last compaction, worked on it:"]
        out += [f"* {n['line']} [commit {n['oid'][:7]}" + ("]" if n.get('earlier') else f", session {n['conversation'][:8]}]")
                for n in changed]
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
        out += ['', 'Corrections the person typed in this repo lately, in their own words:']
        out += [f"- {short_date(epoch(ts)) if epoch(ts) else (ts or '')[:10]}: {quoted(quote, 200)} [session {sid[:8]}]"
                for ts, quote, sid in corrections]
    out += past_rows(voice_and_lines(con, session_id)[1], time.time())
    out += dream_rows(load_dream_log(root)[:8])
    return '\n'.join(out).strip()


CONTEXT_DAYS = 30  # a past line older than this gives context, never a dream's main reason
CONTEXT_ONLY = 'context only'  # dreamer.ts's leansOnOld finds the old lines by these words


def past_rows(near, now):
    """The brief's lines the person typed in past sessions (voice_and_lines), each with its receipt. One older than
    CONTEXT_DAYS says its age and '(… days old: context only)': it may give context, never be the main reason to ask.
    10-12 19:51 a dream's whole reason was a line typed 06-10, "my vision: you kill a session, you restart"; the person
    cannot kill a session. dreamer.ts's leansOnOld drops a dream that stands on such lines alone."""
    if not near:
        return []
    rows = ['', 'Lines the person typed in past sessions, in their own words, that sit close to this work '
                f'(a line older than {CONTEXT_DAYS} days is {CONTEXT_ONLY}: never the main reason to ask):']
    for _, when, sid, project, part in near:
        days = int((now - when) // 86400) if when else 0
        old = f' ({days} days old: {CONTEXT_ONLY})' if days > CONTEXT_DAYS else ''
        rows.append(f"- {short_date(when)} ({project}): {quoted(part, 220)} [session {sid[:8]}]{old}")
    return rows


def dream_rows(dreams):
    """The brief's dreams already offered here, each with what the person answered and, where they said why, their
    own words: a dream they said no to tells the dreamer what it got wrong (10-12 19:53: "To my knowledge the
    dreamer is overdreaming i cannot kill a session")."""
    if not dreams:
        return []
    said = lambda d: f": you said {quoted(d['said'], 200)}" if d.get('said') else ''
    return ['', "Dreams already offered here, and what the person answered (where they said why, their own words):"] + [
        f"- {short_date(d['at'])}: {quoted(d['line'], 200)} [{d['answer'] or 'no answer'}{said(d)}]" for d in dreams]


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


RETOLD = 'next at compact'  # when csr-engine tells the story again: after a compaction (and at the session's end)


def told_ago(seconds):
    """How old a story is, short: 'just told' in its first minute, then minutes, hours, days."""
    minutes = int(seconds // 60)
    if minutes < 1:
        return 'just told'
    if minutes < 120:
        return f'{minutes}m old'
    return f'{minutes // 60}h old' if minutes < 48 * 60 else f'{minutes // 1440}d old'


def story_rights(story, now):
    """The Story heading's right side, longest first: its revision, who told it, how old it is and when it is
    told again, 'rev 11 · Haiku · 82m old · next at compact'; a narrow pane takes a shorter form. The pane never
    asks a model for a fresher one: the age says how far the story may be behind."""
    if not story:
        return ['']
    rev, old = f"rev {story['rev']}", story.get('at') and told_ago(max(0, now - story['at']))
    forms = [(rev, story.get('by'), old, RETOLD), (rev, old, RETOLD), (rev, old), (old or rev,)]
    return [' · '.join(x for x in form if x) for form in forms]


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

# ---------------------------------------------------------------- where a live session is

def project_folder(cwd):
    """Claude Code's folder for a working directory: every character not A-Z, a-z, 0-9 becomes '-'."""
    return os.path.join(HOME, '.claude', 'projects', re.sub(r'[^A-Za-z0-9]', '-', cwd)) if cwd else None


def read_record(path, session_id):
    try:
        with open(path, encoding='utf-8') as handle:
            kept = json.load(handle)
        if kept.get('sessionId') != session_id or not kept.get('cwd'):
            return None
        if kept.get('pid') is not None:  # the record of a session that died is left behind: its pid must be alive
            try:
                os.kill(int(kept['pid']), 0)
            except PermissionError:
                pass  # alive, and not ours
            except (OSError, OverflowError):
                return None
        return dict(cwd=str(kept['cwd']), started=float(kept['startedAt']) / 1000, busy=kept.get('status') == 'busy')
    except (OSError, ValueError, TypeError, KeyError, AttributeError):
        return None


def session_record(session_id, folder=None):
    """dict(cwd, started, busy) from the <pid>.json Claude Code keeps for a live session (receipt: sessionId,
    cwd, startedAt in ms, status busy or idle); None when none matches or it cannot be read. The path found
    last time is tried first, then the folder is searched."""
    folder = folder or SESSIONS
    if not session_id:
        return None
    path = _RECORD.get(session_id)
    found = path and os.path.dirname(path) == folder and read_record(path, session_id)
    if found:
        return found
    for path in sorted(glob.glob(os.path.join(folder, '*.json'))):
        found = read_record(path, session_id)
        if found:
            _RECORD[session_id] = path
            return found
    _RECORD.pop(session_id, None)
    return None


INTERRUPTED = re.compile(r'\[Request interrupted by user')  # Claude Code's own marker, not words a person typed


class Session:
    """This session, read as it goes: the transcript from where it was left, CSR's rows fresh."""

    def __init__(self, session_id, path=None):
        self.id = session_id
        found = [path] if path else glob.glob(os.path.join(HOME, '.claude', 'projects', '*', f'{session_id}.jsonl'))
        self.path = found[0] if found else None
        self.record = None  # Claude Code's <pid>.json for this session while it lives: cwd, started, busy
        self.main = []  # (time, context) of each main-chain reply
        self._main_id = None  # the message the last of them came from: a reply written in parts is one
        self.model = None
        self.told = None  # what CSR injected at the newest SessionStart, as the transcript kept it
        self.previous = None  # the session before this one, as previous_facts: None until read, {} when none
        self.looked = 0.0  # when the transcript was last searched for
        self.offset = 0
        self.replies = {}  # message id -> (time, context tokens, output tokens)
        self.prompts, self.compacts, self.asked, self.questions = [], [], [], []
        self.compact_sizes = []  # (tokens before, context of the first request after, ms) of each compaction
        self.summary_sizes = []  # compact_boundary's postTokens of each: the summary the compaction wrote
        self.measuring = None  # the compaction whose first request after it is still to come
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

    def guess(self):
        """Where the transcript of a live session will be, from its record: the folder for its working directory."""
        return os.path.join(project_folder(self.record['cwd']), f'{self.id}.jsonl') if self.record else None

    def locate(self):
        """Find this session's transcript. Claude Code writes it at the first prompt, so a pane opened before
        that finds none: the live session's record names the folder, and the file is taken the moment it exists.
        Failing that, the folders are searched, at most once every ten seconds."""
        if self.path:
            return
        record = session_record(self.id)
        folder = record and project_folder(record['cwd'])
        guess = folder and os.path.join(folder, f'{self.id}.jsonl')
        if guess and os.path.exists(guess):
            self.path = guess
        elif time.time() - self.looked >= 10:
            self.looked = time.time()
            found = glob.glob(os.path.join(HOME, '.claude', 'projects', '*', f'{self.id}.jsonl'))
            self.path = found[0] if found else None

    def read_transcript(self):
        self.locate()
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
                if context and when and not entry.get('isSidechain'):
                    if message.get('id') and message['id'] == self._main_id:
                        self.main[-1] = (when, context)  # the same reply, written again with its later parts
                    else:
                        self.main.append((when, context))
                    self._main_id = message.get('id')
                    model = message.get('model')
                    self.model = model if isinstance(model, str) and model and not model.startswith('<') else self.model
                if self.measuring is not None and context and not entry.get('isSidechain'):
                    pre, _, ms = self.compact_sizes[self.measuring]
                    self.compact_sizes[self.measuring] = (pre, context, ms)
                    self.measuring = None
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
                if queued.get('type') == 'hook_success' and queued.get('hookEvent') == 'SessionStart' \
                        and re.search(r'csr-engine\S*\s+hook\s+session-start\s*$', str(queued.get('command') or '')) \
                        and isinstance(queued.get('content'), str) and queued['content'].strip():
                    # what CSR told Claude: the newest wins, a resume or a compaction injects again
                    self.told = dict(at=when, source=str(queued.get('hookName') or '').partition(':')[2] or 'startup',
                                     text=queued['content'])
            elif kind == 'system' and entry.get('subtype') == 'compact_boundary' and when:
                self.compacts.append(when)
                meta = entry.get('compactMetadata') if isinstance(entry.get('compactMetadata'), dict) else {}
                # postTokens is only the summary (median 19k here); the next request also carries the system
                # prompt, tools and what SessionStart re-injects (median 65k), so the floor is that request
                self.compact_sizes.append((meta.get('preTokens'), None, meta.get('durationMs')))
                self.summary_sizes.append(meta.get('postTokens'))
                self.measuring = len(self.compact_sizes) - 1
            elif kind == 'ai-title' and entry.get('aiTitle'):
                self.title = entry['aiTitle']
        del self.talk[:-200]

    def load(self):
        self.read_transcript()
        self.record = session_record(self.id) or (self.record and dict(self.record, busy=False))  # gone: not busy
        if self.carried is None and (self.path or self.record):  # once: the session before this one is done being written
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
                    compacts=sorted(self.compacts), compact_sizes=self.compact_sizes,
                    summary_sizes=self.summary_sizes, asked=sorted(self.asked), tools=self.tools, learned=learned,
                    summaries=self.summaries[-2:],  # the newest, for the compaction card's Learnt
                    lost=lost_found(self.summaries, self.typed, self.carried or (),
                                    project_of(self.path or self.guess())),
                    gave=gave, remembered=remembered, sessions=sessions, project=here[0][0] if here else None,
                    files=dict(self.files), edited=sorted(self.edited), shell=dict(self.shell), last=dict(self.last),
                    main=list(self.main), turn_end=self.turn_end, model=self.model, told=self.told,
                    last_session=self.previous, todos=(self.previous or {}).get('todos') or [],
                    busy=bool(self.record and self.record['busy']), cwd=(self.record or {}).get('cwd'),
                    transcript=bool(self.path))


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


_BLENDS = {}  # (a, b, t) -> blend: the strip, the chart and the tints ask for the same few each frame


def blend(a, b, t):
    """a toward b by t, in CIELAB: even steps to the eye, no muddy middle."""
    key = (tuple(a[:3]), tuple(b[:3]), t)
    if key not in _BLENDS:
        if len(_BLENDS) > 4096:
            _BLENDS.clear()
        _BLENDS[key] = from_lab(tuple(p + (q - p) * t for p, q in zip(to_lab(a), to_lab(b))))
    return _BLENDS[key]


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
SECTION['told'], GLYPH['told'] = 'recall', '↑'  # what CSR told Claude at the start
SECTION['last'], GLYPH['last'] = 'ctx', '§'  # the session before this one
SECTION['todos'], GLYPH['todos'] = 'sepia', '◆'  # its leftover todos
SECTION['compacted'], GLYPH['compacted'] = 'ctx', '§'  # CSR's own compaction: what it kept and learnt
DARK = (PIGMENT['payne'], PIGMENT['night'])  # the masthead's ground, each row a step darker in CIELAB from
# payne, which sits clear of a dark terminal's own ground (herdr's is 40) so the masthead keeps its edge, down to
# night; the cockpit's inks lie on top. The vertical tape runs on down the pane's edge in the last step.
ON_DARK = dict(text=PIGMENT['chalk'], label=blend(PIGMENT['chalk'], PIGMENT['payne'], 0.45), you=PIGMENT['chalk'],
               ctx=PIGMENT['indigo_lit'], learn=PIGMENT['viridian_lit'], recall=PIGMENT['viridian_lit'],
               turn=PIGMENT['vermilion_lit'], dream=PIGMENT['ochre_lit'], asked=PIGMENT['violet_lit'],
               rule=(255, 255, 255, 40))
# The cockpit's inks: a normal state is dim and only a deviation lights. Data is the one white; advisory (cyan),
# amber and red are the three degrees of attention.
SEV = dict(normal=ON_DARK['label'], data=ON_DARK['text'], advisory=PIGMENT['indigo_lit'],
           amber=PIGMENT['ochre_lit'], red=PIGMENT['vermilion_lit'])
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
    if grounds:  # RGB, drawn blending: on an RGBA canvas Pillow wrote each wash's alpha over the ground as is, and
        from PIL import Image, ImageDraw  # the terminal showed through under the line (the last session figure's bug)
        image = Image.new('RGB', image.size, tuple(grounds[0][:3]))
        draw = ImageDraw.Draw(image, 'RGBA')
    for i, ground in enumerate(grounds or ()):
        draw.rectangle([0, i * cell[1] * SS, image.width, (i + 1) * cell[1] * SS], fill=tuple(ground[:3]) + (255,))
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


_LAST_PNG = {}  # (id, mtime, size of the series, cols, rows, cell, ground) -> PNG bytes or None; the newest 8


def last_png(facts, cols, rows, cell, ground):
    """The last session in one figure, on `ground` (its row's tint): the context of every main-chain reply over
    active time (Clock), broken at each compaction, as an INK['ctx'] hairline over a faint INK['ctx'] wash; a
    dashed INK['quiet'] rule at the floor (or FLOOR_GUESS) drawn over the wash; an INK['faint'] hairline at each
    compaction from its pre size down to the foot; one INK['you'] tick per prompt under the foot; an INK['turn']
    dot at the peak. No text. None without Pillow, with fewer than 2 points, or on any exception."""
    if not HAS_PIL:
        return None
    try:
        series = facts.get('series') or []
        key = (facts.get('id'), facts.get('mtime'), len(series), tuple(series[0]) if series else None,
               tuple(series[-1]) if series else None, cols, rows, tuple(cell), tuple(ground))
        if key not in _LAST_PNG:
            try:
                _LAST_PNG[key] = draw_last(facts, cols, rows, cell, ground)
            except Exception:
                _LAST_PNG[key] = None
            while len(_LAST_PNG) > 8:
                _LAST_PNG.pop(next(iter(_LAST_PNG)))
        return _LAST_PNG[key]
    except (AttributeError, TypeError):
        return None


def draw_last(facts, cols, rows, cell, ground):
    from PIL import Image, ImageDraw
    series = [(t, c) for t, c in facts.get('series') or ()]
    if len(series) < 2:
        return None
    width, height = cols * cell[0] * SS, rows * cell[1] * SS
    image = Image.new('RGB', (width, height), tuple(ground[:3]))  # RGB: an RGBA one took the wash's alpha as is
    draw = ImageDraw.Draw(image, 'RGBA')  # blends: a wash over the ground, a rule over the wash
    prompts = list(facts.get('prompt_times') or [])
    clock = Clock([t for t, _ in series] + prompts, max([t for t, _ in series] + prompts))
    pad, foot, top = 3 * SS, height - 5 * SS, 3 * SS  # the prompts' ticks sit under the foot rule
    peak = max(c for _, c in series)
    floor = facts.get('floor') or FLOOR_GUESS
    scale = max(peak, floor) * 1.08
    x = lambda t: pad + clock.at(t) * (width - 2 * pad)
    y = lambda c: foot - (c / scale) * (foot - top)
    draw.line([(0, foot), (width, foot)], fill=INK['hair'], width=SS)
    for when, pre, _ in facts.get('compactions') or ():
        draw.line([(x(when), y(pre or peak)), (x(when), foot)], fill=INK['faint'], width=SS)
    cuts, runs, run, k = sorted(c[0] for c in facts.get('compactions') or ()), [], [], 0
    for t, c in series:  # one run from each compaction to the next
        while k < len(cuts) and t >= cuts[k]:
            if run:
                runs.append(run)
            run, k = [], k + 1
        run.append((x(t), y(c)))
    if run:
        runs.append(run)
    for run in runs:
        if len(run) > 1:
            draw.polygon([(run[0][0], foot)] + run + [(run[-1][0], foot)], fill=INK['ctx'][:3] + (28,))
    for run in runs:
        if len(run) > 1:
            draw.line(run, fill=INK['ctx'], width=max(SS, round(1.5 * SS)), joint='curve')
    fy = y(floor)
    for fx in range(0, width, 8 * SS):  # the floor: a dashed rule over the wash
        draw.line([(fx, fy), (fx + 4 * SS, fy)], fill=INK['quiet'], width=SS)
    for t in prompts:
        draw.line([(x(t), foot + 1.5 * SS), (x(t), height - SS)], fill=INK['you'], width=SS)
    when, tokens = max(series, key=lambda point: point[1])
    r = 2.5 * SS
    draw.ellipse([x(when) - r, y(tokens) - r, x(when) + r, y(tokens) + r], fill=INK['turn'])
    return shrink(image)


_PAYBACK_PNG = {}  # (its facts, cols, rows, cell, ground) -> PNG bytes or None; the newest 4
PAYBACK_ROWS = 5  # the card's picture: four rows of the line, one for the axis words
PAYBACK_FACTS = ('at', 'cost', 'saving', 'need', 'done', 'net', 'per')


def payback_png(facts, cols, rows, cell, ground):
    """A compaction's payback in one figure, for its card (draw_payback); None without Pillow, when it never pays
    back, or on any exception. Drawn once per new request."""
    if not HAS_PIL:
        return None
    try:
        key = (tuple(facts.get(name) for name in PAYBACK_FACTS), cols, rows, tuple(cell), tuple(ground))
        if key not in _PAYBACK_PNG:
            try:
                _PAYBACK_PNG[key] = draw_payback(facts, cols, rows, cell, ground)
            except Exception:
                _PAYBACK_PNG[key] = None
            while len(_PAYBACK_PNG) > 4:
                _PAYBACK_PNG.pop(next(iter(_PAYBACK_PNG)))
        return _PAYBACK_PNG[key]
    except (AttributeError, TypeError):
        return None


def label_font(size, bold=False):
    """The figure's type: the terminal's own Menlo where the Mac has it (its bold face when asked), else Pillow's
    own; None without either."""
    from PIL import ImageFont
    for path, index in (('/System/Library/Fonts/Menlo.ttc', 1 if bold else 0), ('/System/Library/Fonts/SFNSMono.ttf', 0)):
        try:
            return ImageFont.truetype(path, size, index=index)
        except OSError:
            continue
    try:
        return ImageFont.load_default(size)
    except (OSError, TypeError):
        return None


def dashed(draw, points, fill, width, on, off):
    """A polyline in dashes of `on` and gaps of `off`, the pattern carried across its corners."""
    left, ink_on = on, True
    for (x0, y0), (x1, y1) in zip(points, points[1:]):
        length = math.hypot(x1 - x0, y1 - y0)
        done = 0.0
        while done < length:
            step = min(left, length - done)
            a, b = done / length, (done + step) / length
            if ink_on:
                draw.line([(x0 + (x1 - x0) * a, y0 + (y1 - y0) * a), (x0 + (x1 - x0) * b, y0 + (y1 - y0) * b)],
                          fill=fill, width=width)
            done += step
            left -= step
            if left <= 0:
                ink_on = not ink_on
                left = on if ink_on else off


def draw_payback(facts, cols, rows, cell, ground):
    """A compaction's payback as one break-even line, on `ground` (its card's tint): across, the API calls after
    it; up, the net saving in input tokens at the full price, the zero line where it has paid back. The compaction
    drops the line by its one-time cost (INK['turn']); each call lifts it by its saving (INK['ctx'], to a dot at the
    calls so far, at facts['net'], the card's own number); the rest of the way is dashed (INK['quiet']) to a dot on
    the zero line at the call that pays it back, INK['learn'] once passed. A pale INK['learn'] zone over zero says
    "saving", a pale INK['quiet'] one under it "not yet paid back"; the last row says what the axis counts, with
    the session's calls a prompt (facts['per']). No key: each mark is named where it is (10-12: the area chart over
    raw tokens, its ghost line and its five-word key, read as jargon)."""
    from PIL import Image, ImageDraw
    cost, saving, need, done, net = (facts[k] for k in ('cost', 'saving', 'need', 'done', 'net'))
    if not need or saving <= 0 or cost <= 0:
        return None
    width, height = cols * cell[0] * SS, rows * cell[1] * SS
    image = Image.new('RGB', (width, height), tuple(ground[:3]))  # RGB, so the zones blend into the ground
    draw = ImageDraw.Draw(image, 'RGBA')
    size = round(cell[1] * SS * 0.5)
    font, bold = label_font(size), label_font(size, bold=True)
    paid = done >= need
    left, right = 3 * SS, width - 3 * SS
    top, foot = 2 * SS, height - round(cell[1] * SS * 0.85)  # the last row's words sit under foot
    xmax = done + max(1, done // 25) if paid else need + max(2, need // 4)  # paid: the end dot clear of the edge
    ymax = max(net * 1.3, cost * 0.42) if paid else cost * 0.42  # room over the end for its words
    ymin = -max(cost * 1.08, ymax * 0.3) if paid else -cost * 1.08  # paid: room under zero for the cost's words
    x = lambda n: left + (n / xmax) * (right - left)
    y = lambda v: top + (ymax - v) / (ymax - ymin) * (foot - top)
    zero = y(0)
    draw.rectangle([left, top, right, zero], fill=INK['learn'][:3] + (40,))
    draw.rectangle([left, zero, right, foot], fill=INK['quiet'][:3] + (30,))
    draw.line([(left, zero), (right, zero)], fill=INK['you'], width=SS)
    draw.line([(x(0), zero), (x(0), y(-cost))], fill=INK['turn'], width=3 * SS)  # the compaction's one-time cost
    if done:
        draw.line([(x(0), y(-cost)), (x(done), y(net))], fill=INK['ctx'], width=3 * SS)
    if not paid:
        dashed(draw, [(x(done), y(net)), (x(need), zero)], INK['quiet'][:3] + (200,), 2 * SS, 6 * SS, 5 * SS)

    def dot(cx, cy, ink, r=4.5 * SS):  # a dot with a ring of the ground, clear of the lines under it
        halo = r + 1.8 * SS
        draw.ellipse([cx - halo, cy - halo, cx + halo, cy + halo], fill=tuple(ground[:3]))
        draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=ink)
    dot(x(need), zero, INK['learn'] if paid else INK['quiet'])
    dot(x(done), y(net), INK['ctx'])
    if font is None:
        return shrink(image)
    gap = 4 * SS
    line_y = lambda px: y(-cost) + (px - x(0)) / max(1e-9, x(done) - x(0)) * (y(net) - y(-cost))  # the calls' line
    draw.text((left + 3 * gap, top + gap), 'saving', font=font, fill=INK['learn'], anchor='la')
    if paid:
        draw.text((x(need) - gap, zero - 2 * gap), 'paid back', font=bold, fill=INK['learn'], anchor='rd')
        said = f'+{tokens_said(net)}'
        end, wide = x(done) - 2 * gap, draw.textlength(said, font=bold)
        if line_y(end) - gap - size >= top:  # over the line, which climbs to the end from below left
            draw.text((end, line_y(end) - gap), said, font=bold, fill=INK['ctx'], anchor='rd')
        else:  # else under all of it
            draw.text((end, line_y(end - wide) + gap), said, font=bold, fill=INK['ctx'], anchor='ra')
        said = f'cost {short(cost)}, one time'
        if foot - zero >= size + 2 * SS and x(need) + 6 * gap + draw.textlength(said, font=bold) <= right:
            draw.text((x(need) + 6 * gap, (zero + foot) / 2), said, font=bold, fill=INK['turn'], anchor='lm')
    else:
        draw.text((right - gap, foot - gap), 'not yet paid back', font=font, fill=INK['quiet'], anchor='rd')
        draw.text((x(need), zero - 2 * gap), 'paid back', font=font, fill=INK['quiet'], anchor='md')
        draw.text((x(0) + 2 * gap, y(-cost * 0.55)), f'cost {short(cost)}', font=bold, fill=INK['turn'], anchor='lm')
        clear = x(need) - draw.textlength('paid back', font=font) / 2 - gap  # where 'paid back' begins
        if done and x(done) + 2.5 * gap + draw.textlength('now', font=bold) > clear:  # near it: under the line, left
            draw.text((x(done) - 2 * gap, line_y(x(done) - 2 * gap - draw.textlength('now', font=bold)) + gap), 'now',
                      font=bold, fill=INK['ctx'], anchor='ra')
        else:
            draw.text((x(done) + 2.5 * gap, y(net) - gap), 'now', font=bold, fill=INK['ctx'], anchor='ld')
    base = height - SS  # the axis: its counts, and what it counts in words a developer knows
    draw.text((x(0), base), '0', font=font, fill=INK['quiet'], anchor='ld')
    tick = draw.textlength(str(need), font=font)
    draw.text((x(need), base), str(need), font=font, fill=INK['quiet'], anchor='md')
    per = facts.get('per')
    words = ([f'API calls after compacting (~{int(per + 0.5)} a prompt)'] if per else []) + \
        ['API calls after compacting', 'API calls']
    rooms = [(x(0) + draw.textlength('0', font=font) + 3 * gap, x(need) - tick / 2 - gap)]  # before the call that pays
    if paid:
        end = draw.textlength(str(done), font=font)
        if x(done) - x(need) > tick + end:
            draw.text((x(done), base), str(done), font=font, fill=INK['quiet'], anchor='md')
        rooms.append((x(need) + tick / 2 + gap, x(done) - end - gap))  # and after it, the wider one first
        rooms.sort(key=lambda r: r[0] - r[1])
    for start, stop in rooms:
        said = next((w for w in words if draw.textlength(w, font=font) <= stop - start), None)
        if said:
            draw.text((start, base), said, font=font, fill=INK['quiet'], anchor='ld')
            break
    return shrink(image)


_CONTEXT_PNG = {}  # the i page's context figure: its key -> PNG bytes or None; the newest 2


def context_png(facts, cols, rows, cell, backs):
    """The i page's context in one figure (draw_context), drawn once per new request; None without Pillow,
    under two requests, or on any exception."""
    if not HAS_PIL:
        return None
    try:
        series = facts.get('series') or []
        key = (len(series), tuple(series[-1]) if series else None, tuple(facts.get('compacts') or ()),
               tuple(facts.get('csr_at') or ()), facts.get('floor'), cols, rows, tuple(cell), tuple(map(tuple, backs)))
        if key not in _CONTEXT_PNG:
            try:
                _CONTEXT_PNG[key] = draw_context(facts, cols, rows, cell, backs)
            except Exception:
                _CONTEXT_PNG[key] = None
            while len(_CONTEXT_PNG) > 2:
                _CONTEXT_PNG.pop(next(iter(_CONTEXT_PNG)))
        return _CONTEXT_PNG[key]
    except (AttributeError, TypeError):
        return None


def draw_context(facts, cols, rows, cell, backs):
    """A session's context over active time (facts['clock']) on the ramp, each row of the picture on its own
    ground (backs, top first): a chalk line of every request's context with a faint wash under it, broken at
    each compaction by a drop from the last request before it to the first after, in vermilion where CSR
    compacted and in the label ink where you did. The band CSR compacts in (BAND) a faint ochre lane, labelled
    at its right end; the floor a dashed rule; the peak named at its point. The bars this replaces (10-12) all
    read orange, and their compaction ticks sat apart from the drops: neither said where you compacted against
    the band."""
    from PIL import Image, ImageDraw
    series = sorted((t, c) for t, c in facts.get('series') or () if t and c)
    if len(series) < 2:
        return None
    clock, bounds, csr = facts['clock'], sorted(facts.get('compacts') or ()), set(facts.get('csr_at') or ())  # csr_at: the dev mod's copy renames csr in quotes
    width, height = cols * cell[0] * SS, rows * cell[1] * SS
    image = Image.new('RGB', (width, height), tuple(backs[0][:3]))
    draw = ImageDraw.Draw(image, 'RGBA')
    band_h = cell[1] * SS
    for k, back in enumerate(backs[:rows]):  # each row on its own step of the ramp
        draw.rectangle([0, k * band_h, width, (k + 1) * band_h], fill=tuple(back[:3]))
    pad, top, foot = 2 * SS, 3 * SS, height - 2 * SS
    peak_t, peak = max(series, key=lambda p: p[1])
    scale = max(peak, BAND[1]) * 1.08
    x = lambda t: pad + clock.at(t) * (width - 2 * pad)
    y = lambda c: foot - (c / scale) * (foot - top)
    font = label_font(round(cell[1] * SS * 0.5))
    label = ON_DARK['label']
    draw.rectangle([pad, y(BAND[1]), width - pad, y(BAND[0])], fill=PIGMENT['ochre_lit'] + (34,))  # the band
    floor = facts.get('floor')
    if floor:
        dashed(draw, [(pad, y(floor)), (width - pad, y(floor))], label[:3] + (150,), SS, 4 * SS, 4 * SS)
    runs, run, edges = [], [], iter(bounds + [float('inf')])
    edge = next(edges)
    drops = []
    for t, c in series:  # the line, broken at each compaction
        while t > edge:
            if run:
                runs.append(run)
                drops.append((edge, run[-1][1]))
            run, edge = [], next(edges)
        run.append((t, c))
    runs.append(run)
    for points in runs:
        if not points:
            continue
        xy = [(x(t), y(c)) for t, c in points]
        if len(xy) > 1:
            draw.polygon([(xy[0][0], foot)] + xy + [(xy[-1][0], foot)], fill=ON_DARK['text'][:3] + (26,))
            draw.line(xy, fill=ON_DARK['text'][:3] + (225,), width=round(1.5 * SS), joint='curve')
    after = {}
    for b in bounds:
        nxt = next(((t, c) for t, c in series if t > b), None)
        if nxt:
            after[b] = nxt[1]
    for b, before in drops:  # the drops: CSR's in vermilion, yours in the label ink
        if b not in after:
            continue
        ink = ON_DARK['turn'] if b in csr else label
        bx = x(b)
        draw.line([(bx, y(before)), (bx, y(after[b]))], fill=ink[:3] + (235,), width=round(1.5 * SS))
        r = 2.2 * SS
        draw.ellipse([bx - r, y(before) - r, bx + r, y(before) + r], fill=ink[:3])
    if font is not None:
        said, mid = f'{short(BAND[0])}–{short(BAND[1])}', (y(BAND[0]) + y(BAND[1])) / 2
        box = draw.textbbox((width - pad - SS, mid), said, font=font, anchor='rm')
        draw.rectangle([box[0] - 2 * SS, box[1] - SS, box[2] + SS, box[3] + SS], fill=tuple(backs[-1][:3]) + (200,))
        draw.text((width - pad - SS, mid), said, font=font, fill=PIGMENT['ochre_lit'], anchor='rm')  # over the line
        px, py = x(peak_t), y(peak)
        anchor = 'lb' if px < width * 0.6 else 'rb'
        draw.text((px + (3 * SS if anchor == 'lb' else -3 * SS), py + 2 * SS), f'peak {short(peak)}', font=font,
                  fill=ON_DARK['text'], anchor=anchor)
    return shrink(image)


def last_sparkline(facts, width):
    """The same series as one row of '▁▂▃▄▅▆▇█' (the most in each bin, carried forward); '' under 8 cells."""
    series = facts.get('series') or []
    if len(series) < 2 or width < 8:
        return ''
    first, last = series[0][0], series[-1][0]
    bins = [0] * width
    for t, c in series:
        i = min(width - 1, int((t - first) / max(1, last - first) * width))
        bins[i] = max(bins[i], c)
    peak, level, out = max(bins) or 1, 0, []
    for b in bins:
        level = b or level
        out.append('▁▂▃▄▅▆▇█'[min(7, int(level / peak * 7.999))] if level else ' ')
    return ''.join(out)


SYNC_ON, SYNC_OFF = '\x1b[?2026h', '\x1b[?2026l'  # a terminal that knows them shows the frame at once


def repaint(drawn, cols, rows, written, placed):
    """What turns the screen as drawn into the frame written: only the rows that differ, each cleared and
    written in place (no clearing the whole screen, which flashes), and the pictures sent again only when
    they changed or a row under one did. The first frame, or a new size, starts from a clear screen."""
    out = [SYNC_ON]
    if drawn is None or drawn[:2] != (cols, rows):
        before, shown = [None] * rows, []
        out.append('\x1b_Ga=d,d=A\x1b\\\x1b[H\x1b[2J')
    else:
        before, shown = drawn[2], drawn[3]
    changed = [i for i, line in enumerate(written) if before[i] != line]
    under = {r + k for r, _, _, h, _ in shown + placed for k in range(h)}
    again = placed != shown or any(i in under for i in changed)
    if again and shown:
        out.append('\x1b_Ga=d,d=A\x1b\\')
    for i in changed:
        out.append(f'\x1b[{i + 1};1H\x1b[0m\x1b[2K' + written[i])
    if again:
        out += [kitty(png, r, c, w, h) for r, c, w, h, png in placed]
    out += [f'\x1b[{rows};{cols}H', SYNC_OFF]
    return ''.join(out)


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
    if name in ('touching', 'story', 'last', 'compacted'):  # Claude's open files, the session's story, the last session: a sheet on its folder
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
    elif name in ('rules', 'kept', 'todos'):  # a rule book: its cover, its spine banded, its pages, a ribbon
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



EMBLEM_FONT = {'C': ('.##', '#..', '#..', '#..', '.##'), 'S': ('.##', '#..', '.#.', '..#', '##.'),
               'R': ('##.', '#.#', '##.', '#.#', '#.#')}  # 3 x 5 voxel letters, top row first


def csr_emblem():
    """CSR's emblem in pixel art, seen isometric like the section icons: the voxel letters C S R in indigo, ochre
    and viridian standing on a dark mirror slab of payne, which reflects them, faint, on its top. Each letter is
    one part outlined in a dark of its own ink; the faces that carry a letter take its full ink, the tops only a
    little lighter, so the strokes read at the size of an icon. Every voxel edge is three pixels: 2:1 stairs."""
    from PIL import Image, ImageChops, ImageDraw
    size, step = (110, 96), 3
    image = Image.new('RGBA', size, (0, 0, 0, 0))
    at = lambda x, y, z: (round(40 + (x - y) * step), round(30 + (x + y) * step / 2 - z * step))
    white, black = (255, 255, 255), (0, 0, 0)
    inks = dict(C=PIGMENT['indigo_lit'], S=PIGMENT['ochre_lit'], R=PIGMENT['viridian_lit'])
    slab = PIGMENT['payne']

    def cube(draw, x, y, z, ink, dx=1, dy=1, dz=1, lit=(0.1, 0.25, 0.5)):
        poly = lambda pts, fill: draw.polygon([at(*p) for p in pts], fill=fill, outline=fill)
        poly([(x, y + dy, z), (x + dx, y + dy, z), (x + dx, y + dy, z + dz), (x, y + dy, z + dz)], mix(ink, white, lit[0]))
        poly([(x + dx, y, z), (x + dx, y + dy, z), (x + dx, y + dy, z + dz), (x + dx, y, z + dz)], mix(ink, black, lit[1]))
        poly([(x, y, z + dz), (x + dx, y, z + dz), (x + dx, y + dy, z + dz), (x, y + dy, z + dz)], mix(ink, white, lit[2]))

    def part(ink, paint):
        layer = Image.new('RGBA', size, (0, 0, 0, 0))
        paint(ImageDraw.Draw(layer))
        alpha = layer.getchannel('A')
        grown = alpha
        for dx, dy in ((1, 0), (-1, 0), (0, 1), (0, -1)):
            grown = ImageChops.lighter(grown, ImageChops.offset(alpha, dx, dy))
        image.paste(mix(ink, black, 0.55), (0, 0) + size, ImageChops.subtract(grown, alpha))
        image.alpha_composite(layer)

    voxels = {ch: [(5 * k + xi, zi) for zi, row in enumerate(reversed(EMBLEM_FONT[ch])) for xi, c in enumerate(row) if c == '#']
              for k, ch in enumerate('CSR')}
    part(slab, lambda d: cube(d, -2, -2, -2, slab, dx=17, dy=8, dz=2))
    mirror = Image.new('L', size, 0)  # the slab's top: the reflection shows only on it
    ImageDraw.Draw(mirror).polygon([at(-2, -2, 0), at(15, -2, 0), at(15, 6, 0), at(-2, 6, 0)], fill=255)
    faint = Image.new('RGBA', size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(faint)
    for ch, cells in voxels.items():  # each letter's face mirrored in the top: (x, y, -z) falls on it at (x+z, y+z, 0)
        for x, zi in cells:
            z = -zi - 1
            draw.polygon([at(x, 1, z), at(x + 1, 1, z), at(x + 1, 1, z + 1), at(x, 1, z + 1)],
                         fill=mix(inks[ch], slab, 0.62), outline=mix(inks[ch], slab, 0.62))
    image.paste(faint, (0, 0), ImageChops.multiply(faint.getchannel('A'), mirror))
    for ch, cells in voxels.items():  # far to near, bottom to top
        ordered = sorted(cells, key=lambda c: (c[0], c[1]))
        part(inks[ch], lambda d, ordered=ordered, ink=inks[ch]: [cube(d, x, 0, z, ink, lit=(0, 0.45, 0.22))
                                                                 for x, z in ordered])
    return image.crop(image.getbbox())


_EMBLEMS = {}


def emblem_png(cell, backs):
    """The emblem in the i page's 5 x 2 slot, its two rows' grounds behind it, blown up by whole pixels, with the
    icons' hard shadow; shrunk smooth only where a slot is smaller than the emblem itself."""
    key = (cell, backs)
    if key in _EMBLEMS:
        return _EMBLEMS[key]
    from PIL import Image
    w, h = ICON[0] * cell[0], ICON[1] * cell[1]
    image = Image.new('RGBA', (w, h), backs[0] + (255,))
    image.paste(backs[1] + (255,), (0, cell[1], w, h))
    mark = csr_emblem()
    k = min(w // (mark.width + SHADOW[0]), h // (mark.height + SHADOW[1]))  # its shadow inside the slot
    if k >= 1:
        mark = mark.resize((mark.width * k, mark.height * k), Image.NEAREST)
    else:  # a slot smaller than the emblem: shrink it to fit, the one place it is not pixel-exact
        k = 1
        scale = min(w / (mark.width + SHADOW[0]), h / (mark.height + SHADOW[1]))
        mark = mark.resize((max(1, int(mark.width * scale)), max(1, int(mark.height * scale))), Image.LANCZOS)
    x, y = (w - mark.width - SHADOW[0] * k) // 2, (h - mark.height - SHADOW[1] * k) // 2
    image.alpha_composite(cast_shadow(mark), (x + SHADOW[0] * k, y + SHADOW[1] * k))
    image.alpha_composite(mark, (x, y))
    out = io.BytesIO()
    image.save(out, 'PNG')
    _EMBLEMS[key] = out.getvalue()
    return _EMBLEMS[key]



LOGO = (4, 1)  # the masthead's first row: CSR's mark where it says CTX, four cells by one row
MASTHEAD_CLICK = 2  # the masthead's first rows: a click anywhere on them opens the i page, which says them in full


def csr_logo(w, h):
    """CSR's mark for one row: the emblem's voxel letters C S R face on, in indigo, ochre and viridian, each
    voxel's top and right side receding at the icons' 2:1 stair (lit from the upper left), drawn at the size of
    the slot itself, never blown up from a smaller drawing. A slab's tilt would slope the word out of one row."""
    from PIL import Image, ImageDraw
    image = Image.new('RGBA', (w, h), (0, 0, 0, 0))
    v = next((v for v in range(max(1, h), 0, -1)
              if 11 * v + 2 * max(1, v // 4) <= w - 2 and 5 * v + max(1, v // 4) <= h - 2), 1)
    t = max(1, v // 4)
    x0, y0 = (w - 11 * v - 2 * t) // 2, (h - 5 * v - t) // 2 + t
    inks = dict(C=PIGMENT['indigo_lit'], S=PIGMENT['ochre_lit'], R=PIGMENT['viridian_lit'])
    cells = [(x0 + (4 * k + cx) * v, y0 + ry * v, inks[ch]) for k, ch in enumerate('CSR')
             for ry, row in enumerate(EMBLEM_FONT[ch]) for cx, c in enumerate(row) if c == '#']
    draw = ImageDraw.Draw(image)
    for x, y, ink in cells:  # the sides first: every voxel's face then covers what is behind it
        draw.polygon([(x, y), (x + 2 * t, y - t), (x + v + 2 * t, y - t), (x + v, y)], fill=mix(ink, (255, 255, 255), 0.35))
        draw.polygon([(x + v, y), (x + v + 2 * t, y - t), (x + v + 2 * t, y + v - t), (x + v, y + v)],
                     fill=mix(ink, (0, 0, 0), 0.4))
    for x, y, ink in cells:
        draw.rectangle([x, y, x + v - 1, y + v - 1], fill=ink[:3] + (255,))
    return image


_LOGOS = {}


def logo_png(cell, ground):
    """The mark in the masthead's LOGO slot on the row's own ground, drawn once per cell size and ground."""
    key = (cell, ground)
    if key not in _LOGOS:
        from PIL import Image
        w, h = LOGO[0] * cell[0], LOGO[1] * cell[1]
        image = Image.new('RGBA', (w, h), tuple(ground[:3]) + (255,))
        image.alpha_composite(csr_logo(w, h))
        out = io.BytesIO()
        image.save(out, 'PNG')
        _LOGOS[key] = out.getvalue()
    return _LOGOS[key]


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


def on_dark(line, ground):
    """A masthead row on its dark ground, run to the pane's edge, light type laid again after each reset."""
    back, fg = '\x1b[48;2;%d;%d;%dm' % ground, '\x1b[38;2;%d;%d;%dm' % ON_DARK['text']
    return back + fg + line.replace(OFF, OFF + back + fg) + '\x1b[K' + OFF


def tinted(line, back):
    """The line on a background that runs to the pane's edge, laid again after each reset in it."""
    return back + line.replace(OFF, OFF + back) + '\x1b[K' + OFF


def sgr(name):
    return '\x1b[38;2;%d;%d;%dm' % SEV[name][:3]


def slot(text, name):
    """One slot of the cockpit: its text in the ink of its severity."""
    return sgr(name) + text + OFF


EIGHTHS = ' ▁▂▃▄▅▆▇█'
TEAL = (64, 190, 176)  # the heat ramp's middle stop, at CTX_AMBER
_HEAT = {}  # thousands of tokens -> its heat, worked out once


def heat(tokens):
    """The context's colour: blue to teal under CTX_AMBER, teal to amber up to CTX_HOT, amber to red up to
    CTX_RED, each step even to the eye (CIELAB). A value past the scale is red; one that is no number, blue."""
    v = tokens if isinstance(tokens, (int, float)) and not isinstance(tokens, bool) and tokens == tokens else 0
    hot, red = max(CTX_HOT, CTX_AMBER), max(CTX_RED, CTX_HOT, CTX_AMBER)
    key = int(max(0, min(v, red)) // 1000)
    if key not in _HEAT:
        v = key * 1000
        if v <= CTX_AMBER:
            _HEAT[key] = blend(PIGMENT['indigo_lit'], TEAL, v / CTX_AMBER)
        elif v <= hot:
            _HEAT[key] = blend(TEAL, PIGMENT['ochre_lit'], (v - CTX_AMBER) / (hot - CTX_AMBER))
        else:
            _HEAT[key] = blend(PIGMENT['ochre_lit'], PIGMENT['vermilion_lit'], (v - hot) / (red - hot))
    return _HEAT[key]


def fg_of(rgb):
    return '\x1b[38;2;%d;%d;%dm' % tuple(rgb[:3])


def bg_of(rgb):
    return '\x1b[48;2;%d;%d;%dm' % tuple(rgb[:3])


SPARK_MIN, SPARK_MAX = 6, 24  # the sparkline's cells: fewer is no line, more is not read


def spark(values, n):
    """The newest n turn-end contexts as n cells of eighths over 0..CTX_RED, each in the heat of its value,
    newest at the right; blank cells where there are fewer turns, never a zero."""
    vals = [v for v in values or () if isinstance(v, (int, float)) and not isinstance(v, bool) and v == v and v > 0][-n:] if n > 0 else []
    cells = [fg_of(heat(v)) + EIGHTHS[max(1, min(8, round(min(1.0, v / CTX_RED) * 8)))] + OFF for v in vals]
    return ' ' * (n - len(vals)) + ''.join(cells)


def ctx_row(g, width, logo=False):
    """Row 1: CTX (a blank LOGO slot where CSR's mark is drawn over it), a sparkline of the last turns' context, the context against its window, the growth per turn
    and the compaction payback (the timer while one runs). Too wide, the trend goes first, then the sparkline,
    then the timer's range and bar, then the window."""
    ctx, trend = g['ctx'], g.get('trend')
    tokens, size = ctx['tokens'], ctx['size']
    arrow = f"{'▲' if trend > 0 else '▼'}{short(abs(trend))}/t" if trend is not None and abs(trend) >= 1000 else ''
    values = ['--'] if tokens is None else [f'{short(tokens)}/{short(size)}', short(tokens)] if size else [short(tokens)]
    info = g.get('compacting')
    pays = [compacting_slot(info), compacting_slot(info, True, False), compacting_slot(info, False)] if info \
        else [(form, g['payback']['sev']) for form in g['payback'].get('forms') or [g['payback']['text']]]
    turns = g.get('turns') or []
    tries = [(values[0], arrow, pays[0], True), (values[0], '', pays[0], True), (values[0], '', pays[0], False)]
    tries += [(values[0], '', pay, want) for pay in pays[1:] for want in (True, False)]
    tries += [(values[-1], '', pay, False) for pay in pays]
    ink_ = 'normal' if tokens is None else 'data' if ctx['sev'] == 'normal' else ctx['sev']
    label = ' ' * LOGO[0] if logo else 'CTX '  # the mark's cells, or the word, the same width: nothing moves
    for value, tail, (text, sev), want in tries:
        fixed = 5 + len(value) + (1 + len(tail) if tail else 0) + (2 + len(text) if text else 0)
        n = min(SPARK_MAX, width - fixed - 1) if want and turns else 0
        if (want and turns and n < SPARK_MIN) or fixed + (n + 1 if n else 0) > width:
            continue
        return ' ' + slot(label, 'normal') + (spark(turns, n) + ' ' if n else '') + slot(value, ink_) \
            + (' ' + slot(tail, 'normal') if tail else '') + ('  ' + slot(text, sev) if text else '')
    return ' ' + slot(label, 'normal') + slot(fit(values[-1], max(1, width - 5)), ink_)


AHEAD = 0.05  # a plan window this far past its pace (used against the share of it gone) is spending ahead


def ahead_of_pace(q):
    """True when the window's used share runs more than AHEAD past its pace: spending faster than time passes.
    False under pace, or when its use or its pace is unknown."""
    return q.get('used') is not None and q.get('pace') is not None and q['used'] / 100 > q['pace'] + AHEAD


def paced(q):
    """A plan window's slot text with » after its percentage when it is ahead of pace: '7D 84%» ↻Sun 07:00'."""
    if not ahead_of_pace(q):
        return q['text']
    head, cut, rest = q['text'].partition(' ↻')
    return head + '»' + cut + rest


def status_row(g, width):
    """Row 2, numbers only: the plan's windows, riskier first, each with » when it spends ahead of pace, the
    cache's minutes, STS at the right when CSR runs without something. Too wide, the cache's word goes (◷),
    then the second window, then the first one's reset time, then the cache; then the first window's code
    goes before its percentage, and the » last of all. STS never goes."""
    quotas, sts, cache = g['quotas'], g.get('sts') or {}, g['cache']
    mark = 'STS' if sts.get('sev') else ''
    reserve = len(mark) + 1 if mark else 0
    full = [(paced(q), q['sev']) for q in quotas]
    head = full[0][0].split(' ↻')[0]
    first, bare = [(head, quotas[0]['sev'])], head.split(' ', 1)[-1]  # '7D 84%»', then '84%»'
    word, glyph = (cache['text'], cache['sev']), ('◷' + cache['text'][len('CACHE '):], cache['sev'])
    tries = [full + [word], full + [glyph], full[:1] + [glyph], first + [glyph], full[:1], first, [(bare, first[0][1])]]
    tail = '»' if bare.endswith('»') else ''
    pieces = next((p for p in tries if 1 + len('  '.join(t for t, _ in p)) + reserve <= width),
                  [(fit(bare[:len(bare) - len(tail)], max(1, width - 1 - reserve - len(tail))) + tail, first[0][1])])
    line = ' ' + '  '.join(slot(t, sev) for t, sev in pieces)
    if not mark:
        return line
    used = 1 + len('  '.join(t for t, _ in pieces))
    return line + ' ' * max(1, width - used - len(mark)) + slot(mark, sts['sev'])


def cockpit(g, width, phone=False, logo=False):
    """The masthead: 2 strings, each ' ' + slots, no wider than width, each slot in the ink of its severity
    (SEV): the context with its sparkline and payback, then the plan's windows (» ahead of pace), the cache and
    STS. A phone draws the same two rows, the sparkline going first."""
    return [ctx_row(g, width, logo), status_row(g, width)]


def stack_rows(g):
    """Column mode's masthead, one value a row, each [(text, sev)] in the ink of its severity, longest form
    first: CTX, the trend, the payback, each plan window, the cache, STS."""
    ctx, trend, cache = g['ctx'], g.get('trend'), g['cache']
    value = '--' if ctx['tokens'] is None else short(ctx['tokens'])
    rows = [[(f'CTX {value}', 'normal' if ctx['tokens'] is None else 'data' if ctx['sev'] == 'normal' else ctx['sev']),
             (value, 'data' if ctx['sev'] == 'normal' else ctx['sev'])]]
    if trend is not None and abs(trend) >= 1000:
        rows.append([(f"{'▲' if trend > 0 else '▼'}{short(abs(trend))}", 'normal')])
    text, sev = (g['payback']['text'], g['payback']['sev']) if not g.get('compacting') else \
        compacting_slot(g['compacting'], bar=False)  # ♻ m:ss or ♻A m:ss, amber past the usual longest
    if text:  # none under PAYBACK_FROM; in words, the prompts then the calls (never 14r or 2t)
        forms = [text, text.replace(' ', '')] if g.get('compacting') else g['payback'].get('forms') or [text]
        rows.append([(form, sev) for form in forms])
    for q in g['quotas']:
        pct = q['text'].split(' ↻')[0]
        rows.append([(pct, q['sev']), (pct.replace(' ', ''), q['sev']), (pct.split(' ')[-1], q['sev'])])
    minutes = cache['text'][len('CACHE '):]
    rows.append([('◷' + minutes, cache['sev']), ('◷' + minutes.split(' ')[0], cache['sev'])])
    if (g.get('sts') or {}).get('sev'):
        rows.append([('STS', g['sts']['sev'])])
    return rows


STRIP = ('ctx', '7d', '5h', 'cache')  # column mode's gauge strip, left to right; a narrow pane drops them from the
# right. Only column mode has one: there the strip is the page. Beside a page, row 2 says the windows (» ahead of pace)
STRIP_HEAD = {'ctx': 'C', '7d': '7', '5h': '5', 'cache': '◷'}  # each tape's one-character header, on the top row
STRIP_ROWS = 6  # the fewest rows that hold a strip
COLUMN_UNDER = 24  # a pane narrower than this is in column mode: the strip is its page, the values stack beside it
COLUMN_TEXT = 5  # column mode keeps at least this many columns of text left of the strip
RAIL = '│'  # an empty tape cell: a thin line, so no fill reads as track
BUG = PIGMENT['chalk']  # the floor's and the pace's bug: a bright bar across the tape at its level
QUOTA_INK = dict(normal=PIGMENT['indigo_lit'], advisory=PIGMENT['indigo_lit'], amber=PIGMENT['ochre_lit'],
                 red=PIGMENT['vermilion_lit'])
WARM = PIGMENT['viridian_lit']  # the cache's tape while it stays warm past CACHE_LOW


def strip_tapes(cols, rows, column=False):
    """How many of STRIP's tapes the pane has room for: none beside a page, where the text takes the width; in
    column mode, where the strip is the page, all four while COLUMN_TEXT columns of text are left."""
    if not column or rows < STRIP_ROWS:
        return 0
    return max(0, min(len(STRIP), cols - 1 - COLUMN_TEXT))


def strip_width(tapes):
    """The columns the strip takes at the right edge: a margin, then a column a tape."""
    return 1 + tapes if tapes else 0


def fill_levels(share, height):
    """The eighths filled in each of height cells, top first, for a share of the scale; a share that is no
    number fills none, one past the scale fills all."""
    if not isinstance(share, (int, float)) or isinstance(share, bool) or share != share:
        return [0] * height
    level = round(max(0.0, min(1.0, share)) * height * 8)
    return [max(0, min(8, level - 8 * (height - 1 - row))) for row in range(height)]


def level_row(share, height):
    """The cell, top first, that a share of the scale falls in."""
    share = share if isinstance(share, (int, float)) and not isinstance(share, bool) and share == share else 0.0
    return height - 1 - min(height - 1, int(max(0.0, min(1.0, share)) * height))


def bug_cell(fill, ink):
    """A bug across a tape: a bright bar, on the fill when the cell is at least half full."""
    return '━', BUG, ink if fill >= 4 else None


def ctx_tape(tokens, floor, height):
    """The context's tape, top first: (glyph, ink, back) a cell, ink 'rail' for an empty cell. It fills bottom-up
    in eighths over 0..CTX_RED, each cell in the heat of its own height, so the colour climbs with the context.
    The floor's bug wins its cell; above the fill, CTX_AMBER and CTX_HOT tick the rail in their inks. Tokens
    unknown: no fill, never a stale one."""
    known = isinstance(tokens, (int, float)) and not isinstance(tokens, bool) and tokens == tokens and tokens > 0
    levels = fill_levels(tokens / CTX_RED if known else None, height)
    ticks = {level_row(CTX_AMBER / CTX_RED, height): SEV['amber'], level_row(CTX_HOT / CTX_RED, height): SEV['red']}
    floor_share = floor / CTX_RED if isinstance(floor, (int, float)) and not isinstance(floor, bool) and floor == floor else None
    bug = level_row(floor_share, height) if floor_share is not None else None
    out = []
    for row in range(height):
        ink = heat((height - 1 - row + 0.5) / height * CTX_RED)
        fill = levels[row]
        if row == bug:
            out.append(bug_cell(fill, ink))
        elif fill:
            out.append((EIGHTHS[fill], ink, None))
        elif row in ticks:
            out.append(('─', ticks[row], None))
        else:
            out.append((RAIL, 'rail', None))
    return out


def quota_tape(q, height):
    """A plan window's tape, top first: filled to the share used in its severity's ink, with the pace's bug at
    the share of the window gone. A window never reported, or rolled over, is the bare rail."""
    used = (q or {}).get('used')
    if used is None:
        return [(RAIL, 'rail', None)] * height
    ink = QUOTA_INK.get(q.get('sev'), QUOTA_INK['normal'])
    levels = fill_levels(used / 100, height)
    bug = level_row(q['pace'], height) if q.get('pace') is not None else None
    return [bug_cell(fill, ink) if row == bug else (EIGHTHS[fill], ink, None) if fill else (RAIL, 'rail', None)
            for row, fill in enumerate(levels)]


def cache_tape(cache, height):
    """The cache's tape, top first: the share of its lifetime still warm, so it drains downward as the clock
    runs; amber under CACHE_LOW. Cold, or its clock unknown, it is the bare rail."""
    left, ttl = (cache or {}).get('left'), (cache or {}).get('ttl')
    if (cache or {}).get('cold') or not left or not ttl:
        return [(RAIL, 'rail', None)] * height
    ink = WARM if left >= CACHE_LOW else PIGMENT['ochre_lit']
    return [(EIGHTHS[fill], ink, None) if fill else (RAIL, 'rail', None) for fill in fill_levels(left / ttl, height)]


def gauge_strip(g, tapes, height):
    """The strip's first tapes of STRIP as [(kind, cells)], each height cells, top first."""
    quotas = {q['code']: q for q in (g or {}).get('quotas') or ()}
    made = dict(ctx=lambda: ctx_tape(g['ctx']['tokens'], g['floor']['tokens'], height),
                **{'7d': lambda: quota_tape(quotas.get('7D'), height), '5h': lambda: quota_tape(quotas.get('5H'), height)},
                cache=lambda: cache_tape(g['cache'], height))
    return [(kind, made[kind]()) for kind in STRIP[:tapes]] if g and height > 0 else []


def cell_text(cell, ground):
    """One tape or chart cell as escapes on its ground: the rail a faint line in the ground's own hue."""
    glyph, ink, back = cell
    if ink == 'rail':
        ink = blend(ground[:3], ON_DARK['text'], 0.24)
    return bg_of(back or ground) + (fg_of(ink) if ink else '') + glyph


def strip_rows(columns, cols, grounds=()):
    """The strip as escapes per screen row, written at the pane's last columns by absolute position, so no tint
    or wide glyph in a row can shift it: the margin, then each tape's header on the top row and its cells below.
    A row beside the masthead takes that row's ground; below, the masthead's last step runs on down the edge."""
    if not columns:
        return []
    start = cols - len(columns)  # 1-based: the margin's column
    out = []
    for row in range(len(columns[0][1]) + 1):
        ground = grounds[row][:3] if row < len(grounds) else DARK[1]
        if row == 0:
            body = sgr('normal') + ''.join(STRIP_HEAD[kind] for kind, _ in columns)
        else:
            body = ''.join(cell_text(cells[row - 1], ground) for _, cells in columns)
        out.append(f'\x1b[{row + 1};{start}H' + bg_of(ground) + ' ' + body + OFF)
    return out


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


CONTROL = re.compile(r'[\x00-\x08\x0b-\x1f\x7f-\x9f]')  # C0 and C1 controls: ESC, Ctrl-C, CSI as one byte...
SGR = re.compile(r'\x1b\[[0-9;:]*m|\x1b\[K')  # ...where a colour, and erase-to-end (a tinted row), are the
# escapes the pane writes inside a line


def one_line(text, most=700):
    """Text from a transcript or the DB on one line, no control left in it: what is drawn or typed into
    Claude's prompt can never clear the screen, set the clipboard or press a key."""
    return fit(' '.join(CONTROL.sub('', str(text)).split()), most)


def safe_line(line):
    """A drawn line with every control taken out but the pane's own colours: transcript text inside it
    cannot move the cursor, clear the screen or reach the clipboard (OSC 52)."""
    parts, colours = SGR.split(line), SGR.findall(line)
    out = [CONTROL.sub('', parts[0])]
    for colour, part in zip(colours, parts[1:]):
        out += [colour, CONTROL.sub('', part)]
    return ''.join(out)


def send_text(agent, text):
    """Words into Claude's prompt, unsent: one line, no controls, so nothing in them submits or presses a key."""
    return bool(agent) and herdr('pane', 'send-text', agent, one_line(text, 4000)) is not None


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


def opened(rows, name, detail, width, ui, key, indent=None, whole=False):
    """A row a click opened: its whole text, what more CSR knows (four rows of it, each in two; whole: all of
    it, never cut), and what a second click does."""
    sent = ui.get('sent')
    if sent and sent[0] == key:
        hint = "✓ now in Claude's prompt" if sent[1] else "✗ couldn't reach Claude's pane"
    else:
        hint = "↳ click again → Claude's prompt"
    indent = INDENT if indent is None else indent
    room = width - indent
    more = [' ' * indent + DIM + line + OFF for text in detail for line in wrap(text, room, 99 if whole else 2)]
    more = more if whole else more[:4]
    last = ' ' * indent + ink(SECTION[name]) + fit(hint, room) + OFF
    return card_edge(rows + more + [last], name, indent)


STAR = ink('asked') + '☾' + OFF  # CSR's mark, as in the band above Claude's prompt
MARK = {'changed': '\x1b[33m~', 'removed': '\x1b[31m-', 'came back': '\x1b[32m+'}
ANSWER_MARK = dict(helpful='\x1b[32m✓', ask_claude='\x1b[36m→', thought_of_it='\x1b[2m=', not_relevant='\x1b[31m✗',
                   got_it='\x1b[35m·', dismiss='\x1b[2m·', ignored='\x1b[2m·', turn_off='\x1b[2m○',
                   expired='\x1b[2m·', sent='\x1b[36m→', rejected='\x1b[31m✗')
ANSWER_WORD = dict(helpful='helpful', ask_claude='asked Claude', thought_of_it="you'd thought of it",
                   not_relevant='not relevant', got_it='read', dismiss='dismissed', ignored='passed',
                   turn_off='turned dreams off', expired='folded at a compaction', sent='sent to Claude',
                   rejected='you said no')


def fit(text, width):
    return text if len(text) <= width else text[:max(1, width - 1)] + '…'


def latest(times):
    """The newest of some times, those missing left out; None when there are none."""
    return max((t for t in times if t), default=None)


def counted(n, word):
    """'3 rules', '1 rule'; '' for none."""
    return f"{n} {word}{'' if n == 1 or word.endswith('ed') else 's'}" if n else ''


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


def heading(name, title, right, width, gutter=GUTTER, life=None):
    """A section's head: its icon's place (its mark where the terminal draws no pictures), its title in
    its ink, a count at the right. A narrower gutter (a phone's) has no icon column: the title starts there.
    right may be a list of forms, longest first: a narrow pane says the shorter one. life, a notice's share
    of its time still to run, is one tape cell at the row's end, draining as the notice goes."""
    lead = ' ' * gutter if gutter < GUTTER else ' ' + ('' if PIXELS else GLYPH[name]).ljust(GUTTER - 2) + ' '
    tape = '' if life is None else ' ' + life_cell(life)
    room = width - gutter - len(tape)
    if not isinstance(right, str):  # its forms, longest first: the first that fits is said
        right = next((form for form in right if room - len(title) - len(form) - 1 >= 1), '')
    gap = room - len(title) - len(right) - 1
    if gap < 1:
        right, gap = '', room - len(title) - 1
    voice = ink(SECTION[name]) if name == 'dreams' else ink('quiet')  # one accent on the page: What if…
    return lead + voice + BOLD + fit(title, room - 1) + OFF + ' ' * max(1, gap) + ink('quiet') + right + tape + OFF


LANE = 9  # the lanes' labels, then their picture


def lanes_legend(s, width, dark=False):
    hue = lambda name: '\x1b[38;2;%d;%d;%dm' % (ON_DARK if dark else INK)[name][:3]
    counts = (len(s['learned']['chunks']), handoffs(s['gave']['recalls']) + len(s['gave']['carried']), len(s['asked']))
    parts = [(hue('learn') + '▮', f'{short(counts[0])} learned'),
             (hue('recall') + '↑', f'{counts[1]} handed to Claude'),
             (hue('asked') + '●', f'{counts[2]} asked')]
    parts = [part for part, n in zip(parts, counts) if n]  # a zero would measure the format, not the session
    if not parts:
        return ''
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


# What CSR or a side agent writes into the prompt box for the person to send, read by the grammar that writes it:
# forward grammar v2, the same in hooks/dreamer.ts (stripForwards) and
# csr-engine (src/extraction/provenance.rs, strip_csr_forwards). v2 says `quoted` in its header, and every “ and ”
# inside a question, reasoning or body is a straight " (straight), so the first ” after an opening “ closes it:
#  - the pane's dream (dream_send), one line: `<attribution>, quoted: “<question>” Its reasoning: “<reasoning>”`;
#  - the compaction card's draft (compact_section), one line: `CSR's compaction card (<stage>), quoted: “<body>”`;
#  - the session story's, one line: `CSR's story of this session, rev <n>, quoted: “<body>”`;
#  - the band's dream (dreamer.ts askClaude): a `“<question>”` ending its paragraph, a blank line, the attribution
#    paragraph `<attribution>, quoted. Its reasoning:`, a blank line, the reasoning as quoted (>) lines.
# A one-line forward is a span of its line: your words before and after it stay (a band hotkey digit or a '>' alone
# before it goes with it); a band question is the span from the last “ of its line, your words before it stay.
# Sent before v2 (header then ':'), a one-line forward is never searched for a closing quote (astra r3 #5 #6): it ends
# at one_line's cut (… as its 700th, or the card's 1,200th, character), else at the end of its line. A v1 band is read
# by line: its attribution line, the nearest line above it that asks (cut whole), the quoted lines below it. A side
# agent's note is its opener line word for word and the quoted lines under it. Every forward is cut, not the first.
BAND_SAID = (DREAMER_SAID + '. Its reasoning:', DREAMER_SAID + '. It is a question, not a task.')  # v1
BAND_QUOTED = DREAMER_SAID + ', quoted. Its reasoning:'
PANE_V1, PANE_V2 = DREAMER_SAID + ': \u201c', DREAMER_SAID + ', quoted: \u201c'
PANE_MID = '\u201d Its reasoning: \u201c'
CARD_OPEN = "CSR's compaction card ("
CARD_STAGES = ('compacting soon', 'compacting', 'compaction skipped', 'compacted by csr')  # compact_section's headings
STORY_OPEN = re.compile(r"CSR's story of this session, rev \d+(, quoted: \u201c|:)")
PANE_SEND_MOST, CARD_SEND_MOST, STORY_SEND_MOST = 700, 1200, 700  # what each send is cut at
NOTE_OPENERS = ('Here is a note offered by a side agent:', 'Here is a note offered by CSR (claude-self-reflect):')
ASKS = re.compile(r'[?\uff1f\u061f]$')
KEYED = re.compile(r'[0-9\s>]*$')  # a band hotkey that landed in the box, or a quote marker: no word of yours


def straight(text):
    """What v2 puts inside its quotes: no control, “ and ” made straight, spaces collapsed."""
    return ' '.join(CONTROL.sub('', str(text)).replace('\u201c', '"').replace('\u201d', '"').split())


def closed_send(head, body, most):
    """`<head> “<body>”` on one line, at most `most` characters, the body straight and cut with … when too long, still
    closed. `head` ends in `, quoted:`."""
    head = ' '.join(CONTROL.sub('', head).split()) + ' '
    return f'{head}\u201c{fit(straight(body), max(1, most - len(head) - 2))}\u201d'


def legacy_end(line, start, most):
    """v1: never a closing-quote search. After one_line's cut, when the send is exactly `most` long and ends on …;
    else the end of the line."""
    cut = start + most - 1
    return cut + 1 if cut < len(line) and line[cut] == '\u2026' else len(line)


def closed_end(line, start, at, most):
    """v2: past the first ” after `at`, when it falls within the send's `most` characters; else the end of the line."""
    close = line.find('\u201d', at)
    return close + 1 if close != -1 and close + 1 - start <= most else len(line)


def next_send(line, at):
    """The next one-line forward on a line at or after `at`: (start, end), or None."""
    found = []
    v2 = line.find(PANE_V2, at)
    if v2 != -1:
        question = line.find('\u201d', v2 + len(PANE_V2))
        whole = question != -1 and line.startswith(PANE_MID, question)
        found.append((v2, closed_end(line, v2, question + len(PANE_MID), PANE_SEND_MOST) if whole else len(line)))
    i = line.find(PANE_V1, at)
    while i != -1:
        if '\u201d Its reasoning:' in line[i + len(PANE_V1):]:
            found.append((i, legacy_end(line, i, PANE_SEND_MOST)))
            break
        i = line.find(PANE_V1, i + 1)
    i = line.find(CARD_OPEN, at)
    while i != -1:
        after = i + len(CARD_OPEN)
        stage = next((known for known in CARD_STAGES
                      if line.startswith(known + '), quoted: \u201c', after) or line.startswith(known + '):', after)), None)
        if stage is not None:
            body = after + len(stage)
            quoted = line.startswith('), quoted: \u201c', body)
            found.append((i, closed_end(line, i, body + 12, CARD_SEND_MOST) if quoted else legacy_end(line, i, CARD_SEND_MOST)))
            break
        i = line.find(CARD_OPEN, i + 1)
    story = STORY_OPEN.search(line, at)
    if story:
        found.append((story.start(), legacy_end(line, story.start(), STORY_SEND_MOST) if story.group(1) == ':'
                      else closed_end(line, story.start(), story.end(), STORY_SEND_MOST)))
    return min(found) if found else None


def cut_sends(line):
    """A line with its one-line forwards cut as spans: your words before, between and after them, or None when it
    carries none."""
    pieces, at = [], 0
    span = next_send(line, at)
    while span:
        before = line[at:span[0]]
        if not (at == 0 and KEYED.match(before)):
            pieces.append(before)
        at = span[1]
        span = next_send(line, at)
    if at == 0:
        return None
    pieces.append(line[at:])
    return ' '.join(piece.strip() for piece in pieces if piece.strip())


def strip_forwards(text):
    """The text with every forward cut and your paragraphs kept (trimmed, joined by a blank line); the text as it
    was when it carries none."""
    lines = text.replace('\r\n', '\n').strip().split('\n')
    blank = lambda line: not line.strip()
    quoted = lambda line: line.lstrip().startswith('>')
    cut, found = [False] * len(lines), False
    for at, line in enumerate(lines):
        v1, v2 = line.strip() in BAND_SAID, line.strip() == BAND_QUOTED
        if not (v1 or v2 or line.strip() in NOTE_OPENERS):
            continue
        above = next((i for i in range(at - 1, -1, -1) if not blank(lines[i])), None)
        asked = lines[above] if above is not None and not cut[above] else ''
        # v1: a line that asks, cut whole. v2: the “…” span that ends the line above, from its last “.
        opens = asked.rfind('\u201c') if v2 and asked.rstrip().endswith('\u201d') else -1
        question = bool(v1 and ASKS.search(asked.strip())) or opens != -1
        below = at + 1
        while below < len(lines) and blank(lines[below]):
            below += 1
        end = below
        while end < len(lines) and quoted(lines[end]):
            end += 1
        if not question and end == below:
            continue
        cut[at] = True
        for i in range(below, end):
            cut[i] = True
        if opens != -1 and not KEYED.match(asked[:opens]):
            lines[above] = asked[:opens].rstrip()  # your words before the question
        elif question:
            cut[above] = True
        found = True
    kept = []
    for i, line in enumerate(lines):
        if cut[i]:
            continue
        own = cut_sends(line)
        if own is None:
            kept.append(line)
            continue
        found = True
        if own:
            kept.append(own)
    if not found:
        return text
    return '\n\n'.join(p.strip() for p in re.split(r'\n\s*\n', '\n'.join(kept)) if p.strip())


def own_words(text):
    """What you typed yourself in a prompt: blocks you pasted (often another agent's words), injected tags and
    quoted lines taken out, and every forward (strip_forwards)."""
    typed = NOT_TYPED_TAIL.sub(' ', NOT_TYPED.sub(' ', text))
    return '\n'.join(line for line in strip_forwards(typed).splitlines() if not line.lstrip().startswith('>'))


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
                     and bool(own_words(text).strip()) and not text.lstrip().startswith('<'))
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
    """Whether a lookup names the word on its own, in quotes (grep -ril "osprey") or as the whole search
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
        private.write_atomic(COACH_KEPT, json.dumps({p: v for p, v in kept.items() if p in paths}), root=KEEP)
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
    The story is pinned on top; the rest come and go under it (Feed): a notice for notice_life() from its event,
    a card waiting on your press until you answer it.
    Above them, the cockpit: three rows of gauges (context, compaction payback and cache, the plan's quotas and CSR's
    status; the compaction timer takes row 2 while one runs). The rest of the session's facts are one key away (i).
    Code that changed since your past sessions stays one key away too (l). On a phone, where the pane is the
    whole screen and tall, each section shows more: PHONE_MORE items where it caps them, and more of the story."""
    root = data['repo']
    more = PHONE_MORE if (ui or {}).get('phone') else 1
    con = connect()
    rules_at = []  # when each rule shown was said: the section's newest content
    try:
        projects = {p for p in (s.get('project'), os.path.basename(root)) if p}
        marks = ','.join('?' * len(projects))
        rules = []  # only your own words: the detector also hears Claude, to itself and to its subagents
        for when, quote, session, transcript, at in con.execute(
                f"select ts, quote, session_id, transcript_path, byte_start from intent_events where project in "
                f"({marks}) and kind = 'correction' order by ts desc limit 200", tuple(projects)).fetchall():
            if len(rules) >= 3 * more:
                break
            if not typed_by_you(transcript, at, quote):
                continue
            day = short_date(epoch(when)) if epoch(when) else (when or '')[:10]
            here = ' · this session' if session == s['id'] else ''
            rules_at.append(epoch(when))
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
        return closed_send(f"{DREAMER_SAID}, quoted: “{straight(d['line'])}” Its reasoning:", f"{d.get('title') or ''}. {' '.join(reasons[:3])}",
                           PANE_SEND_MOST)

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
    folded = ''
    if earlier:
        is_open = 'passed' in (ui.get('folds') or ())
        took = len(earlier) - len(fed['passed'])
        folded = ' · '.join(x for x in (f"{took} adopted" if took else '',
                                        f"{len(fed['passed'])} passed" if fed['passed'] else '') if x)
        dreams.append(dict(key='fold:passed', quiet=True, lines=1, title=f"{'⌄' if is_open else '›'} " + folded,
                           detail=[], send=''))
        if is_open:  # rows a click unfolded: not what the section holds (signature leaves them out)
            for d in earlier:
                reasons = reasons_of(d)
                dreams.append(dict(key='dream:' + d['id'], title=d['line'], lines=1, unsigned=True,
                                   meta=' · '.join(x for x in (stamp(d['at'], now)[0], ANSWER_WORD.get(d['answer'], 'not answered')) if x),
                                   detail=[d.get('title') or ''] + reasons[:2], send=dream_send(d, reasons)))

    # Only what Claude used: a hand-off it then opened, at most two rows. Nothing opened, no section: the
    # counts of what was handed, opened and asked are the i page's (handed_line), never a row to send.
    handed, handed_at = [], []
    for item in s.get('remembered') or []:
        if item['kind'] == 'recall' and item.get('opened') and len(handed) < 2 * more:
            handed_at.append(item.get('at'))
            where = ' · '.join(x for x in (from_when(item.get('when'), now), item.get('project'),
                                           f"{item['times']}×" if item['times'] > 1 else '') if x)
            gist = re.sub(r'\*\*|(?:^|\s)#+\s*', ' ', item['text']).strip()  # a note's markdown, as words
            handed.append(dict(recall_act(item, now), title=gist, meta='opened · ' + where))

    story, told = data.get('story'), []
    if story:
        told.append(dict(key='story', like=f"story:{s.get('id')}:{story['rev']}", title=story['now'], open_title=story['text'], lines=6 * more, open_lines=30, detail=[],
                         send=closed_send(f"CSR's story of this session, rev {story['rev']}, quoted:", story['text'], STORY_SEND_MOST)))
    if not data.get('dreams_on'):
        right = 'off'
    elif not fed['left']:
        right = '0 left · next compact'
    elif look and not starred:
        when = stamp(look['at'], now)[0]
        right = f"looked {'just now' if when == 'now' else when} · {fed['left']} left"
    else:
        right = f"{fed['left']} of {fed['of']} left"
    rights = story_rights(story, now)
    readings = data.get('readings') or []
    # count: what column mode shows of each, one number; at: its newest content's time, waits: it waits on you,
    # brief: a few words on it, for its row in Earlier (the feed's)
    sections = [
        dict(name='story', title='Story', items=told, empty='', count=story and story['rev'], right=rights[0],
             rights=rights[1:], at=story and story.get('at'), brief=story and f"rev {story['rev']}"),
        dict(name='dreams', title='What if…', right=right, items=dreams, empty='', count=fed['left'], star=starred,
             at=latest(d['at'] for d in ([fed['star']] if starred else []) + earlier), waits=starred, brief=folded),
        dict(name='remembered', title='Handed to Claude', right='', items=handed, empty='', count=len(handed),
             at=latest(handed_at), brief=counted(len(handed), 'opened')),
        dict(name='coach', title='How Claude read you', right='', items=reading_items(readings, now), empty='',
             at=latest(r.get('when') for r in readings), brief=counted(len(readings), 'reading')),
        dict(name='rules', title='Your rules', right='', items=rules, empty='', at=latest(rules_at),
             brief=counted(len(rules), 'rule')),
    ]
    sections[2:2] = lost_sections(s, now, 3 * more)
    card = compact_section((data.get('infinite') or {}).get('card'), data.get('compacting'), lost_waiting(s.get('lost')))
    if card:  # CSR's own compaction: under the story, which is pinned on top (the feed keeps it there)
        sections.insert(1, card)
    head = dict(gauges=gauges(data.get('claude_fields'), data.get('claude_at'), s, now, data.get('limits'),
                              data.get('sts'), data.get('compacting')),
                session=s if s.get('learned') else None, status=data.get('claude_line'))
    return dict(head=head, quiet='Nothing to report yet. A section shows here once CSR has something for it.',
                sections=sections)


# ---------------------------------------------------------------- the default view

PIC = '\x1b[8m\x1b[28m'  # an invisible pair of colour codes: it marks a row a picture will cover
PIC_MIN = 24  # fewer cells than this and a picture gives way to one row of block characters
PIC_ROWS = lambda rows: 4 if rows >= 30 else 3
HOLD_MAX = 120  # seconds the main page's sections stay as drawn while Claude works
TOLD_WHEN = dict(startup='at start', resume='at resume', compact='after compact', clear='after clear')


def told_lines(text):
    """CSR's SessionStart block in the parts the pane shows: dict(recap, dreamt=[...], anchors, index, full).
    recap is the 'recap [' line without its marker, else the first line that is not CSR's own preamble."""
    lines = [line.strip() for line in str(text or '').splitlines()]
    recap = next((re.sub(r'\s*\[\[CSR:RECAP\]\]\s*$', '', line) for line in lines if line.startswith('recap [')), None)
    if recap is None:
        recap = next((line for line in lines if line and not line.startswith(
            ('CSR ENDLESS MEMORY ACTIVE', 'The blocks below are memory', 'Reflex:', '[briefing', '#', '**'))), None)
    full = next((line[len('Full state:'):].strip() for line in lines if line.startswith('Full state:')), None)
    at = next((k for k, line in enumerate(lines) if 'EPISODE INDEX' in line), None)
    return dict(recap=recap, dreamt=[line for line in lines if line.startswith('Dreamt:')],
                anchors=next((line for line in lines if line.startswith('ANCHORS:')), None),
                index=sum(1 for line in lines[at + 1:] if line.startswith('- [')) if at is not None else 0, full=full)


def told_section(s):
    """"CSR told Claude": the recap CSR injected at the start, one item that opens to the rest."""
    told = s.get('told')
    parts = told_lines(told['text']) if told else None
    if not parts or not parts['recap']:
        return None
    parts = dict(parts, dreamt=[one_line(x, 300) for x in parts['dreamt']],
                 anchors=one_line(parts['anchors'], 300) if parts['anchors'] else None,
                 full=one_line(parts['full'], 300) if parts['full'] else None)  # transcript text: no control reaches a row
    anchors = ' · '.join(x for x in (parts['anchors'], f"{parts['index']} earlier threads" if parts['index'] else '') if x)
    source = str(told.get('source') or 'startup')
    when = datetime.fromtimestamp(told['at']).strftime('%H:%M') if told.get('at') else ''
    recap, full = one_line(parts['recap'], 600), parts['full']
    item = dict(key='told:recap', like=f"told:{s.get('id')}:{told.get('at')}", title=recap, lines=3, plain_body=True,
                body=[x for x in (parts['dreamt'][:1] + [anchors]) if x],
                detail=parts['dreamt'][1:3] + ([full] if full else []),
                send=one_line(f"CSR's recap at the start of this session: {recap}" + (f' The rest: {full}' if full else '')))
    said = TOLD_WHEN.get(source, f'at {one_line(source, 12)}')
    return dict(name='told', title='CSR told Claude', right=' '.join(x for x in (said, when) if x), items=[item], empty='',
                at=told.get('at'), brief=said)


def last_section(s, now, rows):
    """"Last session": the session before this one in a picture and two rows of numbers."""
    facts = s.get('last_session')
    if not facts or not facts.get('id'):
        return None
    id8 = facts['id'][:8]
    typed, said = facts.get('typed'), facts.get('said')
    title = one_line(facts.get('title') or (typed and gist(typed[1])) or f'session {id8}', 200)
    running = session_record(facts['id']) is not None
    active, prompts, compacts = facts.get('active') or 0, facts.get('prompts') or 0, facts.get('compacts') or 0
    learned, handed, peak, floor = facts.get('learned'), facts.get('handed'), facts.get('peak'), facts.get('floor')
    first = [f'{span(active)} active', f'{prompts} prompts' if prompts else '', f'{compacts} compacts' if compacts else '']
    second = [f'CSR learned {short(learned)}' if learned else '', '', f'peak {short(peak)}' if peak else '']
    if handed:
        second[1] = ('handed' if learned else 'CSR handed') + f' {handed}×'
    body = [' · '.join(x for x in line if x) for line in (first, second)]
    detail = [f"After a compaction the context settled near {short(floor)}, the median of the last "
              f"{min(FLOOR_ROWS, compacts)}." if floor and compacts else '',
              f'You last: {quoted(typed[1], 200)}' if typed else '', f'Claude last: {quoted(said[1], 200)}' if said else '']
    item = dict(key='last:' + facts['id'], title=title, lines=1, fit_body=True,
                picture=dict(facts=facts, rows=PIC_ROWS(rows)), body=[x for x in body if x],
                detail=[x for x in detail if x],
                send=one_line(f'Last session {id8} (“{title}”): {span(active)} active, {prompts} prompts, {compacts} '
                              f'compactions, last row {hhmm(facts.get("last"))}. '
                              f'The rest of it: csr_reflect_on_past("conv_{facts["id"]}")'))
    right = f"{id8} · {'still running' if running else stamp(facts['last'], now)[0]}" if facts.get('last') else id8
    return dict(name='last', title='Last session', right=right, items=[item], empty='', at=facts.get('last'), brief=id8)


def todos_section(s):
    """"Todos left": what the session before this one left open, one item each."""
    facts = s.get('last_session') or {}
    id8 = (facts.get('id') or '')[:8]
    items = [dict(key=f'todo:{id8}:{k}', title=todo['subject'], lines=2,
                  meta=f"{todo['status'].replace('_', ' ')} · from {id8}",
                  send=one_line(f"Left over from session {id8}: “{todo['subject']}” ({todo['status']})."))
             for k, todo in enumerate(s.get('todos') or [])]
    return dict(name='todos', title='Todos left', right='', items=items, empty='', at=facts.get('last'),
                brief=counted(len(items), 'todo')) if items else None


def default_view(page, s):
    """True while the session has nothing of its own: it has an id and no turn has ended; or every section
    that has items is 'rules' or 'lost', and every thread not set aside is one carried from the session before."""
    if not (s or {}).get('id'):
        return False
    if not s.get('turn_end'):
        return True
    if not {section['name'] for section in page['sections'] if section['items']} <= {'rules', 'lost'}:
        return False
    return all(t['state'] == 'carried' for t in s.get('lost') or [] if not t['hidden'])


def page_default(page, data, s, now, rows):
    """The page with its sections replaced by what a fresh session can show from real data, the head (the
    cockpit) kept: a dream waiting on you, what CSR told Claude, the last session, what it left over."""
    own = {section['name']: section for section in page['sections']}
    star = feed(data.get('dreams') or [], data.get('chapter'), s.get('id'))['star'] is not None
    sections = [own['dreams']] if star and own.get('dreams') and own['dreams']['items'] else []
    sections += [x for x in (told_section(s), last_section(s, now, rows)) if x]
    lost = lost_sections(s, now)
    if lost:
        every = s.get('lost') or []
        sections.append(dict(lost[0], title='Left over',
                             right=f"{sum(1 for t in every if not t['hidden'] and t['waiting'])} waiting · f all {len(every)}"))
    if todos_section(s):
        sections.append(todos_section(s))
    # what this view shows is news from the session's start, however old the last session is: each notice's
    # clock starts when CSR told Claude, else when the pane first saw it
    begun = (s.get('told') or {}).get('at')
    sections = [section if 'born' in section else dict(section, born=begun) for section in sections]
    taken = {section['name'] for section in sections} | {'lost', 'story'}
    sections += [section for section in page['sections'] if section['items'] and section['name'] not in taken]
    quiet = 'Reading the session before this one…' if s.get('last_session') is None \
        else 'First session here: no earlier session in this project to carry over.'
    return dict(page, sections=sections, quiet=quiet)


def held_sections(hold, busy, now):
    """The sections to draw in place of fresh ones: the held ones while Claude works and they are under
    HOLD_MAX seconds old; None to draw fresh. hold is dict(sections, at) or None."""
    return hold['sections'] if hold and busy and now - hold['at'] < HOLD_MAX else None


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
PHONE_GUTTER = 2  # on a phone the section icons' column goes: text starts here, and has the width
PHONE_MORE = 2  # on a phone, the tall screen shows this many times the items a section caps


def gutter(ui):
    """Where a section's text starts: under its title, past the icons' column; on a phone, near the edge."""
    return PHONE_GUTTER if (ui or {}).get('phone') else ITEM


HEART = ('♡', '♥')  # an item you have not liked, and one you have: in the faint ink, at its row's right end
HEART_MARK = '\x1b[24m'  # a code that changes nothing on screen, set just before the heart: page_lines finds its row by it


def with_heart(row, liked, width):
    """The row with its heart in its last column, width - 1."""
    gap = max(1, width - 1 - len(COLOUR.sub('', row)))
    return row + ' ' * gap + HEART_MARK + ink('faint') + HEART[bool(liked)] + OFF


def like_key(item):
    """What a like is kept under: the item's own like key where its key alone is not one thing (the story is
    told again under the same key), else its key."""
    return item.get('like') or item['key']


def likeable(item, name):
    """A feed item takes a like: never a fold, a quiet row, or a row of Earlier."""
    return name != 'earlier' and not item.get('quiet') and not item.get('unrated') and not str(item['key']).startswith(('fold:', 'back:'))


def item_rows(item, name, width, is_open, at=ITEM, heart=None):
    """One item: its title on up to two lines (all of it once opened), its body where it has one (the star
    dream's reasoning, always shown), then what CSR knows about it, quiet, with a flag first in the risk ink.
    at: the gutter its text starts at. heart, when not None, is whether you liked it: its heart then ends the
    meta row, or the first row when it has no meta, and that row's words leave it two columns."""
    room = width - at
    flag, meta = item.get('flag') or '', item.get('meta') or ''
    spare = 2 if heart is not None else 0  # the heart and the space before it
    title = item.get('open_title', item['title']) if is_open else item['title']
    title = wrap(title.replace('`', ''), room - (0 if flag or meta else spare),
                 item.get('open_lines', 6) if is_open else item.get('lines', 2))
    voice = ink('quiet') if item.get('quiet') else ink('you')  # lamp black: the page's own type
    rows = [' ' * at + voice + line + OFF for line in title]
    rows += [' ' * at + voice + fit(line, room) + OFF for line in item.get('lede') or ()]  # whole rows, before a picture
    picture = item.get('picture')
    if picture and PIXELS and room >= PIC_MIN:  # rows a picture will cover, marked for page_lines
        rows += [' ' * at + PIC] * picture['rows']
    elif picture and room >= 8 and picture.get('kind', 'last') == 'last':  # the last session's series in block
        rows.append(' ' * at + ink('ctx') + last_sparkline(picture['facts'], room) + OFF)  # characters; a payback, its text rows alone
    for k, text in enumerate(item.get('body') or ()):
        if item.get('fit_body'):
            rows.append(' ' * at + ink('quiet') + fit_pieces(text, room) + OFF)
            continue
        voice = ink('you' if k < item.get('loud', 0) else 'quiet') + (BOLD if k == 0 and not item.get('plain_body') else '')
        rows += [' ' * at + voice + line + OFF for line in wrap(text, room, 1 if k == 0 else 2)]
    text = fit(' · '.join(x for x in (flag, meta) if x), room - spare)
    line = ink('faint') + text
    if flag and text.startswith(flag):
        line = ink('turn') + flag + ink('faint') + text[len(flag):]
    rows = rows + [' ' * at + line + OFF] if text else rows
    if heart is not None:
        at_row = len(rows) - 1 if text else 0
        rows[at_row] = with_heart(rows[at_row], heart, width)
    return rows


ANSWERS = (('helpful', '✓ helpful'), ('thought_of_it', '= thought of it'),
           ('not_relevant', '✗ not relevant'), ('ask_claude', '→ ask Claude'))


def answer_rows(item, width, ui):
    """The star dream's answers, two to a row; a click on either half answers it. Each row is its own
    block, so a click knows the dream and, by its column, the answer."""
    if not item.get('answers'):
        return []
    at = gutter(ui)
    half = (width - at) // 2
    rows = []
    for pair in (ANSWERS[:2], ANSWERS[2:]):
        text, spans = ' ' * at, []
        for k, (answer, label) in enumerate(pair):
            start = at + k * half
            spans.append((start, start + half, answer))
            text += ink(SECTION['dreams']) + BOLD + label[0] + OFF + ink('quiet') + fit(label[1:], half - 2).ljust(half - 1) + OFF
        rows.append(([text], dict(key=item['key'] + ':answers', dream=item['dream'], send=item['send'], spans=spans)))
    answered = ui.get('answered')
    if answered and answered[0] == item['dream']:
        said = ANSWER_WORD.get(answered[1], answered[1])
        rows = [([' ' * at + ink('faint') + fit(f'✓ kept: {said}', width - at) + OFF], None)]
    return rows


def page_lines(page, width, rows, ui):
    """The page in the terminal's type on paper: its facts and the session's gauges, then each section
    with something to report, its rows in its own tint (every other one a shade deeper), a strip of paper
    between. Every section keeps its heading and first row; the room left goes to the earlier ones first."""
    head, s = page['head'], page['head'].get('session')
    label = '\x1b[38;2;%d;%d;%dm' % ON_DARK['label']
    lanes = None
    at = gutter(ui)
    if head.get('gauges'):  # the cockpit: three rows, nothing else (two on a phone)
        top = cockpit(head['gauges'], width, ui.get('phone'), ui.get('logo'))
    else:  # the l page: its title and facts
        top = [' ' + line for line in ansi_wrap(head.get('status') or '', width - 1, 2)]  # Claude's status line
        top[:0] = compact_rows(head.get('compacting'), width)  # while a compaction runs
        if head.get('title'):
            top.append(' ' + BOLD + fit(head['title'], width - 1) + OFF)
        top.append(' ' + fit_pieces(head['facts'], width - 1))
        if s:
            if PIXELS and width - LANE > 8:
                lanes = len(top)
                top += [' ' + label + name.ljust(LANE - 1) + OFF for name in ('you', 'context', 'memory')]
            top.append('  ' + lanes_legend(s, width - 2, dark=True))
    top += infinite_rows(head.get('infinite'), width)  # the mod's plan for the next compaction, and the story
    grounds = dark_steps(len(top))  # one gradient, top row to bottom
    out = Lines([on_dark(line, ground) for line, ground in zip(top, grounds)])
    out.acts.update({row: dict(key='view:info') for row in range(min(MASTHEAD_CLICK, len(top)))})
    out.grounds = grounds  # the vertical tape takes them beside the masthead
    out.heads, out.wanted = [], []  # wanted: (first row, rows, picture, ground) of each picture to lay over blank rows
    out.lanes = None if lanes is None else (lanes, grounds[lanes:lanes + 3])  # the gauges' row, their grounds
    built = []
    likes = ui.get('likes')  # key -> liked, from the pane; without it no item has a heart
    for section in page['sections']:
        blocks = []
        for item in section['items']:
            is_open = ui.get('open') == item['key']
            heart = likes.get(like_key(item), False) if likes is not None and likeable(item, section['name']) else None
            rows_ = item_rows(item, section['name'], width, is_open, at, heart)
            if is_open:
                rows_ = opened(rows_, section['name'], item.get('detail') or [], width, ui, item['key'], indent=at,
                               whole=item.get('whole_detail', False))
            blocks.append((rows_, item))
            blocks += answer_rows(item, width, ui) + lost_rows(item, width, ui, is_open)
        if blocks:
            built.append((section, blocks, 2 + len(blocks[0][0]), 2 + sum(len(rows_) for rows_, _ in blocks)))
    if not built:
        return out.put([' ' + ink('faint') + l + OFF for l in wrap(page.get('quiet') or '', width - 2, 3)])
    left = rows - len(out) - sum(base for *_, base, _ in built)  # the footer's row out, the first strip's back in
    rooms = {}
    for k in sorted(range(len(built)), key=lambda k: built[k][0]['name'] != 'lost'):  # the spare rows: Lost and found first
        _, _, base, want = built[k]
        rooms[k] = base + max(0, min(want - base, left))
        left -= rooms[k] - base
    for k, (section, blocks, base, want) in enumerate(built):
        room = rooms[k]
        name = section['name']
        part = Lines([heading(name, section['title'], [section['right']] + list(section.get('rights') or ()), width, at,
                              section.get('life'))])
        fit_blocks(part, blocks, room - 1, ui, tail=False)
        for i, line in enumerate(part):  # the heart's row: a click on its last cells likes the item
            if HEART_MARK in line and part.acts.get(i):
                part.acts[i] = dict(part.acts[i], heart=(width - 3, width + 1), source=name)
        shade = []
        for i, line in enumerate(part):  # the heading a shade deeper than its items, which share one tint
            back = tint(name, HEAD if i == 0 else ROW)
            opened_row = line.startswith(band(name))
            shade.append(tint(name) if opened_row else back)
            part[i] = line if opened_row else tinted(line, '\x1b[48;2;%d;%d;%dm' % back)
        if k:  # a strip of paper between sections; the first sits right under the masthead
            out.put([''])
        out.heads.append((len(out), name, (shade[0], shade[1] if len(shade) > 1 else GROUND)))
        start = len(out)
        out.extend(part)
        out.acts.update({start + i: act for i, act in part.acts.items()})
        i = 0
        while i < len(part):  # each run of rows marked PIC is where one item's picture goes
            if PIC not in part[i]:
                i += 1
                continue
            j = i
            while j + 1 < len(part) and PIC in part[j + 1]:
                j += 1
            picture = (part.acts.get(i) or {}).get('picture')
            if picture and picture['rows'] == j - i + 1:
                out.wanted.append((start + i, j - i + 1, picture, shade[i]))
            i = j + 1
    return out


def draw_page(page, cols, rows, ui):
    """The page on paper in the terminal's own type, so every row takes a click; pictures only for the
    gauges and the section icons, which no click needs."""
    width = max(24, cols - 1)
    logo = bool(PIXELS and page['head'].get('gauges'))  # CSR's mark over the masthead's CTX
    lines = page_lines(page, width, rows, dict(ui, logo=logo) if logo else ui)
    pictures = []
    if PIXELS:
        cell = cell_size()
        if logo:
            try:
                pictures.append((0, 1, LOGO[0], LOGO[1], logo_png(cell, lines.grounds[0])))
            except Exception:  # the word is better than a hole
                lines[0] = on_dark(cockpit(page['head']['gauges'], width, ui.get('phone'))[0], lines.grounds[0])
        s = page['head'].get('session')
        if s and lines.lanes is not None:
            row, grounds = lines.lanes
            pictures.append((row, LANE, width - LANE, 3, session_png(s, width - LANE, cell, time.time(), grounds)))
        at = gutter(ui)
        if at >= ICON[0]:  # a phone's gutter has no room for the icons
            pictures += [(row, 0, ICON[0], ICON[1], iso_png(name, cell, backs))
                         for row, name, backs in lines.heads if row + ICON[1] <= rows - 1]
        for row, run, picture, ground in getattr(lines, 'wanted', ()):
            draw = payback_png if picture.get('kind') == 'payback' else last_png
            png = draw(picture['facts'], width - at, run, cell, ground)
            if png:
                pictures.append((row, at, width - at, run, png))
    return lines, pictures


def draw_main(data, s, cols, rows, now, ui=None, hold=None, hold_quiet=None, feed=None):
    """The main page. A session with nothing of its own yet gets the default view; hold, the sections as last
    drawn, replaces the sections while Claude works (the masthead stays live). feed, the session's Feed, puts
    the sections in feed order with Earlier last; a held page is already in it."""
    if s is None:
        s = dict(id=None, title=None, prompts=[], replies=[], compacts=[], gave=dict(recalls=[], carried=[], dreams=[]),
                 remembered=[], sessions=0, project=None, files={}, edited=[], shell={}, last={})
    page = with_compacting(page_main(data, s, now, ui), data)
    if default_view(page, s):
        page = page_default(page, data, s, now, (ui or {}).get('rows', rows))
    if feed is not None and hold is None:
        page = feed.page(page, now)
    if hold is not None:  # held still while Claude works, but a notice whose time is up leaves all the same
        page['sections'] = living(hold, now)
        page['quiet'] = hold_quiet if hold_quiet is not None else page.get('quiet')
    lines, pictures = draw_page(page, cols, rows, ui or {})
    lines.sections, lines.quiet = page['sections'], page.get('quiet')
    return lines, pictures


def draw_log(data, cols, rows, ui=None):
    return draw_page(page_log(data), cols, rows, ui or {})


# ---------------------------------------------------------------- the main page as a feed

SEEN_DIR = 'seen'  # KEEP/seen/<session>.json: what each main-page section held, and when it was first on screen
SEEN_MIN = 3 * 60  # seconds on screen, its content unchanged, before a section moves to Earlier
STALE = 45 * 60  # content older than this moves to Earlier once it has been on screen at all
NEW_FOR = 60  # seconds a section with new content says "new" in its heading
SEEN_EVERY = 30  # the seen file is written at most this often
SECTION['earlier'], GLYPH['earlier'] = 'quiet', '↓'  # Earlier: graphite, as Changed since
NOTICE_LIFE = 300  # seconds a notice stays under the story, from its own event, before it leaves (CSR_CARD_LIFE_S)
PINNED = ('story',)  # the one card that never leaves the top of the page


def notice_life():
    """A notice's life in seconds: CSR_CARD_LIFE_S when it is a whole number over zero, else NOTICE_LIFE."""
    raw = os.environ.get('CSR_CARD_LIFE_S', '').strip()
    return int(raw) if raw.isdigit() and int(raw) > 0 else NOTICE_LIFE


def waits_on_press(section):
    """A card whose buttons wait on the person: the dreamer's open question, Lost and found's asks, a compaction
    still counting down or running. The answer ends it, never a clock; Earlier is the feed's own."""
    return bool(section.get('star') or section.get('live') or section['name'] in ('lost', 'earlier'))


def timed(section):
    """A notice: a one-way report under the story (a compaction done, the last session, what CSR told Claude,
    what it handed over, a rule, a reading), with a life on a clock like a notification's."""
    return section['name'] not in PINNED and not waits_on_press(section)


def life_share(section, now):
    """The share of a notice's life still to run: 1 at its event, 0 as it leaves; None for a card with no clock."""
    born = section.get('born')
    if not timed(section) or not isinstance(born, (int, float)) or isinstance(born, bool):
        return None
    life = notice_life()
    return max(0.0, min(1.0, (born + life - now) / life))


def living(sections, now):
    """The sections still on the page at now: a notice past its life has left it; each one that lives carries
    life, the share left, for its heading's tape."""
    out = []
    for section in sections:
        share = life_share(section, now)
        if share is not None and share <= 0:
            continue
        out.append(dict(section, life=share) if share is not None else section)
    return out


def life_cell(share):
    """A notice's time left as one cell of the cache tape's eighths, draining down as it goes; the empty rail
    in its last moments."""
    fill = fill_levels(share, 1)[0]
    return EIGHTHS[fill] if fill else RAIL


def signature(section):
    """What a section holds, short: a hash of its items' keys and words. A row a click unfolds (a fold and what it
    opens) is no part of it, so opening one never makes the section new."""
    import hashlib
    said = [(item['key'], item.get('title') or '', item.get('open_title') or '') for item in section['items']
            if not item.get('unsigned') and not str(item['key']).startswith('fold:')]
    return hashlib.sha256(json.dumps(said, ensure_ascii=False).encode()).hexdigest()[:12]


def age_of(seconds):
    """How long ago, shortest: 'now', '82m', '5h', '3d'."""
    minutes = int(max(0, seconds) // 60)
    if minutes < 1:
        return 'now'
    if minutes < 120:
        return f'{minutes}m'
    return f'{minutes // 60}h' if minutes < 48 * 60 else f'{minutes // 1440}d'


def first_words(section):
    """The start of what a section says: its first row that is not a fold, else the fold's own words."""
    item = next((i for i in section['items'] if not str(i['key']).startswith('fold:')), section['items'][0])
    title = item.get('title') or ''
    return title[2:] if str(item['key']).startswith('fold:') else title


def earlier_row(section, at, now):
    """One row of Earlier: where it came from, a few words on it (or that it waits on you), how old it is, and
    the start of what it says: 'Story · rev 11 · 82m · Two v5/v6 screenshots were already…'."""
    said = 'waits on you' if section.get('waits') else section.get('brief') or ''
    words = first_words(section).replace('`', '')
    parts = (section['title'], said, age_of(now - at) if at else '', '' if words == said else words)
    return dict(key='back:' + section['name'], title=one_line(' · '.join(x for x in parts if x), 300), lines=1,
                quiet=True, detail=[], send='', back=section['name'])


def read_seen(path):
    """The seen file's sections: name -> dict(sig, since, seen, back, new, at); {} when there is none or it is
    of another shape."""
    try:
        with open(path, encoding='utf-8') as handle:
            got = json.load(handle).get('sections')
    except (OSError, ValueError, AttributeError, TypeError):
        return {}
    keys = ('sig', 'since', 'seen', 'back', 'new', 'at')
    return {name: {k: entry.get(k) for k in keys} for name, entry in (got if isinstance(got, dict) else {}).items()
            if isinstance(entry, dict) and isinstance(entry.get('sig'), str) and isinstance(entry.get('since'), (int, float))}


class Feed:
    """The main page as a feed, one per session. The story is pinned on top. Under it the sections run newest
    content first, and come and go like notifications: a notice (timed: a one-way report) lives notice_life() from
    its own event, a one-cell tape on its heading draining as it goes, then leaves the page. A card that waits on
    your press (waits_on_press) has no clock: once its content has stood on screen unchanged for SEEN_MIN, or is
    older than STALE and has been on screen once, it moves to Earlier at the bottom, one quiet row each (its row
    says it waits on you). A click on that row brings it back to the top until it has been seen again for
    SEEN_MIN. New content is fresh again and says "new" for NEW_FOR. What it knows is kept in
    KEEP/seen/<session>.json, written at most every SEEN_EVERY seconds, so a restarted pane goes on from it. The
    l, s, f and i pages stay as they are: the deeper way back, and the i page still carries the compactions."""

    def __init__(self, session_id):
        self.sid = session_id if session_id and re.fullmatch(r'[\w-]+', session_id) else None
        self.entries = read_seen(os.path.join(KEEP, SEEN_DIR, f'{self.sid}.json')) if self.sid else {}
        self.primed = bool(self.entries)  # a pane with a past: what changed while it was closed is new
        self.saved, self.dirty = 0.0, False

    def observe(self, sections, now):
        """Each section's entry brought up to what it holds now: new content starts again, unseen."""
        for section in sections:
            sig, entry = signature(section), self.entries.get(section['name'])
            if entry is None or entry['sig'] != sig:
                entry = self.entries[section['name']] = dict(sig=sig, since=now, seen=None, back=None,
                                                             new=now if self.primed else None, at=None)
                self.dirty = True
            entry['at'] = section.get('at')
            # a notice's clock starts at its own event; with none, when the pane first saw this content. born may
            # be set to None on purpose (the default view's cards): their news is this session's start
            born = section['born'] if 'born' in section else section.get('at')
            entry['born'], entry['timed'] = born or entry['since'], timed(section)
        self.primed = True

    def when(self, name):
        """A section's content time: its newest item's, else when the pane first saw this content."""
        entry = self.entries[name]
        return entry['at'] or entry['since']

    def fresh_at(self, name):
        """What the feed ranks by: the content's time, or when it last changed under the pane's eyes if later."""
        return max(self.when(name), self.entries[name]['new'] or 0)

    def sunk(self, name, now):
        """Whether a section has had its time: seen for SEEN_MIN, or seen once and older than STALE. One brought
        back waits out SEEN_MIN on screen again, however old. The story never sinks (it is pinned), nor does a
        notice: it leaves on its clock instead."""
        entry = self.entries.get(name)
        if not entry or entry['seen'] is None or name in PINNED or entry.get('timed'):
            return False
        return now - entry['seen'] >= SEEN_MIN or (not entry['back'] and now - self.when(name) > STALE)

    def is_new(self, name, now):
        entry = self.entries.get(name)
        return bool(entry and entry['new'] and now - entry['new'] < NEW_FOR)

    def life_level(self, name, now):
        """A notice's tape as the frame sees it: its eighths left, -1 once it has left; None for a card with no
        clock. The pane redraws each time it drains an eighth, and the moment it leaves."""
        entry = self.entries.get(name) or {}
        if not entry.get('timed') or entry.get('born') is None:
            return None
        share = life_share(dict(name=name, born=entry['born']), now)
        return fill_levels(share, 1)[0] if share else -1

    def key(self, now):
        """What the feed would change on screen at now, for the frame's signature: it redraws when a section sinks,
        stops being new, or a notice's tape drains a step or runs out, with nothing else moving."""
        return sorted((name, self.sunk(name, now), self.is_new(name, now), self.life_level(name, now))
                      for name in self.entries)

    def page(self, page, now):
        """The page's sections in feed order: the story pinned first, then those brought back, then the freshest;
        notices past their life gone; Earlier last."""
        live = [section for section in page['sections'] if section['items']]
        self.observe(live, now)
        live = living([dict(section, born=self.entries[section['name']]['born']) for section in live], now)
        pinned = [section for section in live if section['name'] in PINNED]
        rest = [section for section in live if section['name'] not in PINNED]
        fresh = [section for section in rest if not self.sunk(section['name'], now)]
        fresh.sort(key=lambda x: (self.entries[x['name']]['back'] or 0, self.fresh_at(x['name'])), reverse=True)
        gone = sorted((section for section in rest if self.sunk(section['name'], now)),
                      key=lambda x: self.fresh_at(x['name']), reverse=True)
        sections = [self.marked(section, now) for section in pinned + fresh]
        if gone:
            items = [earlier_row(section, self.when(section['name']), now) for section in gone]
            said = ['click one to bring it back', 'click to bring back']
            if not fresh:  # all of it seen: say why the page is this short
                said = ['nothing new · click one to bring it back', 'nothing new · click to bring back', 'nothing new']
            sections.append(dict(name='earlier', title='Earlier', right=said[0], rights=said[1:], items=items, empty='',
                                 count=len(items)))
        return dict(page, sections=sections + [section for section in page['sections'] if not section['items']])

    def marked(self, section, now):
        """A section with new content says so first in its heading."""
        if not self.is_new(section['name'], now):
            return section
        forms = [section['right']] + list(section.get('rights') or ())
        said = [' · '.join(x for x in ('new', form) if x) for form in forms]
        return dict(section, right=said[0], rights=said[1:] + ['new'])

    def shown(self, names, now):
        """The sections whose headings are on screen now: each seen from now, unless it already was."""
        for name in names:
            entry = self.entries.get(name)
            if entry and entry['seen'] is None:
                entry['seen'] = now
                self.dirty = True

    def back(self, name, now):
        """A click on its Earlier row: the section goes back to the top, unseen, until seen for SEEN_MIN again."""
        entry = self.entries.get(name)
        if entry:
            entry.update(back=now, seen=None)
            self.dirty = True

    def save(self, now):
        """The seen file, when something changed and SEEN_EVERY has passed since the last write."""
        if not (self.sid and self.dirty) or now - self.saved < SEEN_EVERY:
            return
        try:
            keep_json(SEEN_DIR, self.sid, dict(sections=self.entries))
        except OSError:
            return  # a pane that cannot write still ranks; it only forgets on a restart
        self.saved, self.dirty = now, False


def on_screen(heads, top, room):
    """The sections whose headings sit on the rows shown, top to top + room."""
    return [name for row, name, _ in heads if top <= row < top + room]


# ---------------------------------------------------------------- what CSR cost you this week, and what you took

WEEK = 7 * 86400  # the value line's window: a plan's weekly one, else the last seven days


def plan_of(fields, limits, now):
    """The window the value line counts in, one for every count: dict(mode, start, used).
    On a subscription Claude Code's status line carries rate_limits.seven_day (the session's own, else the account's
    newest): mode 'plan', the week that resets at its resets_at, so start is seven days before; used is its
    used_percentage, None once that week has reset with no newer reading (start then moves on by whole weeks).
    With an API key there are no rate_limits: mode 'api', the last seven days. A machine whose status line never
    reaches the pane reads as 'api' too: its line then says list price, still true."""
    for source in ((fields or {}).get('rate_limits') if isinstance(fields, dict) else None, limits):
        week = source.get('seven_day') if isinstance(source, dict) else None
        resets = seconds_of(week.get('resets_at')) if isinstance(week, dict) else None
        if resets is None:
            continue
        used = percent_of(week.get('used_percentage'))
        if resets <= now:  # that week is over: the next began, and its meter has not been read
            resets += WEEK * (1 + int((now - resets) // WEEK))
            used = None
        return dict(mode='plan', start=min(now, resets - WEEK), used=used)
    return dict(mode='api', start=now - WEEK, used=None)
VALUE_ROWS = 1  # the rows it takes above the legend: one, in the cockpit's codes; the i page says it in words


# US$ per million tokens, (input, output), by the first key a model's id holds, lowercased. Copied from csr-engine's
# own table, MODEL_PRICES in csr-engine/src/journal/composer.rs: keep the two the same.
MODEL_PRICES = (('mythos-5', 10.0, 50.0), ('sonnet-5', 3.0, 15.0), ('sonnet-4', 3.0, 15.0), ('fable-5', 10.0, 50.0),
                ('haiku-4', 1.0, 5.0), ('opus-5', 5.0, 25.0), ('opus-4', 5.0, 25.0), ('sonnet', 3.0, 15.0),
                ('haiku', 1.0, 5.0), ('opus', 5.0, 25.0))
CACHE_READ = 0.1  # a cache read, against the input rate
CACHE_WRITE = 1.25  # a cache write kept five minutes (and one whose life the call did not say)
CACHE_WRITE_1H = 2.0  # a cache write kept an hour
BOUND_MODEL = 'opus-5'  # a look from before its split was kept: every token priced as uncached input at this rate


def model_price(model):
    """(input, output) US$ per million tokens; None for a model the table does not know: it gets no price, never 0."""
    lower = str(model or '').lower()
    return next(((rate_in, rate_out) for key, rate_in, rate_out in MODEL_PRICES if key in lower), None)


def usage_counts(usage):
    """(input, output, cache read, cache write, of it kept an hour) from a usage in the API's spelling. The hour's
    share comes from cache_creation's 5m/1h split, where the call reported one; else 0."""
    usage = usage if isinstance(usage, dict) else {}
    split = usage.get('cache_creation') if isinstance(usage.get('cache_creation'), dict) else {}
    count = lambda value: max(0, int(value or 0)) if isinstance(value, (int, float)) else 0
    return (count(usage.get('input_tokens')), count(usage.get('output_tokens')), count(usage.get('cache_read_input_tokens')),
            count(usage.get('cache_creation_input_tokens')), count(split.get('ephemeral_1h_input_tokens')))


def cost_of(model, tokens_in, tokens_out, cache_read, cache_write, cache_write_1h=0):
    """US$ at list price for those counts; None when the model has no price. Cache reads at CACHE_READ of the input
    rate, an hour's cache writes at CACHE_WRITE_1H, the rest of the writes at CACHE_WRITE."""
    price = model_price(model)
    if price is None:
        return None
    hour = min(cache_write_1h, cache_write)
    return (price[0] * (tokens_in + CACHE_READ * cache_read + CACHE_WRITE * (cache_write - hour) + CACHE_WRITE_1H * hour)
            + price[1] * tokens_out) / 1e6


def narrative_week(start):
    """csr-engine's own model calls since start, from narrative_usage (every call, a failed one too: it was paid for):
    dict(calls, tokens, usd, unpriced). calls and tokens are the Haiku calls' (tokens all four counts, cache included);
    usd is every call's at list price, whatever model wrote the story, or None when one's model has no price
    (unpriced names them). These calls run with --no-session-persistence: no transcript holds them."""
    since = datetime.fromtimestamp(start, tz=timezone.utc).strftime('%Y-%m-%dT%H:%M:%S')
    con = connect()
    try:
        rows = con.execute("select model, count(*), sum(input_tokens), sum(output_tokens), sum(cache_read_tokens), "
                           "sum(cache_creation_tokens) from narrative_usage where ts >= ? group by model", (since,)).fetchall()
    finally:
        con.close()
    out = dict(calls=0, tokens=0, usd=0.0, unpriced=[])
    for model, calls, tokens_in, tokens_out, cache_read, cache_write in rows:
        counts = [int(x or 0) for x in (tokens_in, tokens_out, cache_read, cache_write)]
        if 'haiku' in str(model or '').lower():
            out['calls'] += int(calls or 0)
            out['tokens'] += sum(counts)
        cost = cost_of(model, *counts)
        if cost is None:
            out['unpriced'].append(model)
        else:
            out['usd'] += cost
    if out['unpriced']:
        out['usd'] = None
    return out


def log_lines(path):
    """Each line of a JSONL log as a dict; a missing log, or a line that is not one, gives nothing."""
    try:
        with open(path, encoding='utf-8') as handle:
            lines = handle.read().splitlines()
    except OSError:
        return []
    out = []
    for line in lines:
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if isinstance(record, dict):
            out.append(record)
    return out


def since(record, start):
    """Whether a log line was kept at or after start."""
    try:
        return float(record.get('at') or 0) >= start
    except (TypeError, ValueError):
        return False


def dreamer_lines(start):
    """The dreamer's looks since start, from the dream log and the mod's earlier one (one file when the two names
    meet): every look line, and every dream line, since a look that found a dream logs only the dream."""
    return [r for path in dict.fromkeys((DREAM_LOG, FORMER_DREAM_LOG)) for r in log_lines(path)
            if r.get('kind') in ('look', 'dream') and since(r, start)]


def look_tokens(record):
    try:
        return max(0, int(record.get('tokens') or 0))
    except (TypeError, ValueError):
        return 0


def called_model(record):
    """Whether a look or dream line spent tokens: its usage kept, or a total above zero."""
    return isinstance(record.get('usage'), dict) or look_tokens(record) > 0


def dreamer_looks(start):
    """(calls, tokens): the dreamer's model calls since start and their tokens, each line's one total (input + output +
    cache creation + cache read). A look that stayed quiet, skipped or met its budget called no model and is not
    counted: most looks are those. The dreamer runs on the session's own model, not Haiku."""
    calls = [r for r in dreamer_lines(start) if called_model(r)]
    return len(calls), sum(look_tokens(r) for r in calls)


def look_cost(record):
    """(US$, bound) for one look or dream line. Priced from the fork's usage and model where the mod kept them (since
    it does); a line from before (its tokens alone, the split unknown) at the most it could have cost: every token as
    uncached input at BOUND_MODEL's rate, bound True. (None, False) when the model has no price."""
    if isinstance(record.get('usage'), dict):
        return cost_of(record.get('model'), *usage_counts(record['usage'])), False
    tokens = look_tokens(record)
    return (tokens * model_price(BOUND_MODEL)[0] / 1e6, True) if tokens else (0.0, False)


def looks_cost(lines):
    """(US$, bound) for the dreamer's lines together: None when any one's model has no price; bound when any one was
    priced at its upper bound, so the sum is a ceiling, not a figure."""
    total, bound = 0.0, False
    for record in lines:
        cost, upper = look_cost(record)
        if cost is None:
            return None, False
        total, bound = total + cost, bound or upper
    return total, bound


def dreams_taken(start):
    """The dreams you took up since start (helpful, or handed to Claude), in any folder: each once, from the
    dream log and the mod's earlier one (one file when the two names meet). One you said no to after is not."""
    answers = [r for path in dict.fromkeys((DREAM_LOG, FORMER_DREAM_LOG)) for r in log_lines(path)
               if r.get('kind') == 'answer' and r.get('id')]
    rejected = {r['id'] for r in answers if r.get('answer') == 'rejected'}
    return len({r['id'] for r in answers if r.get('answer') in ADOPTED and since(r, start)} - rejected)


def threads_taken(start):
    """The Lost and found threads whose ask you typed into Claude's prompt since start, each once."""
    return len({r.get('key') for r in log_lines(LOST_LOG) if r.get('answer') == 'sent' and r.get('key') and since(r, start)})


LIKES = os.path.join(KEEP, 'likes.jsonl')  # {key, source, title, at, liked} a line; the last per key wins. Local only:
# nothing reads it but this pane, and a like never moves a section in the feed


def read_likes():
    """Every item you have liked or unliked: key -> (liked, when), the last line for a key winning."""
    out = {}
    for r in log_lines(LIKES):
        if isinstance(r.get('key'), str):
            try:
                out[r['key']] = (bool(r.get('liked')), float(r.get('at') or 0))
            except (TypeError, ValueError):
                continue
    return out


def keep_like(key, source, title, liked, at):
    private.append(LIKES, private.dumps(dict(key=key, source=source, title=one_line(title or '', 200), at=at, liked=liked),
                                        ensure_ascii=False) + '\n', root=KEEP)


def likes_since(likes, start):
    """The items liked since start and still liked."""
    return sum(1 for liked, at in likes.values() if liked and at >= start)


LOOKS_IN_TRANSCRIPTS = False  # a dreamer's fork writes no transcript line: its spend is added to the plan's, not found


def plan_share(used, csr_usd, local_usd, narrative_usd, looks_usd):
    """The share of the weekly limit CSR used, in points of the plan's meter: used% x CSR's spend / everything the
    week spent that this machine can see (its transcripts, CSR's own calls, which no transcript holds, and the
    dreamer's forks, which none holds either). The meter counts the plan's spend wherever it happened; the
    transcripts only this machine's Claude Code. So when the plan is also used on claude.ai or another machine, the
    sum here is short of what the meter counts, and this share overstates CSR."""
    seen = local_usd + narrative_usd + (0.0 if LOOKS_IN_TRANSCRIPTS else looks_usd)
    return used * csr_usd / seen if seen > 0 else None


def share_text(percent, bound=False):
    """'≈0.04%': under 1% one significant figure, else one decimal; '≤' for a bound, rounded up so it stays one (at
    most 0.044% is '≤0.05%', never '≤0.04%'). A share rounded into the next decade says that decade's one figure:
    0.0996 is '≈0.1%', never '≈0.10%' (10-12: the live pane said '≤0.10%'). '' for none or nothing."""
    if not percent or percent <= 0:
        return ''
    mark = '≤' if bound else '≈'
    digits = 1 if percent >= 1 else -math.floor(math.log10(percent))
    scale = 10 ** digits
    shown = (math.ceil(round(percent * scale, 6)) if bound else round(percent * scale)) / scale  # round(): 5.000000001
    if shown >= 1:
        return f'{mark}{shown:.1f}%'
    digits = -math.floor(math.log10(shown) + 1e-9)  # counted again: 0.0996 shown as 0.1 has one digit, not two
    return f'{mark}{shown:.{digits}f}%'


def week_figure(plan, narrative, lines, spend):
    """What CSR cost in the window: dict(usd, bound, share). usd is CSR's own calls' and the dreamer's together, at
    list price; bound when a look was priced at its upper bound. share is plan_share's, on a plan only and only when
    every part is known: the meter read this week, the scan caught up (spend, else None) over at least one message,
    and no model unpriced, CSR's or the transcripts'. None when CSR's cost itself has no price: never shown low."""
    looks_usd, bound = looks_cost(lines)
    narrative_usd = (narrative or {}).get('usd')
    if looks_usd is None or narrative_usd is None:
        return None
    usd, share = narrative_usd + looks_usd, None
    if plan['mode'] == 'plan' and plan.get('used') is not None and spend and spend.get('messages') \
            and not spend.get('unpriced') and usd > 0:
        share = plan_share(plan['used'], usd, spend['usd'], narrative_usd, looks_usd)
    return dict(usd=usd, bound=bound, share=share)


def week_value(plan, narrative=None, likes=None, spend=None):
    """What the value line counts, all in the plan's one window (plan_of): dict(mode, calls, looks, tokens, dreams,
    threads, likes, figure). tokens is the Haiku calls' and the looks' together, cache included in both. narrative
    is narrative_week's over the same window, read from the DB once a minute; spend the transcripts' (Spend.total),
    on a plan once caught up; the logs are the pane's own and small, read each frame. figure is week_figure's."""
    narrative, start = narrative or {}, plan['start']
    lines = dreamer_lines(start)
    return dict(mode=plan['mode'], calls=narrative.get('calls') or 0, looks=sum(1 for r in lines if called_model(r)),
                tokens=(narrative.get('tokens') or 0) + sum(look_tokens(r) for r in lines), dreams=dreams_taken(start),
                threads=threads_taken(start), likes=likes_since(likes or {}, start),
                figure=week_figure(plan, narrative, lines, spend))


# What this machine's sessions spent in the plan's week, from Claude Code's own transcripts.

SPEND_FILE = 'spend.json'  # in KEEP: where the scan stopped in each transcript, and what it found, for one week
SCAN_BUDGET = 0.1  # seconds one frame or one tick may spend reading transcripts
SCAN_LIST_EVERY = 60  # seconds between two listings of the transcripts
SPEND_EVERY = 30  # the spend file is written at most this often
SCAN_LINES = 256  # lines read between two looks at the clock
ASSISTANT = b'"assistant"'  # in every assistant line, however its JSON is spaced: a cheap test before parsing one
STAMP = b'"timestamp":"'  # a transcript line's own time is its last one: the message (and any tool input) comes before


class Spend:
    """What Claude Code spent on this machine since the plan's week began, at list price, from its transcripts
    (PROJECTS/**/*.jsonl, subagents included): each assistant line stamped at or after start, '<synthetic>' left
    out, one message per (message.id, requestId) however many lines and files carry it, each count its largest
    (a message streamed over several lines grows its output there). The transcripts of one week run to hundreds of
    MB, so they are read a little at a time, SCAN_BUDGET a call, each from where the last read stopped; that place,
    and what was found before it, is kept in KEEP/spend.json, so a restarted pane goes on from it. A file whose inode
    changed, or that got shorter, is read again from its start. ready once every file has been read to its end."""

    def __init__(self, path=None, projects=None):
        self.path, self.projects = path or os.path.join(KEEP, SPEND_FILE), projects or PROJECTS
        self.start, self.since, self.files, self.queue = None, '', {}, []
        self.listed, self.saved, self.dirty, self.ready, self.sums = None, 0.0, False, False, None

    def begin(self, start):
        """A new week: what is kept for it, if the file kept this one, else nothing."""
        self.start, self.files, self.queue, self.listed, self.ready, self.sums = start, {}, [], None, False, None
        self.since = datetime.fromtimestamp(start, tz=timezone.utc).strftime('%Y-%m-%dT%H:%M:%S')
        try:
            with open(self.path, encoding='utf-8') as handle:
                kept = json.load(handle)
            if kept.get('start') == start:
                self.files = {path: entry for path, entry in kept['files'].items() if isinstance(entry.get('msgs'), dict)}
        except (OSError, ValueError, TypeError, AttributeError, KeyError):
            self.files = {}

    def list(self):
        """The transcripts written since the week began, each queued while there is more of it to read."""
        self.queue = []
        for path in glob.glob(os.path.join(self.projects, '**', '*.jsonl'), recursive=True):
            try:
                stat = os.stat(path)
            except OSError:
                continue
            if stat.st_mtime < self.start:
                continue
            entry = self.files.get(path)
            if entry is None or entry.get('inode') != stat.st_ino or stat.st_size < entry.get('offset', 0):
                entry = self.files[path] = dict(inode=stat.st_ino, offset=0, msgs={})  # new, rotated or cut: from the top
                self.sums, self.dirty = None, True
            if stat.st_size > entry['offset']:
                self.queue.append(path)

    def take(self, entry, line):
        """One transcript line: kept when it is a priced or unpriced assistant message of this week."""
        if ASSISTANT not in line:
            return
        at = line.rfind(STAMP)  # Claude Code's compact JSON: an old line is passed over unparsed
        if at >= 0 and line[at + len(STAMP):at + len(STAMP) + 19].decode('ascii', 'replace') < self.since:
            return
        try:
            record = json.loads(line)
        except ValueError:
            return
        message = record.get('message') if isinstance(record, dict) else None
        if record.get('type') != 'assistant' or not isinstance(message, dict) or not isinstance(message.get('usage'), dict) \
                or message.get('model') == '<synthetic>' or str(record.get('timestamp') or '') < self.since:
            return
        key = f"{message.get('id')}|{record.get('requestId')}"
        counts, old = list(usage_counts(message['usage'])), entry['msgs'].get(key)
        entry['msgs'][key] = [message.get('model')] + ([max(a, b) for a, b in zip(old[1:], counts)] if old else counts)
        self.sums, self.dirty = None, True

    def read(self, path, began, budget, clock):
        """Read one transcript on from its place until its end or the budget's; True at its end. A last line still
        being written (no newline yet) waits for the next read."""
        entry = self.files[path]
        try:
            with open(path, 'rb') as handle:
                handle.seek(entry['offset'])
                for k, line in enumerate(handle):
                    if not line.endswith(b'\n'):
                        return True
                    entry['offset'] += len(line)
                    self.take(entry, line)
                    if k % SCAN_LINES == SCAN_LINES - 1 and clock() - began >= budget:
                        return False
        except OSError:
            return True
        self.dirty = True
        return True

    def step(self, start, now, budget=SCAN_BUDGET, clock=time.monotonic):
        """Read on for at most budget seconds; True once every transcript has been read to its end (ready)."""
        began = clock()
        if start != self.start:
            self.begin(start)
        if self.listed is None or now - self.listed >= SCAN_LIST_EVERY:
            self.list()
            self.listed = now
        while self.queue and clock() - began < budget:
            if self.read(self.queue[0], began, budget, clock):
                self.queue.pop(0)
        caught = not self.ready and not self.queue
        self.ready = self.ready or not self.queue
        self.save(now, force=caught)  # the moment it catches up is kept at once, for the next pane to start from
        return self.ready

    def save(self, now, force=False):
        """The spend file, when something changed: at most every SPEND_EVERY seconds, unless forced."""
        if not self.dirty or (not force and now - self.saved < SPEND_EVERY):
            return
        try:
            private.write_atomic(self.path, json.dumps(dict(start=self.start, files=self.files)), root=KEEP)
        except OSError:
            return  # a pane that cannot write still counts; it only reads again after a restart
        self.saved, self.dirty = now, False

    def total(self):
        """dict(usd, messages, unpriced): the week's spend so far, each message once across every file; usd is the
        priced ones', unpriced names the models with no price."""
        if self.sums is None:
            merged = {}
            for entry in self.files.values():
                for key, row in entry['msgs'].items():
                    old = merged.get(key)
                    merged[key] = row if old is None else [row[0]] + [max(a, b) for a, b in zip(old[1:], row[1:])]
            usd, unpriced = 0.0, set()
            for model, *counts in merged.values():
                cost = cost_of(model, *counts)
                if cost is None:
                    unpriced.add(str(model))
                else:
                    usd += cost
            self.sums = dict(usd=usd, messages=len(merged), unpriced=sorted(unpriced))
        return self.sums


def plural(n, word):
    return f"{n} {word}{'' if n == 1 else 's'}"


def money_text(usd, bound=False):
    """'$1.69' from a dollar up, '42¢' under it, '<1¢' under a cent: never $0.00. '≤' for a bound, rounded up so it
    stays one (at most $1.694 is '≤$1.70', never '≤$1.69', as share_text does); '' for none."""
    if usd is None or usd <= 0:
        return ''
    if bound:
        cents = math.ceil(round(usd * 100, 6))  # round(): 1.69 is 169.00000000000003 cents
        return f'≤${cents / 100:.2f}' if cents >= 100 else f'≤{cents}¢'
    cents = round(usd * 100)
    if cents >= 100:
        return f'${usd:.2f}'
    return '<1¢' if cents < 1 else f'{cents}¢'


# The i page reads like a review's at-a-glance box (10-12: "designed like a NYT review - make it a useful scan"):
# three sections, each a heading over a hairline, a label in plain words (never a code: Haiku, not HK) and its
# value on one line where it fits, a faint note under it only where it adds something.
SESSION, COST, WIRED = 'This session', 'What CSR cost you this week', "How it's wired"


# One row of the i page. An empty label goes under the row above it; shown_by names the instrument on the ramp that
# already says it ('prompts', 'compacts', 'context', 'cache', '7d', '5h'): the page leaves the row out while it is drawn.
Fact = namedtuple('Fact', 'section label value sev note shown_by', defaults=(None, None, None))


def fuel_text(v):
    """What CSR cost, the figure alone: '≈0.04%' of the weekly limit on a subscription, '$1.69' at list price with
    an API key, '≤' before either when it is a bound; '' when it cannot be said (a subscription's scan not caught
    up, a model unpriced)."""
    v = v or {}
    figure = v.get('figure') or {}
    if v.get('mode') == 'plan':
        return share_text(figure.get('share'), figure.get('bound'))
    return money_text(figure.get('usd'), figure.get('bound'))


def cost_piece(v):
    """What CSR cost, as the plan counts it: '≈0.04% of your weekly limit' on a subscription, '$1.69 at list
    price' with an API key; '' when it cannot be said."""
    said = fuel_text(v)
    if not said:
        return ''
    return f'{said} of your weekly limit' if v.get('mode') == 'plan' else f'{said} at list price'


def value_pieces(v):
    """The value line in words, in pieces: what CSR cost ('CSR this week: ≈0.04% of your weekly limit', or '$1.69 at
    list price'), then '77 Haiku calls', '24 dreamer calls', each left out only when it is zero; what you took, 'you
    took 5 dreams, 0 lost threads', both counts once either is not zero; then '♥ 7'. None at all when everything is
    zero. The i page says each with its code (week_rows)."""
    v = v or {}
    cost = [cost_piece(v)] if cost_piece(v) else []
    cost += [plural(v['calls'], 'Haiku call')] if v.get('calls') else []
    cost += [plural(v['looks'], 'dreamer call')] if v.get('looks') else []
    dreams, threads = v.get('dreams') or 0, v.get('threads') or 0
    took = [f"you took {plural(dreams, 'dream')}, {plural(threads, 'lost thread')}"] if dreams or threads else []
    pieces = cost + took
    if v.get('likes'):
        pieces.append(f"{HEART[1]} {v['likes']}")
    if not pieces:
        return []
    pieces[0] = 'CSR this week: ' + pieces[0]
    return pieces


def value_codes(v):
    """The value row's words and their figures, in order, each left out at zero (Fuel when it cannot be said):
    Fuel what CSR cost (fuel_text), Haiku its Haiku calls, Dreams dreamer calls that ran a model, Took dreams and
    lost threads you took, ♥ rows you liked. Plain words, never codes (10-12: "Haiku (not HK), DRM (Dream)").
    [('Fuel', '≤0.1%'), ('Haiku', '16'), ...]; [] when the week says nothing."""
    v = v or {}
    took = (v.get('dreams') or 0) + (v.get('threads') or 0)
    codes = (('Fuel', fuel_text(v)), ('Haiku', v.get('calls') or 0), ('Dreams', v.get('looks') or 0), ('Took', took),
             (HEART[1], v.get('likes') or 0))
    return [(code, str(figure)) for code, figure in codes if figure]


def value_rows(v, width):
    """The value row above the legend, one row of width: 'Fuel ≤0.1% · Haiku 16 · Dreams 8 · Took 1 · ♥ 7', each
    word in the legend's pencil, its figure in the page's ink. Too narrow, they go from the right (♥, Took, Dreams,
    then Haiku) and Fuel last, cut if it must. [] when it says nothing; the i page says it in words."""
    codes = value_codes(v)
    if not codes:
        return []
    said = lambda n: ' · '.join(f'{code} {figure}' for code, figure in codes[:n])
    n = next((n for n in range(len(codes), 0, -1) if len(said(n)) <= width - 1), 0)
    if not n:
        return [' ' + ink('faint') + fit(said(1), width - 1) + OFF]
    return [' ' + (ink('faint') + ' · ').join(ink('faint') + code + ' ' + ink('you') + figure
                                              for code, figure in codes[:n]) + OFF]


def week_rows(v):
    """The i page's "What CSR cost you" facts (Fact, section COST): each figure the value row can show, said in
    plain words, the ones at zero left out. How the cost is worked out is Fuel's note, faint under it: the 7D
    meter share on a subscription, list price with an API key. [] when the week says nothing."""
    v = v or {}
    plan, rows = v.get('mode') == 'plan', []
    cost = cost_piece(v)
    if cost:
        bound = ' · an upper bound' if ((v.get('figure') or {}).get('bound')) else ''
        how = ("7D meter × CSR cost ÷ this machine's Claude spend this plan week" if plan
               else "CSR's model calls at list price, the last 7 days") + bound
        rows.append(Fact(COST, 'Fuel', cost, None, how))
    if v.get('calls'):
        rows.append(Fact(COST, 'Haiku', plural(v['calls'], 'call') + ', for summaries'))
    if v.get('looks'):
        rows.append(Fact(COST, 'Dreams', plural(v['looks'], 'call') + ' that ran a model'))
    dreams, threads = v.get('dreams') or 0, v.get('threads') or 0
    if dreams or threads:
        rows.append(Fact(COST, 'You took', f"{plural(dreams, 'dream')}, {plural(threads, 'lost thread')}"))
    if v.get('likes'):
        rows.append(Fact(COST, 'Liked', plural(v['likes'], 'row')))
    return rows


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
            private.write_atomic(cache, json.dumps(current), root=KEEP)
        except OSError:
            pass
    return sorted(seconds)


def compaction_usual(seconds):
    """How long a compaction usually takes: dict(low, median, high, n) with the 10th and 90th percentile
    around the median; None with fewer than three to go on."""
    if len(seconds) < 3:
        return None
    return dict(low=percentile(seconds, 0.1), median=percentile(seconds, 0.5), high=percentile(seconds, 0.9), n=len(seconds))


# Your own /compact, for the card beside CSR's: compact_boundary rows of the last 30 days' transcripts with trigger
# manual and preTokens in 181k-320k (your band and the drift past it), less CSR's own (its records ±5 min: they
# read manual too). 10-12: 144 of yours, median 89 s; CSR's that day took 140 s and 91 s. A scan reads ~5 GB in ~5 s,
# so it runs in a process of its own once a day; the card only reads what it kept.
COMPACT_USUAL = os.path.join(KEEP, 'compact-usual.json')  # {at, median, n, excluded}: written by --compact-usual
USUAL_LIFE = 24 * 3600
USUAL_DAYS = 30
USUAL_BAND = (181_000, 320_000)
_USUAL_ASKED = [0.0]  # when this process last started a scan: one at a time, at most one in ten minutes


def own_compactions(projects, now, csr_times, days=USUAL_DAYS, band=USUAL_BAND):
    """Seconds each of your own /compacts took: manual, preTokens within band, in transcripts written in the last
    `days`, none within five minutes of a time in csr_times. Each file is read whole and searched in C."""
    out, since = [], now - days * 86400
    for path in glob.glob(os.path.join(projects, '*', '*.jsonl')):
        try:
            if os.path.getmtime(path) < since:
                continue
            with open(path, 'rb') as handle:
                data = handle.read()
        except OSError:
            continue
        at = data.find(b'"compact_boundary"')
        while at != -1:
            start, end = data.rfind(b'\n', 0, at) + 1, data.find(b'\n', at)
            end = len(data) if end == -1 else end
            at = data.find(b'"compact_boundary"', end)
            try:
                row = json.loads(data[start:end])
            except ValueError:
                continue
            meta = row.get('compactMetadata') if isinstance(row.get('compactMetadata'), dict) else {}
            when, pre, ms = epoch(row.get('timestamp')), meta.get('preTokens'), meta.get('durationMs')
            if row.get('subtype') != 'compact_boundary' or meta.get('trigger') != 'manual' or not when or when < since:
                continue
            if not (isinstance(pre, (int, float)) and band[0] <= pre <= band[1] and isinstance(ms, (int, float)) and ms > 0):
                continue
            if any(abs(when - t) <= 300 for t in csr_times):
                continue
            out.append(ms / 1000)
    return sorted(out)


def csr_compaction_times(logs=None):
    """When CSR's own compactions ended, from the dream logs' compact records."""
    times = set()
    for log in logs or (DREAM_LOG, FORMER_DREAM_LOG):
        try:
            with open(log, encoding='utf-8') as handle:
                for line in handle:
                    if '"compact"' not in line:
                        continue
                    try:
                        record = json.loads(line)
                    except ValueError:
                        continue
                    if record.get('kind') == 'compact' and record.get('event') == 'compact' and \
                            isinstance(record.get('at'), (int, float)):
                        times.add(float(record['at']))
        except OSError:
            continue
    return sorted(times)


def keep_compact_usual(now, projects=None, path=None, logs=None):
    """Scan and keep how long your own /compact takes: dict(at, median, n, excluded: CSR's left out), median
    None under three."""
    csr = csr_compaction_times(logs)
    seconds = own_compactions(projects or PROJECTS, now, csr)
    kept = dict(at=now, median=percentile(seconds, 0.5) if len(seconds) >= 3 else None, n=len(seconds), excluded=len(csr))
    path = path or COMPACT_USUAL
    try:
        private.write_atomic(path, json.dumps(kept), root=KEEP)
    except OSError:
        pass
    return kept


def compact_usual(now, path=None, refresh=True):
    """How long your own /compact takes, as last kept: dict(median, n) or None. Never scans here: a day-old or
    missing file starts --compact-usual in a process of its own, and the card has the answer next time."""
    try:
        with open(path or COMPACT_USUAL, encoding='utf-8') as handle:
            kept = json.load(handle)
        at = float(kept.get('at') or 0)
    except (OSError, ValueError, TypeError, AttributeError):
        kept, at = None, 0.0
    if refresh and now - at > USUAL_LIFE and now - _USUAL_ASKED[0] > 600:
        _USUAL_ASKED[0] = now
        try:
            subprocess.Popen([sys.executable, '-I', os.path.abspath(__file__), '--repo', HOME or '/', '--compact-usual'],
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             start_new_session=True)
        except OSError:
            pass
    if not kept or not isinstance(kept.get('median'), (int, float)):
        return None
    return dict(median=float(kept['median']), n=int(kept.get('n') or 0))


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
    """The page with the compaction in its head, for the masthead to draw: the timer takes row 2."""
    info = data.get('compacting')
    page['head']['compacting'] = info
    page['head']['infinite'] = data.get('infinite')  # the mod's plan and what CSR's compactions did
    gauge = page['head'].get('gauges')
    if gauge and info:
        page['head']['gauges'] = dict(gauge, compacting=info,
                                      payback=dict(zip(('text', 'sev'), compacting_slot(info))))
    return page


# ---------------------------------------------------------------- compact at the right moment (the mod's plan)

COMPACT_PLANS = os.path.join(KEEP, 'compact-plan')  # <session>.json: {at, state, text, ends_at?}, written by the mod
PLAN_LIFE = 2 * 3600  # a plan this old is from a session that went quiet: not drawn
COUNTING = re.compile(r'^Compacting in \d+s')


def compact_plan_of(session_id, now, folder=None):
    """The mod's plan for this session's next compaction, dict(state, text), or None: none written, taken down
    (state none), or two hours old. A countdown's seconds are worked out here, from when it ends."""
    try:
        with open(kept_file(folder or COMPACT_PLANS, session_id) or '', encoding='utf-8') as handle:
            plan = json.load(handle)
        at, state, text = float(plan.get('at') or 0), str(plan.get('state') or ''), plan.get('text')
    except (OSError, ValueError, TypeError, AttributeError):
        return None
    if state == 'none' or not isinstance(text, str) or not text.strip() or now - at > PLAN_LIFE:
        return None
    ends = plan.get('ends_at')
    if state == 'countdown' and isinstance(ends, (int, float)) and not isinstance(ends, bool):
        text = COUNTING.sub(f'Compacting in {max(0, math.ceil(ends - now))}s', text)
    return dict(state=state, text=one_line(text, 200))


def compact_records(session_id, path=None):
    """The mod's records of kind compact for this session, oldest first: each plan, countdown, cancel,
    compaction and skip it logged."""
    try:
        with open(path or DREAM_LOG, encoding='utf-8') as handle:
            lines = handle.read().splitlines()
    except OSError:
        return []
    out = []
    for line in lines:
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if isinstance(record, dict) and record.get('kind') == 'compact' and record.get('session') == session_id:
            out.append(record)
    return sorted(out, key=lambda r: r.get('at') if isinstance(r.get('at'), (int, float)) else 0)


PRE_REQUESTS = 12  # the requests before a compaction its card's picture draws, climbing to it


def compaction_realized(record, main, compacts, summaries=None):
    """What one of CSR's compactions has paid so far, from the main-chain requests after it and before the next
    compaction: dict(before, floor, summary, cost, saving, requests, net, even_at, even_request, series, start
    (its boundary), pre (the last PRE_REQUESTS before it, since the compaction before)), or None before its
    first request. The floor is that first request's context; the summary its own size (the mod's ctx_after,
    the boundary's postTokens), else the session's median; each request saved request_saving against not
    compacting; even_at, the request whose saving passed the cost.

    10-12 20:55: 278,129 before, 70,300 on the first request, a 21,730 summary: cost 277,063, saving 20,783 a
    request, even at the 14th (13:57), +2.0M by the 110th."""
    number = lambda v: isinstance(v, (int, float)) and not isinstance(v, bool)
    at, before = record.get('at'), record.get('ctx_before')
    if not number(at) or not number(before):
        return None
    bounds = sorted(compacts or ())
    own = [b for b in bounds if at - 600 <= b <= at + 5]  # its own boundary is written just before the record
    start = own[-1] if own else at
    later = [b for b in bounds if b > at + 5]
    until = later[0] if later else float('inf')
    ordered = sorted(main or ())
    series = [(t, c) for t, c in ordered if start < t < until and c]
    if not series:
        return None
    earlier = [b for b in bounds if b < start - 5]
    pre = [(t, c) for t, c in ordered if (earlier[-1] if earlier else 0) < t <= start and c][-PRE_REQUESTS:]
    floor = series[0][1]
    summary = record['ctx_after'] if number(record.get('ctx_after')) and record['ctx_after'] > 0 \
        else summary_of(summaries)[0]
    cost, saving = compaction_cost(floor, before, summary), request_saving(floor, before)
    even = next((i for i in range(len(series)) if saving > 0 and (i + 1) * saving >= cost), None)
    return dict(before=before, floor=floor, summary=summary, cost=cost, saving=saving, requests=len(series),
                net=len(series) * saving - cost, even_at=series[even][0] if even is not None else None,
                even_request=even + 1 if even is not None else None, series=series, start=start, pre=pre)


def compact_story(records, compacts, sizes, main, context, summaries=None):
    """What CSR's compactions did in this session, for the infinite-session row: dict(text, by_csr, by_you,
    payback), or None before CSR has compacted here. The payback so far, in input tokens at the full price:
    each compaction's net (compaction_realized) over the requests after it and before the next."""
    number = lambda v: isinstance(v, (int, float)) and not isinstance(v, bool)
    done = sorted((r for r in records or () if r.get('event') == 'compact' and r.get('outcome') == 'compacted'
                   and number(r.get('at')) and number(r.get('ctx_before'))), key=lambda r: r['at'])
    if not done:
        return None
    net, floor = 0.0, floor_of(sizes)[0]
    for record in done:
        paid = compaction_realized(record, main, compacts, summaries)
        summary = record['ctx_after'] if number(record.get('ctx_after')) and record['ctx_after'] > 0 \
            else summary_of(summaries)[0]
        net += paid['net'] if paid else -compaction_cost(floor, record['ctx_before'], summary)  # spent, nothing back yet
    by_csr = len(done)
    by_you = max(0, len(compacts or ()) - by_csr)
    text = ' · '.join(x for x in (
        f"this session: {by_csr} {'compaction' if by_csr == 1 else 'compactions'} by CSR", f'{by_you} by you',
        f'context {short(context)}' if context else '',
        f'net saving so far ~{tokens_said(net)} tokens' if net >= 0 else f'not yet paid back ~{short(-net)} tokens') if x)
    return dict(text=text, by_csr=by_csr, by_you=by_you, payback=int(net))


# ---------------------------------------------------------------- the card of CSR's own compaction

CARD_LIFE = 30 * 60  # a compaction's card stays this long after it ends; the story row tells it after
CARD_EVENTS = ('countdown', 'start', 'compact', 'skip', 'cancel')
LEARNT = ('Errors and fixes',)  # the summary sections that say what the window learnt


def learnt_items(text, most=4):
    """What a compaction summary says its window learnt: the heads of its Errors and fixes, the person's
    corrections among them."""
    out = []
    for name in LEARNT:
        for head, nested in section_items(summary_section(text, name)):
            if head.endswith(':') and nested:  # a label: what it names is under it
                head = f'{head} {nested[0]}'
            if head:
                out.append(head)
    return out[:most]


def compaction_card(records, summaries, now, main=None, compacts=None, summary_sizes=None):
    """CSR's newest compaction in this session as the pane's card: dict(stage, ...) or None. Streamed from the
    mod's records as it writes them: soon (counting down), running (the summary runs, with what it keeps),
    done (the sizes, what it kept, what the window learnt: the new summary's Errors and fixes, and what it has
    paid so far: compaction_realized over main), or skipped.
    A cancel takes it down; a done or skipped one goes CARD_LIFE after it ended."""
    number = lambda v: isinstance(v, (int, float)) and not isinstance(v, bool)
    episode = [r for r in records or () if r.get('event') in CARD_EVENTS and number(r.get('at'))]
    if not episode:
        return None
    last = episode[-1]
    at, event = last['at'], last['event']
    seam = last.get('seam') if isinstance(last.get('seam'), dict) else {}
    keep = last.get('keep') if isinstance(last.get('keep'), dict) else {}
    rows = lambda name: [one_line(str(x), 300) for x in keep.get(name) or () if str(x).strip()]
    card = dict(at=at, why=one_line(str(last.get('why') or seam.get('why') or ''), 120),
                before=last['ctx_before'] if number(last.get('ctx_before')) else None,
                keeping=rows('running') + rows('decisions') + rows('work') + rows('rules'))
    if event == 'countdown':
        ends = last.get('ends_at')
        if not number(ends) or ends < now - 5:  # a countdown nothing followed: the mod reloaded under it
            return None
        return dict(card, stage='soon', left=max(0, math.ceil(ends - now)))
    if event == 'start':
        return dict(card, stage='running', elapsed=int(max(0, now - at))) if now - at < MARKER_LIFE else None
    if event == 'cancel' or now - at > CARD_LIFE:
        return None
    if event == 'skip':
        return dict(card, stage='skipped', reason=one_line(str(last.get('reason') or 'a hook said no'), 200))
    took = last['took_ms'] / 1000 if number(last.get('took_ms')) else None
    began = at - (took or 0)
    summary = next((text for when, text in reversed(summaries or ()) if began - 30 <= when <= at + 300), None)
    paid = compaction_realized(last, main, compacts, summary_sizes)
    after = paid['floor'] if paid else last.get('floor') if number(last.get('floor')) else None
    return dict(card, stage='done', after=after if number(after) else None, measured=bool(paid), took=took,
                summary=last['ctx_after'] if number(last.get('ctx_after')) and last['ctx_after'] > 0 else None,
                payback=last['payback'] if number(last.get('payback')) else None,
                per_turn=last['per_turn'] if number(last.get('per_turn')) else None, paid=paid, usual=None,
                learnt=learnt_items(summary) if summary else [])


CARD_COLS = 48  # the compaction card's rows fit this many columns at their longest numbers: none wraps or is cut


def tokens_said(n):
    """'2.0M', '276k': a payback's tokens, as the status line's story says them."""
    return f'{n / 1e6:.1f}M' if n >= 1e6 else short(n)


def card_payback(card):
    """The card's payback as one set of numbers, so its rows and its picture agree: dict(before, floor, summary,
    cost, saving, need, done, net, earned, measured), in input tokens at the full price. From the requests since
    (compaction_realized: net is their saving less the cost, earned the cost plus net) or, before the first, from
    the mod's record (its floor the plan's guess, nothing earned); need, the call that pays it back (None when it
    never would). None when the sizes are unknown. 10-12 15:46: 274,073 → 70,026, a 20,881 summary: cost 271,864,
    20,405 a call, 6 calls in, 149k not yet paid back (the mockup's 276k used a guessed summary)."""
    number = lambda v: isinstance(v, (int, float)) and not isinstance(v, bool)
    paid = card.get('paid')
    if paid:
        before, floor, summary = paid['before'], paid['floor'], paid['summary']
        cost, saving, done, net = paid['cost'], paid['saving'], paid['requests'], paid['net']
    else:
        before, floor = card.get('before'), card.get('after')
        if not number(before) or not number(floor):
            return None
        summary = card['summary'] if number(card.get('summary')) else SUMMARY_GUESS
        cost, saving, done = compaction_cost(floor, before, summary), request_saving(floor, before), 0
        net = -cost
    need = math.ceil(cost / saving) if saving > 0 else None
    return dict(before=before, floor=floor, summary=summary, cost=cost, saving=saving, need=need, done=done,
                net=net, earned=cost + net, measured=bool(paid))


def payback_rows(money):
    """The card's two payback rows, a fact a sentence, by its state: just compacted, the one-time cost and the
    call that pays it back; paying back, the calls done and to go and what is earned back; paid back, where it did
    and the net saving. One that never pays back says so. 10-12: "2 of 14 requests to pay back, no clue still. 231k
    still to recoup etc. doesnt make sense"."""
    if not money:
        return []
    if not money['need']:
        return ['This compaction does not pay back.', 'The context did not get smaller.']
    cost, need, done = short(money['cost']), money['need'], money['done']
    if done == 0:
        return [f'One-time cost: {cost} tokens at full input price.',
                f"Each call saves {short(money['saving'])}. Paid back at call {need}."]
    if done < need:
        left = need - done
        return [f"Paid back: {done} of {need} calls. {left} {'call' if left == 1 else 'calls'} to go.",
                f"Earned back: {short(money['earned'])} of {cost} (full input price)."]
    return [f'Paid back at call {need}. Now at call {done}.',
            f"Net saving: {tokens_said(money['net'])} tokens at full input price."]


def took_row(took, usual):
    """'Took 1:29. Your /compact: 1:27 (median of 136).': how long it took beside how long yours take (the same
    summary call; CSR's keep list the difference), 'of 136' gone when the row would pass CARD_COLS."""
    if not took:
        return ''
    said = f'Took {clock_text(took)}.'
    if not usual:
        return said
    yours = f"{said} Your /compact: {clock_text(usual['median'])}"
    full = f"{yours} (median of {usual['n']})."
    return full if len(full) <= CARD_COLS else f'{yours} (median).'


def kept_row(kept, learnt):
    """'Kept 2 items, 2 lessons. Click to read.': what the summary carried, counted; a click shows it whole.
    '' when it carried nothing."""
    halves = [f"{n} {word}{'' if n == 1 else 's'}" for n, word in ((kept, 'item'), (learnt, 'lesson')) if n]
    return f"Kept {', '.join(halves)}. Click to read." if halves else ''


def lost_waiting(threads):
    """How many threads Lost and found shows waiting on you: not set aside, lost or open (lost_sections' rows), and
    waiting. All of them, not its top three. compact.ts's waitingOn counts pane.py --lost the same way."""
    return sum(1 for t in threads or () if not t.get('hidden') and t.get('rank', 9) < 3 and t.get('waiting'))


def lost_row(waiting):
    """'2 waiting in Lost and found.': the card says what the notice on the phone says. '' when none wait."""
    return f'{waiting} waiting in Lost and found.' if waiting else ''


def compact_section(card, compacting=None, waiting=0):
    """The card as a section of the page: its stage in the heading, the sizes or the reason on the title row,
    what it learnt and kept under it, and once done how many threads wait on you in Lost and found (waiting,
    lost_waiting's count, live); a click hands Claude all of it."""
    if not card:
        return None
    stage, keeping = card['stage'], card['keeping']
    size = short(card['before']) if card.get('before') else ''
    picture = None
    if stage == 'soon':
        title, right, body, detail = ' · '.join(x for x in (size, card['why']) if x), f"in {card['left']}s", [], []
        heading = 'Compacting soon'
    elif stage == 'running':
        usual = (compacting or {}).get('usual')
        right = ' · '.join(x for x in (clock_text((compacting or {}).get('elapsed', card['elapsed'])),
                                       usual and f"usually {clock_text(usual['low'])}–{clock_text(usual['high'])}") if x)
        title = ' · '.join(x for x in (f'Summarizing {size}' if size else 'Summarizing', card['why']) if x)
        body, detail, heading = [f'Keeping: {x}' for x in keeping[:3]], [f'Keeping: {x}' for x in keeping[3:]], 'Compacting'
    elif stage == 'skipped':
        title, right, body, detail, heading = f"Did not run: {card['reason']}", '', [], [], 'Compaction skipped'
    else:
        usual, money = card.get('usual'), card_payback(card)
        after = card.get('after') and ('' if card.get('measured') else '~') + short(card['after'])
        title = f'Context: {size} → {after} tokens.' if size and after else f'Context: {size} tokens.' if size else ''
        lede = [f"Each API call now sends {short(money['before'] - money['floor'])} fewer tokens."] \
            if money and money['need'] else []
        told = payback_rows(money)
        picture = dict(kind='payback', rows=PAYBACK_ROWS, facts=dict(
            at=card['at'], per=card.get('per_turn'), **{k: money[k] for k in ('cost', 'saving', 'need', 'done', 'net')})) \
            if money and money['need'] else None
        body = told + [x for x in (took_row(card.get('took'), usual), kept_row(len(keeping), len(card['learnt'])),
                                   lost_row(waiting)) if x]
        detail = [f'Kept: {x}' for x in keeping] + [f'Learnt: {x}' for x in card['learnt']] + ([
            f"Cost: cache {short(money['floor'] * 2)} + summary {short(money['summary'] * 5)} + read "
            f"{short(money['before'] * 0.1)}."] if money and money['need'] else [])
        right, heading = datetime.fromtimestamp(card['at']).strftime('%H:%M'), 'Compacted by CSR'
        brief = told[0] if told else title
    said = '; '.join((lede if stage == 'done' else []) + body + detail)
    item = dict(key=f"compacted:{card['at']}", title=title or heading, lines=2, plain_body=True, body=body,
                lede=lede if stage == 'done' else [], loud=len(told) if stage == 'done' else 0, whole_detail=stage == 'done',
                picture=picture if stage == 'done' else None,
                unrated=stage in ('soon', 'running'),  # nothing to like until it has run
                detail=detail, send=closed_send(f"CSR's compaction card ({heading.lower()}), quoted:",
                                                f"{title.rstrip('.')}." + (f' {said}.' if said else ''), CARD_SEND_MOST))
    return dict(name='compacted', title=heading, right=right, items=[item], empty='', at=card['at'],
                live=stage in ('soon', 'running'), brief=brief if stage == 'done' else title)


def compact_plan_json(session_id, context, log=None):
    """For the mod at a turn end in the band: this session's floor and summary size, the payback of compacting
    now (payback_requests: the requests, None when it would never pay back), the session's requests a turn, and
    whether the payback is within PAYBACK_UNDER turns, the context's trend, and the infinite-session story. Read
    from the transcript alone: no csr-engine, no model."""
    session = Session(session_id) if session_id and re.fullmatch(r'[\w-]+', session_id) else None
    if session:
        session.read_transcript()
    main, prompts = (session.main, session.prompts) if session else ([], [])
    compacts, sizes = (session.compacts, session.compact_sizes) if session else ([], [])
    summaries = session.summary_sizes if session else []
    floor, measured = floor_of(sizes)
    summary = summary_of(summaries)[0]
    requests = payback_requests(floor, context, summary) if context else None
    per_turn = requests_per_turn(main, prompts)
    turns = payback_turns(requests, per_turn) if requests is not None else None
    ends = turn_ends(main, prompts)
    return dict(ctx=context, floor=floor, measured=measured, summary=summary,
                payback=round(requests, 1) if requests is not None else None,
                per_turn=round(per_turn, 1) if per_turn else None,
                worth=turns is not None and turns <= PAYBACK_UNDER, trend=ctx_trend(main, prompts, compacts, ends),
                story=compact_story(compact_records(session_id, log), compacts, sizes, main, context, summaries))


def infinite_info(session_id, now, s=None, folder=None, log=None):
    """The infinite-session rows for the masthead, dict(plan, state, story, card, tally, csr_at), each a text
    (tally: the story's counts and payback, for the i page; csr_at: the compact boundaries that were CSR's, for its
    context figure) or None; None when there is neither a plan nor a compaction of CSR's to tell."""
    plan = compact_plan_of(session_id, now, folder)
    story = card = None
    if s:
        main, records = s.get('main') or [], compact_records(session_id, log)
        story = compact_story(records, s.get('compacts'), s.get('compact_sizes'), main,
                              max(main)[1] if main else None, s.get('summary_sizes'))
        card = compaction_card(records, s.get('summaries'), now, main, s.get('compacts'), s.get('summary_sizes'))
        if card and card['stage'] == 'done' and card.get('took'):
            card['usual'] = compact_usual(now)  # how long your own /compact takes, as last kept (never scanned here)
    if not plan and not story and not card:
        return None
    return dict(plan=plan and plan['text'], state=plan and plan['state'], story=story and story['text'], card=card,
                tally=story and {k: story[k] for k in ('by_csr', 'by_you', 'payback')},
                csr_at=csr_boundaries(records, s.get('compacts')) if s else [])


def csr_boundaries(records, compacts):
    """The compact boundaries CSR's compactions wrote, oldest first: for each compacted record, the newest
    boundary in the ten minutes before it (as compaction_realized finds its own)."""
    bounds, out = sorted(compacts or ()), set()
    for r in records or ():
        at = r.get('at')
        if r.get('event') == 'compact' and r.get('outcome') == 'compacted' and isinstance(at, (int, float)):
            own = [b for b in bounds if at - 600 <= b <= at + 5]
            out.update(own[-1:])
    return sorted(out)


def piece_rows(text, width, most=3):
    """text's ' · '-joined pieces packed into rows of width: a piece goes whole onto the row it fits, else starts the
    next (one wider than a row wraps by its words). At most `most` rows; only past them is the last one cut."""
    rows = []
    for piece in text.split(' · '):
        if rows and len(rows[-1]) + 3 + len(piece) <= width:
            rows[-1] += ' · ' + piece
        else:
            rows += wrap(piece, width, most)
    if len(rows) > most:
        rows = rows[:most - 1] + [fit(rows[most - 1] + ' …', width)]
    return rows


def infinite_rows(info, width):
    """The plan (bold) and the story (dim) under the masthead's gauges; nothing when there is neither. The plan
    wraps by its pieces, so its payback is never cut (10-12: "· pays back i…" at 61 columns)."""
    if not info:
        return []
    rows = [' ' + BOLD + row + OFF for row in piece_rows(info['plan'], width - 1)] if info.get('plan') else []
    return rows + ([' ' + DIM + fit(info['story'], width - 1) + OFF] if info.get('story') else [])


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
                 ('later', 'Set aside for later', 'later'), ('handed', 'Handed off', 'handed'), ('drop', 'Dropped', 'dropped'))


def thread_group(t):
    if t['hidden']:
        return t['hidden']
    if t['state'] in ('lost', 'open') and t['waiting']:
        return 'waiting'
    return t['state']


def draw_threads(data, s, cols, rows, ui, top=0):
    """The f page: every thread Lost and found knows, grouped, nothing hidden; each says why it sits where
    it does. A click opens one and shows its answers, as on the main page."""
    width, at = max(24, cols - 1), gutter(ui)
    every = (s or {}).get('lost') or []
    groups = {name: [t for t in every if thread_group(t) == name] for name, *_ in THREAD_GROUPS}
    facts = f"{len(every)} threads: " + ' · '.join(f"{len(groups[name])} {short}" for name, _, short in THREAD_GROUPS
                                                   if groups[name]) if every else 'No compaction summary yet.'
    head = [' ' + BOLD + fit('Lost and found · every thread', width - 1) + OFF] + [' ' + line for line in wrap(facts, width - 1, 3)]
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
            act = dict(key=item['key'], send=asks['side'], lost=t['key'])
            for line in wrap(t['text'], width - at, 3):
                body.append((' ' * at + ink('you') + line + OFF, act))
            body.append((' ' * at + ink('faint') + fit(t.get('why') or '', width - at) + OFF, act))
            if ui.get('open') == item['key']:
                for line in t['detail'][:3]:
                    body.append((' ' * at + ink('quiet') + fit(line, width - at) + OFF, act))
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


PAPER_INK = dict(normal='you', data='you', advisory='ctx', amber='dream', red='turn')  # SEV, on the paper


def reset_text(resets, now):
    try:
        return reset_label(resets, now)
    except (OverflowError, OSError, ValueError):
        return ''


def info_rows(data, s, now, width=60):
    """The i page's facts, [Fact], section by section, the most useful first. This session: a compaction under way
    (its facts under it), the context, prompts, compactions (CSR's story of them when there is one), the compact
    plan, the cache and the two limits, each marked with the instrument that already says it. What CSR cost you
    this week (week_rows). How it's wired: the model, Claude's own status line, the daemon, the recap, and any
    CSR_NO_* switch set. Values are laid for a column `width` cells wide; Model and Recap always show."""
    data, s = data or {}, s or {}
    g = gauges(data.get('claude_fields'), data.get('claude_at'), s, now, data.get('limits'), data.get('sts'),
               data.get('compacting'))
    rows = []
    if data.get('compacting'):
        lines = [COLOUR.sub('', row).strip() for row in compact_rows(data['compacting'], width)]
        rows += [Fact(SESSION, 'Compacting', lines[0], g['payback']['sev'])] + [Fact(SESSION, '', line) for line in lines[1:]]
    ctx, main = g['ctx'], s.get('main') or []
    if ctx['tokens']:
        measured = len([r for r in s.get('compact_sizes') or () if isinstance(r[1], (int, float)) and r[1] > 0])
        rows.append(Fact(SESSION, 'Context', ' · '.join(x for x in (
            short(ctx['tokens']) + (f" of {short(ctx['size'])}" if ctx['size'] else ''),
            f'peak {short(max(c for _, c in main))}' if main else '',
            f"floor {short(g['floor']['tokens'])} " + (f'(last {min(FLOOR_ROWS, measured)})' if g['floor']['measured'] else '(guess)'))
            if x), ctx['sev'], shown_by='context'))
    if s.get('prompts'):
        clock = Clock(s['prompts'] + [r[0] for r in s.get('replies') or ()], now)
        rows.append(Fact(SESSION, 'Prompts', f"{len(s['prompts'])} · {span(clock.total)} active", shown_by='prompts'))
    lasting = data.get('infinite') or {}
    tally = lasting.get('tally')
    if tally:  # who compacted, and what CSR's compactions paid back: no instrument says it
        paid = tally['payback']
        rows.append(Fact(SESSION, 'Compactions', f"{tally['by_csr']} by CSR, " + (  # the saving is CSR's alone
            f'~{tokens_said(paid)} saved' if paid >= 0 else f'~{short(-paid)} not yet paid back') + f" · {tally['by_you']} by you",
            note='net saving, tokens at full input price, against not compacting'))
    elif lasting.get('story'):
        rows.append(Fact(SESSION, 'Compactions', re.sub(r'^this session: ', '', lasting['story'])))
    elif s.get('compacts'):
        payback = '' if data.get('compacting') or not g['payback']['text'] else f" · {g['payback']['text']}"
        rows.append(Fact(SESSION, 'Compactions', f"{len(s['compacts'])}" + payback, shown_by='compacts'))
    if lasting.get('plan'):  # its label says what it is: the plan's own first words go
        rows.append(Fact(SESSION, 'Compact plan', re.sub(r'^Compact planned · ', '', lasting['plan'])))
    if g['cache']['text'] != 'CACHE --':
        rows.append(Fact(SESSION, 'Cache', g['cache']['long'], g['cache']['sev'], shown_by='cache'))
    for q in g['quotas']:
        label = {'7D': 'Weekly limit', '5H': '5-hour limit'}.get(q['code'], f"{q['code']} limit")
        if q['stale']:
            rows.append(Fact(SESSION, label, '-- · its window rolled over', q['sev']))
        elif q['used'] is not None:
            when = reset_text(q['resets'], now) if q['resets'] else ''
            rows.append(Fact(SESSION, label, f"{round(q['used'])}%" + (f' ↻ {when}' if when else ''), q['sev'],
                             shown_by=q['code'].lower()))
    rows += week_rows(data.get('value'))
    rows.append(Fact(WIRED, 'Model', one_line(s.get('model') or '--', 60)))
    if data.get('claude_line'):
        rows.append(Fact(WIRED, 'Status line', data['claude_line']))
    inop = g['sts'].get('inop') or []
    daemon = g['sts'].get('daemon')
    if daemon and daemon.get('up'):
        since = f" since {datetime.fromtimestamp(daemon['since']).strftime('%H:%M')}" if daemon.get('since') else ''
        rows.append(Fact(WIRED, 'Daemon', f"up{since} · pid {daemon['pid']}"))
    rows += [Fact(WIRED, 'Daemon', value.replace('DOWN', 'down', 1), sev) for label, value, sev in inop if label == 'DAEMON']
    told = s.get('told')
    if told:
        rows.append(Fact(WIRED, 'Recap', f"injected {datetime.fromtimestamp(told['at']).strftime('%H:%M')} ({told['source']})"))
    elif not s.get('transcript', bool(s.get('prompts') or s.get('replies'))):
        rows.append(Fact(WIRED, 'Recap', 'after your first prompt'))
    else:
        rows.append(Fact(WIRED, 'Recap', 'none this session'))
    switches = [(label, value, sev) for label, value, sev in inop if label != 'DAEMON' and label not in QUIET_SWITCHES]
    if switches:  # CSR_NO_* set: what it runs without, in words, and where each is set under it
        where = {'daemon': "the daemon's LaunchAgent", 'hooks': "settings.json's env"}
        names = [label.lower().replace('_', ' ') for label, _, _ in switches]
        notes = []
        for label, value, _ in switches:
            found = re.search(r'\((\w+)\)$', value)
            notes.append(f"CSR_NO_{label} in {where.get(found.group(1), found.group(1)) if found else 'its env'}")
        rows.append(Fact(WIRED, 'Switched off', ', '.join(names), max((sev for _, _, sev in switches),
                                                                       key=('advisory', 'amber').index), '; '.join(notes)))
    return rows


INFO_LABEL = 15  # the i page's label column: a margin, the longest label ('Weekly limit', 'Switched off'), two cells


def info_heading(section, width, first=False):
    """A section's head: a hairline the width of the page over its name in bold capitals; the first section's
    rule is the ramp's own edge, a blank row in its place."""
    return ['' if first else ' ' + ink('faint') + '─' * max(1, width - 1) + OFF,
            ' ' + BOLD + ink('you') + fit(section.upper(), width - 1) + OFF]


def info_line(fact, width):
    """One fact as rows: its label in pencil in the label column, its value in ink on one line where it fits
    (wrapped under itself to three rows where it does not; Claude's status line cut, its own colours kept), its
    note faint under the value, two rows at most. An empty label sets the value under the one above."""
    room = max(4, width - INFO_LABEL)
    colour = ink(PAPER_INK[fact.sev]) if fact.sev else ink('you')
    indent = ' ' * INFO_LABEL
    head = ' ' + ink('quiet') + fit(fact.label, INFO_LABEL - 3).ljust(INFO_LABEL - 1) + OFF if fact.label else indent
    if fact.label == 'Status line':
        rows = [head + ansi_fit(fact.value, room) + OFF]
    else:
        pieces = wrap(one_line(fact.value, 400), room, 3)
        rows = [head + colour + pieces[0] + OFF] + [indent + colour + piece + OFF for piece in pieces[1:]]
    if fact.note:
        rows += [indent + ink('faint') + piece + OFF for piece in wrap(one_line(fact.note, 400), room, 2)]
    return rows


def info_sections(facts, width, shown=(), lanes=None):
    """The page under the ramp: each section that has a fact (or, for This session, the lanes) as its heading and
    its rows, a blank row between sections. A fact an instrument shows is left out. lanes: (rows, pictures)
    laid under This session's first facts (a compaction under way leads), pictures by their row in lanes' rows.
    (Lines, pictures)."""
    out, pictures = Lines(), []
    for section in (SESSION, COST, WIRED):
        rows = [f for f in facts if f.section == section and not (f.shown_by and f.shown_by in shown)]
        lead = 0  # a compaction under way and the rows under it: above the lanes
        while lead < len(rows) and (rows[lead].label == 'Compacting' or (lead and rows[lead].label == '')):
            lead += 1
        has_lanes = section == SESSION and lanes and lanes[0]
        if not rows and not has_lanes:
            continue
        out.put(info_heading(section, width, first=not out))
        for fact in rows[:lead]:
            out.put(info_line(fact, width))
        if has_lanes:
            pictures += [(r + len(out), c, w, h, png) for r, c, w, h, png in lanes[1]]
            out.put(lanes[0])
        for fact in rows[lead:]:
            out.put(info_line(fact, width))
    return out, pictures


EMBLEM = 6  # the i page's first columns on its title row and the row under it: the CSR emblem's slot (5 x 2 cells)
PANEL_ROWS = 5  # the i page's session chart, and its tapes, this many rows tall
SLOT = 4  # one tape's columns on the i page: the tape two cells wide, its value and label under it
PANEL_TAPES = ('7d', '5h', 'cache')
PANEL_LABEL = {'7d': '7D', '5h': '5H', 'cache': '◷'}
CHART_MIN = 10  # fewer columns than this for the chart and its tapes go; fewer still, no chart


def info_title(s, now):
    """The i page's title: whose session, and since when. Claude Code's own title for it is left out."""
    stamps = sorted([t for t in s.get('prompts') or () if t] + [r[0] for r in s.get('replies') or () if r and r[0]]
                    + [r[0] for r in s.get('main') or () if r and r[0]])  # as the chart's axis begins
    since = datetime.fromtimestamp(stamps[0]).strftime('%a %H:%M') if stamps else ''
    return ' · '.join(x for x in ('Session info', (s.get('id') or '')[:8], since and f'since {since}') if x)


def chart_cells(peaks, floor, height):
    """A session's context as height rows (top first) of one cell a slice: each slice's highest context in
    eighths over 0..CTX_RED, in its heat, the part under the floor dimmed - a cell the floor crosses is
    two-toned, the dim block in front, the bright part as its background. A slice with no reply is blank."""
    total = height * 8
    rows = [[] for _ in range(height)]
    f = round(max(0.0, min(1.0, floor / CTX_RED)) * total) if isinstance(floor, (int, float)) and floor == floor else 0
    for v in peaks:
        if not v:
            for row in rows:
                row.append((' ', None, None))
            continue
        h = max(1, min(total, round(min(1.0, v / CTX_RED) * total)))
        bright = heat(v)
        dim = blend(bright, DARK[1], 0.55)
        for row in range(height):
            base = 8 * (height - 1 - row)
            fill, under = max(0, min(8, h - base)), max(0, min(8, f - base))
            if not fill:
                rows[row].append((' ', None, None))
            elif under >= fill:
                rows[row].append((EIGHTHS[fill], dim, None))
            elif not under:
                rows[row].append((EIGHTHS[fill], bright, None))
            elif fill == 8:
                rows[row].append((EIGHTHS[under], dim, bright))
            else:
                rows[row].append((EIGHTHS[fill], bright, None))
    return rows


_SERIES = {}  # the i page's last chart: its key, and (peaks, ticks, clock)


def session_series(s, n, now):
    """(peaks, ticks, clock) for an n-slice chart of the session, its time squeezed of idle gaps as the lanes
    draw it: the highest main-chain context in each slice, the slices a compaction fell in. None with no
    context to draw."""
    main = [(t, c) for t, c in s.get('main') or () if t and c]
    if not main or n < 1:
        return None
    key = (s.get('id'), len(main), main[-1], len(s.get('prompts') or ()), len(s.get('replies') or ()),
           len(s.get('compacts') or ()), n, int(now // 60))
    if _SERIES.get('key') == key:  # no new reply and the same minute: the same chart
        return _SERIES['series']
    clock = Clock(list(s.get('prompts') or ()) + [r[0] for r in s.get('replies') or ()] + [t for t, _ in main], now)
    at = lambda t: min(n - 1, int(clock.at(t) * n))
    peaks = [0] * n
    for t, c in main:
        peaks[at(t)] = max(peaks[at(t)], c)
    _SERIES.update(key=key, series=(peaks, {at(t) for t in s.get('compacts') or ()}, clock))
    return _SERIES['series']


def tape_pair(cell, ground):
    """A tape cell two columns wide for the i page: the rail as a thin line at its middle."""
    glyph, ink, back = cell
    if ink == 'rail':
        ink = blend(ground[:3], ON_DARK['text'], 0.24)
        return bg_of(ground) + fg_of(ink) + '▕▏'
    return bg_of(back or ground) + (fg_of(ink) if ink else '') + glyph * 2


def panel_value(kind, g):
    """(text, sev) under one of the i page's tapes: the share used, or the cache's minutes."""
    if kind == 'cache':
        cache = g['cache']
        if cache.get('cold'):
            return 'cold', 'amber'
        left = cache.get('left')
        if not left:
            return '--', 'normal'
        return (f'{int(left // 60)}m' if left < 6000 else f'{round(left / 3600)}h'), cache['sev']
    q = next((q for q in g['quotas'] if q['code'] == PANEL_LABEL[kind]), None)
    if not q or q['used'] is None:
        return '--', q['sev'] if q else 'normal'
    return f"{round(q['used'])}%", q['sev']


def info_panel(data, s, g, width, now):
    """The i page's instruments, under its title on the dark ramp: a row of the session's counts, the whole
    session's context as a chart PANEL_ROWS tall with the 7D, 5H and cache tapes beside it, the compactions
    ticked and the time named under it, each tape's value and label under it. (rows, each [(text, ground
    -> escapes)] as functions of the row's ground, the instruments it draws, as a Fact's shown_by, and the
    figure: with pixels, the chart as one picture, dict(row, col, cols, rows, facts) for draw_context, else
    None). Nothing without a chart."""
    n = width - 1 - (1 + SLOT * len(PANEL_TAPES))
    tapes = n >= CHART_MIN
    n = n if tapes else width - 2
    series = session_series(s, n, now) if n >= CHART_MIN else None
    if not series:
        return [], set(), None
    peaks, ticks, clock = series
    shows = {'cache', '7d', '5h'} if tapes else set()
    shows -= {q['code'].lower() for q in g['quotas'] if q['stale']}  # a rolled-over window says so in the list
    counts = [f"{len(s.get('prompts') or ())} prompts", f'{span(clock.total)} active',
              f"{len(s.get('compacts') or ())} compacts", '' if g.get('compacting') else g['payback']['text']]
    said = fit_pieces(' · '.join(x for x in counts if x), width - 1)
    shows |= {'prompts'} if counts[0] in said else set()
    shows |= {'compacts'} if counts[2] in said else set()
    rows = [lambda ground: ' ' + sgr('data') + said + OFF] if said else []
    figure = None
    if PIXELS:  # one picture over the chart's cells (draw_context), the cells left as ground under it
        main = [(t, c) for t, c in s.get('main') or () if t and c]
        figure = dict(row=len(rows), col=1, cols=n, rows=PANEL_ROWS, facts=dict(
            series=main, compacts=sorted(s.get('compacts') or ()), clock=clock, floor=g['floor']['tokens'],
            csr_at=((data.get('infinite') or {}).get('csr_at') or [])))
        chart = [[(' ', None, None)] * n for _ in range(PANEL_ROWS)]
    else:
        chart = chart_cells(peaks, g['floor']['tokens'], PANEL_ROWS)
    columns = [(kind, (quota_tape(next((q for q in g['quotas'] if q['code'] == PANEL_LABEL[kind]), None), PANEL_ROWS)
                       if kind != 'cache' else cache_tape(g['cache'], PANEL_ROWS))) for kind in PANEL_TAPES] if tapes else []

    def slots(cells_of, ground):
        return bg_of(ground) + ' ' + ''.join(bg_of(ground) + ' ' + cells_of(kind, cells) + bg_of(ground) + ' '
                                              for kind, cells in columns)

    for k in range(PANEL_ROWS):
        rows.append(lambda ground, k=k: ' ' + ''.join(cell_text(cell, ground) for cell in chart[k])
                    + (slots(lambda kind, cells: tape_pair(cells[k], ground), ground) if columns else '') + OFF)
    tick_row = ' ' * n if figure else ''.join(sgr('amber') + '╹' if i in ticks else ' ' for i in range(n))  # drawn as drops
    values = [panel_value(kind, g) for kind, _ in columns]
    rows.append(lambda ground: ' ' + tick_row + OFF + (' ' + ''.join(slot(t.center(SLOT), sev) for t, sev in values)
                                                       if columns else ''))
    first = datetime.fromtimestamp(clock.begin).strftime('%a %H:%M')
    last = 'now'
    axis = first + ' ' * max(1, n - len(first) - len(last)) + last if len(first) + len(last) < n else fit(first, n)
    rows.append(lambda ground: ' ' + sgr('normal') + axis + OFF + (
        ' ' + ''.join(slot(PANEL_LABEL[kind].center(SLOT), 'normal') for kind, _ in columns) if columns else ''))
    return rows, shows, figure


def draw_info(data, s, cols, rows, ui, top=0):
    """The i page: its title and the context on the dark ramp (their first EMBLEM columns kept for the emblem),
    then the instruments (info_panel) on the ramp below, then on paper the at-a-glance sections (info_sections):
    This session with its lanes (you, context, memory), What CSR cost you this week, How it's wired, each fact
    an instrument already draws left out; then the key. The title rows stay; the rest scrolls by top.
    (lines, pictures, top)."""
    width = max(24, cols - 1)
    now, s = time.time(), s or {}
    g = gauges(data.get('claude_fields'), data.get('claude_at'), s, now, data.get('limits'), data.get('sts'),
               data.get('compacting'))
    title = info_title(s, now)
    if EMBLEM + len(title) > width:  # too narrow: the id goes before when it began, then the word since
        title = ' · '.join(p for p in title.split(' · ') if p != (s.get('id') or '')[:8])
    if EMBLEM + len(title) > width:
        title = title.replace(' · since ', ' · ')
    head = [' ' * EMBLEM + BOLD + title[:len('Session info')] + OFF + sgr('normal')
            + fit(title[len('Session info'):], max(1, width - EMBLEM - len('Session info'))) + OFF]
    ctx, main, shows = g['ctx'], s.get('main') or [], set()
    if ctx['tokens']:
        measured = len([r for r in s.get('compact_sizes') or () if isinstance(r[1], (int, float)) and r[1] > 0])
        pieces = ' · '.join(x for x in (
            short(ctx['tokens']) + (f" of {short(ctx['size'])}" if ctx['size'] else ''),
            f'peak {short(max(c for _, c in main))}' if main else '',
            f"floor {short(g['floor']['tokens'])} " + (f'(last {min(FLOOR_ROWS, measured)})' if g['floor']['measured'] else '(guess)'))
            if x)
        head.append(' ' * EMBLEM + sgr('normal') + 'CONTEXT ' + slot(fit_pieces(pieces, width - EMBLEM - 8), 'data'))
        shows.add('context')
    else:
        head.append('')
    panel, more, figure = info_panel(data, s, g, width, now)
    shows |= more
    grounds = dark_steps(len(head) + len(panel))
    out = Lines([on_dark(line, ground) for line, ground in zip(head, grounds)])
    body = Lines([on_dark(row(ground), ground) for row, ground in zip(panel, grounds[len(head):])])
    placed = []  # the context figure over the chart's rows, each of its rows on that row's ground
    if figure:
        backs = grounds[len(head) + figure['row']:len(head) + figure['row'] + figure['rows']]
        png = context_png(figure['facts'], figure['cols'], figure['rows'], cell_size(), backs)
        if png:
            placed.append((figure['row'], figure['col'], figure['cols'], figure['rows'], png))
    lanes, drawn = Lines(), []  # you, context and memory: This session's lanes, the counts under them
    if s.get('prompts') or s.get('replies'):  # a session with nothing yet has no lanes and no counters
        if PIXELS and width - LANE > 8 and s.get('learned'):
            try:
                png = session_png(s, width - LANE, cell_size(), now)
                drawn.append((len(lanes), LANE, width - LANE, 3, png))
                lanes.put([' ' + ink('faint') + name.ljust(LANE - 1) + OFF for name in ('you', 'context', 'memory')])
            except Exception:  # no picture is better than no page
                pass
        suffix = ' · this session'
        legend = lanes_legend(s, width - 2 - len(suffix)) if s.get('learned') else ''
        if legend:  # its first count stays at any width: the suffix goes first, then the count is cut
            fits = 2 + len(COLOUR.sub('', legend)) + len(suffix) <= width
            lanes.put([ansi_fit('  ' + legend + (ink('quiet') + suffix + OFF if fits else ''), width)])
        handed = handed_line(s)
        if handed:
            lanes.put([' ' + ink('faint') + fit_pieces(handed, width - 2) + OFF])
    page, pictures = info_sections(info_rows(data, s, now, max(8, width - INFO_LABEL)), width, shows, (lanes, drawn))
    pictures = placed + [(r + len(body), c, w, h, png) for r, c, w, h, png in pictures]
    body.extend(page)
    key = ('─ context · ▬ your band · ╷ compacted, red by CSR' if figure else ('' if PIXELS else 'CTX ') + '▁▃▆ context per turn') \
        + ' · ♻ compact payback · ↻ resets · STS status · ◷ cache'
    body.put([''] + [' ' + ink('faint') + fit_pieces(key, width - 2) + OFF, ' ' + ink('faint') + tape_key(width - 2) + OFF])
    shown, kept, top = window(body, pictures, rows - len(out), top)
    out.extend(shown)
    # the title and the instruments still in view: a click on them goes back, as one on the main page's masthead
    # opened this page (10-12: "clicking on it again should return back to main")
    out.acts.update({r: dict(key='view:main') for r in range(min(len(head) + max(0, len(panel) - top), len(out)))})
    out.grounds = grounds[:len(head)] + grounds[len(head) + top:]  # the strip beside the ramp's rows still in view
    pictures = [(r + len(head), c, w, h, png) for r, c, w, h, png in kept]
    if PIXELS and len(head) >= ICON[1]:  # the emblem in the title rows' first columns, left free for it
        try:
            pictures.insert(0, (0, 0, ICON[0], ICON[1], emblem_png(cell_size(), tuple(g[:3] for g in grounds[:ICON[1]]))))
        except Exception:  # no emblem is better than no page
            pass
    return out, pictures, top


def handed_counts(s):
    """(handed, opened, asked) this session: the hand-offs CSR made to Claude, how many of them Claude opened, and
    the questions Claude asked CSR itself."""
    s = s or {}
    given = handoffs(s['gave']['recalls']) + len(s['gave']['carried']) if s.get('gave') else 0
    items = s.get('remembered') or []
    return (given, sum(1 for item in items if item.get('opened')),
            sum(1 for item in items if item.get('kind') != 'recall'))


def handed_line(s):
    """The i page's one line on what CSR handed Claude: 'CSR handed 27 · opened 0 · Claude asked 8×'. Opened
    shows even at 0, the health of the hand-offs; '' when nothing was handed, opened or asked."""
    given, opened, asked = handed_counts(s)
    if not (given or opened or asked):
        return ''
    return ' · '.join(x for x in (f'CSR handed {given}', f'opened {opened}', f'Claude asked {asked}×' if asked else '') if x)


def tape_key(width):
    """One line on the pace and the tapes' marks: » on row 2 is a window ahead of pace; the bright bar is the pace
    on 7D and 5H, the floor on C; the thin ticks are CTX_AMBER and CTX_HOT. Shorter wordings before any is cut,
    the » kept longest, so it never wraps."""
    amber, hot = short(CTX_AMBER), short(CTX_HOT)
    forms = (f'» ahead of pace · ━ pace (7·5) or floor (C) · ─ {amber} / {hot}',
             f'» ahead of pace · ━ pace or floor · ─ {amber} / {hot}', f'» ahead of pace · ━ pace/floor · ─ {amber}/{hot}',
             f'» ahead · ━ pace/floor · ─ {amber}/{hot}', '» ahead of pace · ━ pace/floor', '» ahead · ━ pace/floor')
    return next((f for f in forms if len(f) <= width), fit_pieces(forms[-1], width))


def column_line(forms, width):
    """The first of forms [(text, sev)] that fits width, else the last one cut. Column mode spares no column for a
    margin: the strip's own keeps the text off the tapes."""
    text, sev = next(((t, sv) for t, sv in forms if len(t) <= width), (fit(forms[-1][0], max(1, width)), forms[-1][1]))
    return slot(text, sev)


COLUMN_LABEL = dict(story='Story', dreams='If…', lost='Lost', remembered='Hand', coach='Read', rules='Rule',
                    told='Told', last='Last', todos='Todo', touching='File', changed='Diff', compacted='Cmp')  # 'Lost 18' in 12 columns


def draw_column_main(data, s, width, rows, now, ui):
    """Column mode's main page, width columns left of the strip: the masthead's values one a row on the dark
    ramp, each in the ink of its severity (stack_rows), then each section with something in it as one count
    on its tint. The pages stay one key away; no sparkline, no icons."""
    if s is None:
        s = dict(id=None, title=None, prompts=[], replies=[], compacts=[], gave=dict(recalls=[], carried=[], dreams=[]),
                 remembered=[], sessions=0, project=None, files={}, edited=[], shell={}, last={})
    page = with_compacting(page_main(data, s, now, ui), data)
    if default_view(page, s):
        page = page_default(page, data, s, now, rows)
    stack = stack_rows(page['head']['gauges'])
    grounds = dark_steps(len(stack))
    out = Lines([on_dark(column_line(forms, width), ground) for forms, ground in zip(stack, grounds)])
    out.acts.update({row: dict(key='view:info') for row in range(min(MASTHEAD_CLICK, len(stack)))})
    out.grounds = grounds
    for section in page['sections']:
        if not section['items']:
            continue
        name, count = section['name'], section.get('count')
        count = len(section['items']) if count is None else count
        label = COLUMN_LABEL.get(name, section['title'].split(' ')[0])
        forms = [f'{label} {count}', f'{label[0]} {count}', f'{label[0]}{count}']
        text = next((t for t in forms if len(t) <= width), fit(forms[-1], max(1, width)))
        if len(out) > len(stack):  # a strip of paper between counts; the first sits right under the values
            out.put([''])
        out.put([tinted(BOLD * bool(section.get('star')) + ink(SECTION[name]) + text + OFF, band(name, HEAD))])
    return out


def draw_column_info(data, s, width, rows, top=0):
    """Column mode's i page: the title wrapped on the dark ramp, then the sections as on the wide page, each
    heading over its hairline and each fact its label over its value, its note faint under that, wrapped to
    width. Scrolled by top; (lines, pictures, top)."""
    now, s = time.time(), s or {}
    title = ' '.join(p for p in info_title(s, now).split(' · ') if p != (s.get('id') or '')[:8])  # no room for the id
    head = [BOLD + line + OFF for line in wrap(title, max(1, width), 5)]
    out = Lines([on_dark(line, ground) for line, ground in zip(head, dark_steps(len(head)))])
    out.grounds = dark_steps(len(head))
    body, facts = Lines(), info_rows(data, s, now, max(4, width))
    for section in (SESSION, COST, WIRED):
        rows_of = [f for f in facts if f.section == section]
        if not rows_of:
            continue
        if body:
            body.put([''])
        body.put([BOLD + ink('you') + piece + OFF for piece in wrap(section.upper(), max(1, width), 2)]
                 + [ink('faint') + '─' * max(1, width) + OFF])
        for fact in rows_of:
            value = COLOUR.sub('', fact.value) if fact.label == 'Status line' else fact.value
            if fact.label:
                body.put([ink('quiet') + fit(fact.label, max(1, width)) + OFF])
            colour = ink(PAPER_INK[fact.sev]) if fact.sev else ink('you')
            body.put([colour + piece + OFF for piece in wrap(one_line(value, 400), max(1, width), 4)])
            if fact.note:
                body.put([ink('faint') + piece + OFF for piece in wrap(one_line(fact.note, 400), max(1, width), 3)])
    shown, _, top = window(body, [], rows - len(out), top)
    out.extend(shown)
    out.acts.update({r: dict(key='view:main') for r in range(len(head))})  # the title, clicked: back to main
    return out, [], top


FG = re.compile(r'\x1b\[38;[0-9;]*m')


def column_reflow(lines, width):
    """A page drawn at its narrowest, its rows' words rewrapped to width for column mode, each row in its first
    ink; a row's click goes with every row it became. Pictures and tints drop."""
    out = Lines()
    for i, line in enumerate(lines):
        text = CONTROL.sub('', SGR.sub('', line)).strip()
        colour = (FG.search(line) or re.match('', '')).group()
        pieces = wrap(text, max(1, width), 99) if text else ['']
        out.put([colour + piece + OFF if piece else '' for piece in pieces], getattr(lines, 'acts', {}).get(i))
    return out


def info_keys(pane, keys):
    """The i page's keys: i opens it, i or Esc goes back, j and k scroll; True when the screen must redraw."""
    if 'i' in keys and pane.view != 'info':
        pane.view, pane.open, pane.info_top = 'info', None, 0
        return True
    if pane.view != 'info':
        return False
    if keys == '\x1b' or 'i' in keys:
        pane.view, pane.open = 'main', None
        return True
    step = 3 if any(k in keys for k in ('j', '\x1b[b')) else -3 if any(k in keys for k in ('k', '\x1b[a')) else 0
    if ' ' in keys:
        step = 12
    if step:
        pane.info_top = max(0, pane.info_top + step)
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
    shown.grounds = getattr(lines, 'grounds', [])[top:]  # the masthead's grounds, for the tape beside it
    kept = [(row - top, col, w, h, png) for row, col, w, h, png in pictures if 0 <= row - top and row - top + h <= room]
    return shown, kept, top


def compact_key(data):
    """What redraws the pane for a compaction: the timer's second and facts, the summary page's place."""
    summary = data.get('summary')
    return [data.get('compacting'), summary and [summary['count'], summary['top']], data.get('infinite')]


RELOAD = 60  # seconds between two reads of csr-engine and the logs; the legend counts down to the next


def reload_in(loaded, now):
    """Whole seconds until the next reload, as the legend says them: never under 0."""
    return max(0, math.ceil(RELOAD - (now - loaded)))


def reload_label(seconds):
    """'↻ 42s', padded to the width of '↻ 60s' so the keys after it never move as it counts down."""
    return f'↻ {seconds}s'.ljust(len(f'↻ {RELOAD}s'))


def reload_now(pane):
    """r: read everything again at the next frame, and draw the main page fresh even while Claude works."""
    pane.loaded, pane.hold = 0.0, None


def legend(view, width, held=None, phone=False, column=False, reload=None):
    """The bottom row: its keys in lamp black, what each does in pencil. While an edit to the pane's own file
    does not compile, the row says so and the old pane runs on."""
    return legend_parts(view, width, held, phone, column, reload)[0]


def legend_parts(view, width, held=None, phone=False, column=False, reload=None):
    """The bottom row, and where each label sits on it: (text, [(first col, past last col, key)]). On a phone the
    main page's keys start short, one word each, and f (every lost thread, also a click on its heading) goes.
    In column mode, the keys alone. reload, the seconds to the next read, puts 'r ↻ 42s' before close on the
    main page: r reads now."""
    if held:
        return ' ' + ink('turn') + fit(f'edit not loaded: {held}', width - 1) + OFF, []
    if column:
        keys = ['l', 's', 'i', 'q'] if view == 'main' else ['d', 'j/k', 'q'] if view in ('summary', 'threads', 'info') \
            else ['d', 'q']
        while len(keys) > 1 and 1 + len(' '.join(keys)) > width:
            keys.pop(-2)  # q, close, stays
        spans, at = [], 1
        for key in keys:
            spans.append((at, at + len(key), key))
            at += len(key) + 1
        return ' ' + ' '.join(f'{BOLD}{ink("you")}{key}{OFF}' for key in keys), spans
    if view == 'main' and phone:
        keys = [('l', 'changes'), ('s', 'summary'), ('i', 'info'), ('q', 'close')]
    elif view == 'main':
        keys = [('l', 'changes since your sessions'), ('s', 'compaction summary'), ('f', 'every lost thread'),
                ('i', 'session info'), ('q', 'close')]
    else:
        keys = [('d', 'back')] + ([('j/k', 'scroll')] if view in ('summary', 'threads', 'info') else []) + [('q', 'close')]
    if view == 'main' and reload is not None:
        keys.insert(len(keys) - 1, ('r', reload_label(reload)))
    marks = '' if view in ('main', 'summary', 'threads', 'info') else '~ changed  - removed  + back   '
    words = [(key, said) for key, said in keys]
    wide = lambda gap: len(marks) + sum(len(k) + len(w) + 1 + gap for k, w in words) - gap + 1
    if view == 'main' and not phone:  # one label at a time, from the left, until the row fits: l 'changes', s 'summary', f 'thread', i 'info'
        for at, label in ((0, 'changes since your'), (0, 'changes since'), (0, 'changes'), (1, 'summary'), (2, 'thread'), (3, 'info')):
            if wide(2) <= width:
                break
            words[at] = (words[at][0], label)
    elif view != 'main':
        while len(marks) + sum(len(k) + len(w) + 4 for k, w in words) > width and len(words[0][1]) > 7:
            words[0] = (words[0][0], words[0][1].rsplit(' ', 1)[0])
        if len(marks) + sum(len(k) + len(w) + 4 for k, w in words) > width:  # still too wide: 's summary'
            words = [words[0]] + [(key, said.rsplit(' ', 1)[-1]) for key, said in words[1:]]
    sep = '   ' if (wide(3) if view == 'main' else len(marks) + sum(len(k) + len(w) + 4 for k, w in words)) <= width else '  '
    total = lambda: 1 + len(marks) + sum(len(k) + 1 + len(w) for k, w in words) + len(sep) * (len(words) - 1)
    if total() > width:
        marks = ''  # the log's marks go before any key does
    while len(words) > 1 and total() > width:
        # narrower still: f goes first, then s, then i (the keys least needed to find a page), then the countdown,
        # then scroll
        at = next((k for name in ('f', 's', 'i', 'r', 'j/k') for k, (key, _) in enumerate(words) if key == name and 0 < k < len(words) - 1),
                  len(words) - 2)
        words.pop(at)
    if total() > width:
        words = [(words[0][0], fit(words[0][1], max(1, width - len(words[0][0]) - 2)))]
    shown = sep.join(f'{BOLD}{ink("you")}{key}{OFF}{ink("faint")} {said}{OFF}' for key, said in words)
    spans, at = [], 1 + len(marks)
    for key, said in words:
        spans.append((at, at + len(key) + 1 + len(said), key))
        at += len(key) + 1 + len(said) + len(sep)
    return ' ' + ink('faint') + marks + OFF + shown, spans


def press(pane, key):
    """What a page key does, for a click on its label: l, s, f and i open their pages, d goes back, r reads now.
    Scrolling and close stay on the keyboard, so a stray click never closes the pane."""
    if key == 'r':
        reload_now(pane)
    elif key == 'l' or key == 'd':
        pane.view, pane.open = ('log' if key == 'l' else 'main'), None
    elif key == 's':
        summary_keys(pane, 's')
    elif key == 'f':
        threads_keys(pane, 'f')
    elif key == 'i':
        info_keys(pane, 'i')


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


HERDR_CONFIG = os.path.join(HOME, '.config', 'herdr', 'config.toml')
MOBILE_WIDTH = 64  # herdr's ui.mobile_width_threshold when its config does not set one
MOBILE_EVERY = 5  # seconds between two reads of the layout at one pane size


def mobile_threshold(path=None):
    """herdr's ui.mobile_width_threshold: at or under this many columns it lays a tab out for a phone, one pane
    at a time under a one-line header. Its default on any doubt (no file, bad TOML, not a positive int)."""
    try:
        import tomllib
        with open(path or HERDR_CONFIG, 'rb') as handle:
            value = tomllib.load(handle).get('ui', {}).get('mobile_width_threshold', MOBILE_WIDTH)
    except Exception:
        return MOBILE_WIDTH
    return value if isinstance(value, int) and not isinstance(value, bool) and value > 0 else MOBILE_WIDTH


def is_mobile(layout, threshold):
    """True when `herdr pane layout` (its JSON text) puts this pane's tab in herdr's phone layout: the tab's area
    no wider than threshold. A tab follows the size of the client that last used it, so this is the screen in
    front of you now. Anything unreadable is a desktop."""
    try:
        width = json.loads(layout)['result']['layout']['area']['width']
    except Exception:
        return False
    return isinstance(width, int) and not isinstance(width, bool) and 0 < width <= threshold


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
    """Once a click has put text in Claude's prompt, the focus goes back to Claude's pane on the left. Any
    other click leaves it here, so the pane's keys work."""
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
        self.info_top = 0  # the i page's first line shown
        self.hold = None  # dict(sections, at): the main page's sections as last drawn, kept while Claude works
        self.main_top = 0  # the main page's first line shown, once scrolled or a row is opened
        self.anchor = None  # (row key, screen row) a click just opened: it stays where it was clicked
        self.tall = False  # the main page keeps its scrolled layout until Esc or another page
        self.drawn = None  # (cols, rows, rows written, pictures placed): what is on the screen now
        self.seen_phone = None  # ((cols, rows), when, on a phone): the last read of herdr's layout
        self.tape_s = None  # the session as the tape last read it, for the pages that load none
        self.edge = None  # (first tape column, rows it runs): a click there is on the tape, not a row
        self.foot = None  # (on a phone, in column mode): what the legend row was last drawn for
        self.feed = Feed(session_id) if session_id else None  # the main page's order, Earlier, what is new
        self.spend = Spend()  # this machine's spend in the plan's week, read from the transcripts a little at a time

    def phone(self, cols, rows, now):
        """Whether herdr shows this pane on a phone now: read from its layout on a new size, else at most every
        MOBILE_EVERY seconds. Outside herdr, or on any doubt, a desktop."""
        seen = self.seen_phone
        if seen and seen[0] == (cols, rows) and 0 <= now - seen[1] < MOBILE_EVERY:
            return seen[2]
        me = os.environ.get('HERDR_PANE_ID')
        found = bool(me) and is_mobile(herdr('pane', 'layout', '--pane', me) or '', mobile_threshold())
        self.seen_phone = ((cols, rows), now, found)
        return found

    def frame(self, force):
        cols, rows = shutil.get_terminal_size((60, 30))
        now = time.time()
        phone = fail_open(self.phone, False, cols, rows, now)
        column = cols < COLUMN_UNDER  # a pane this narrow (a phone's split) is its gauge strip and a column of values
        tapes = strip_tapes(cols, rows, column)
        body = cols - strip_width(tapes)  # the pages draw left of the strip
        try:
            if self.data is None or now - self.loaded >= RELOAD:
                self.data, self.loaded = load(self.root, self.limit, self.session and self.session.id), now
            self.data['dreams'] = load_dream_log(self.root)  # the mod appends while we watch
            self.data['look'] = last_look(self.root)
            self.data['chapter'] = last_chapter(self.root, self.session and self.session.id)
            self.data['claude_line'] = claude_status(self.session and self.session.id, now)
            self.data['story'] = session_story(self.session and self.session.id)
            # each of these reads a file of someone else's: one that fails is empty, it never blanks the pane
            self.data['claude_fields'], self.data['claude_at'] = fail_open(
                kept_status, ({}, None), self.session and self.session.id, now)
            self.data['limits'] = fail_open(account_limits, {}, now)
            self.data['sts'] = fail_open(csr_status, dict(sev=None, inop=[]), now)
            self.data['compacting'] = compacting_info(self.session and self.session.id, now, self.data)
            if self.view == 'summary':
                self.data['summary'] = newest_summary(self.session and self.session.path)
            force = compaction_ended(self, self.data['compacting']) or force  # the marker went: redraw at once
            self.data['readings'] = how_claude_read(self.session and self.session.id)
            self.data['likes'] = fail_open(read_likes, {})
            plan = plan_of(self.data['claude_fields'], self.data['limits'], now)
            # the plan's week, never named window: that is the tall page's layout, called below (10-12 17:18 a click
            # on a row raised TypeError there, a float in its place)
            week = plan['start'] if plan['mode'] == 'plan' else plan['mode']  # a rolling week moves on each minute
            if self.data.get('narrative') is None or self.data.get('narrative_window') != week:
                self.data['narrative'] = fail_open(narrative_week, {}, plan['start'])  # once a minute, or a new week
                self.data['narrative_window'] = week
            self.data['plan'] = plan
            self.data['spend'] = self.scan(plan, now)
            self.data['value'] = fail_open(week_value, {}, plan, self.data.get('narrative'), self.data['likes'],
                                           self.data['spend'])
            s = self.session.load() if self.session and self.view in ('main', 'threads', 'info') else None
            self.data['infinite'] = fail_open(infinite_info, None, self.session and self.session.id, now,
                                              s if s is not None else getattr(self, 'tape_s', None))
        except Exception as error:  # the DB is busy or missing: say so, keep the pane alive
            said = SETUP if not os.path.exists(DB) else f'could not read csr-engine: {error}'
            sys.stdout.write(f"\x1b_Ga=d,d=A\x1b\\\x1b[H\x1b[2J{BOLD}CSR{OFF}\n{DIM}"
                             + '\n'.join(wrap(said, cols - 1, 4)) + f"{OFF}\n")
            sys.stdout.flush()
            self.drawn = self.shown = None  # the screen is no longer the last frame: the next one starts clear
            return
        if self.sent and now - self.sent[2] > 6:  # the "in Claude's prompt" line has been read
            self.open = None if self.open == self.sent[0] else self.open
            self.sent = None
        if self.answered and now - self.answered[2] > 6:
            self.answered = None
        ui = dict(open=self.open, sent=self.sent, answered=self.answered, folds=self.folds, rows=rows, phone=phone,
                  column=column, likes={key: liked for key, (liked, _) in self.data['likes'].items()})
        self.tape_s = s if s is not None else self.tape_s
        g = fail_open(gauges, None, self.data.get('claude_fields'), self.data.get('claude_at'), self.tape_s, now,
                      self.data.get('limits'), self.data.get('sts'), self.data.get('compacting')) if tapes else None
        columns = gauge_strip(g or gauges({}, None, None, now), tapes, rows - 2) if tapes else []
        sig = json.dumps([self.view, cols, rows, phone, columns, self.data['total'], self.data['dreams_on'], self.open, self.sent,
                          self.answered,
                          self.data.get('claude_line'),
                          [self.data.get('claude_fields'), self.data.get('claude_at'), self.data.get('limits'),
                           self.data.get('sts'), int(now // 60)],
                          [(d['id'], d['answer']) for d in self.data['dreams']], self.data.get('look'),
                          self.data.get('chapter'), sorted(self.folds), self.data.get('story'), self.held,
                          compact_key(self.data), self.data.get('readings'), self.threads_top, self.main_top,
                          self.info_top, bool(s and s.get('busy')), self.feed and self.feed.key(now),
                          value_pieces(self.data.get('value')),  # what it says: the dollars under it move each step
                          sorted(key for key, (liked, _) in self.data['likes'].items() if liked),
                          s and [s.get('last_session') is not None, bool(s.get('told')), s.get('turn_end'),
                                 len(s.get('todos') or [])],
                          s and [(t['key'], t['hidden'], t['state']) for t in s['lost']],
                          s and [len(s['replies']), len(s['prompts']), len(s['learned']['chunks']), s['title'],
                                 len(s['learned']['turns']), len(s['gave']['recalls']), len(s['remembered'])]])
        if sig == self.shown and not force:
            self.tick(now)  # nothing else moved: the countdown may have
            return
        self.shown = sig
        self.tall = self.view == 'main' and not column and bool(self.tall or self.main_top or self.open)
        # the value line above the legend, on the main page: the page gives up its rows
        value = value_rows(self.data.get('value'), cols - 1) if self.view == 'main' and not column else []
        room = rows - len(value)  # the rows the page is laid out in, the legend's own still counted
        if column and self.view == 'main':  # the strip is the page: the values stack, each section one count
            lines, pictures = draw_column_main(self.data, s, body, rows, now, ui), []
        elif column and self.view == 'info':
            lines, pictures, self.info_top = draw_column_info(self.data, s, body, rows, self.info_top)
        elif self.tall:  # laid out whole, the screen a window on it
            lines, pictures = self.draw_main_held(self.data, s, body, TALL, now, ui)
            if self.anchor:
                row = min((r for r, act in lines.acts.items() if act.get('key') == self.anchor[0]), default=None)
                self.main_top = max(0, row - self.anchor[1]) if row is not None else self.main_top
                self.anchor = None
            heads = getattr(lines, 'heads', [])
            lines, pictures, self.main_top = window(lines, pictures, room, self.main_top)
            self.seen(on_screen(heads, self.main_top, room - 1), now)
        elif self.view == 'main':
            lines, pictures = self.draw_main_held(self.data, s, body, room, now, ui)
            self.seen(on_screen(getattr(lines, 'heads', []), 0, room - 1), now)
        elif self.view == 'summary':
            lines, pictures = draw_summary(self.data, body, rows, ui)
        elif self.view == 'threads':
            lines, pictures, self.threads_top = draw_threads(self.data, s, body, rows, ui, self.threads_top)
        elif self.view == 'info':
            lines, pictures, self.info_top = draw_info(self.data, s, body, rows, ui, self.info_top)
        else:
            lines, pictures = draw_log(self.data, body, rows, ui)
        if column and self.view not in ('main', 'info'):  # a page laid out at its narrowest, its words rewrapped
            lines, pictures = column_reflow(lines, body), []
        self.acts = {row: act for row, act in getattr(lines, 'acts', {}).items() if row < room - 1}
        if value:  # the value row: a click opens the i page, which says it in words
            self.acts[room - 1] = dict(key='view:info')
        foot, spans = legend_parts(self.view, cols - 1, self.held, phone, column, self.countdown(now, column))
        self.acts[rows - 1] = dict(key='legend', keys=spans) if spans else \
            dict(key='view:log' if self.view == 'main' else 'view:main')
        grounds = getattr(lines, 'grounds', ())  # the masthead's, for the strip beside it (a slice drops them)
        lines = lines[:room - 1]
        whole = any(h >= rows for _, _, _, h, _ in pictures)  # a paper page draws its own legend
        written = [safe_line(line) for line in lines] + [''] * (room - 1 - len(lines)) + value
        # the strip after the row, by absolute position: a repainted row repaints its strip cells with it
        strips = strip_rows(columns, cols, grounds)
        written = [line + strip for line, strip in zip(written, strips)] + written[len(strips):]
        self.edge = (body, rows - 1) if columns else None
        written.append('' if whole else foot)
        placed = [(r, c, w, h, png) for r, c, w, h, png in pictures if r + h <= (room - 1 if value else rows)]
        sys.stdout.write(repaint(self.drawn, cols, rows, written, placed))
        sys.stdout.flush()
        self.drawn = (cols, rows, written, placed)
        self.foot = None if whole else (phone, column)  # what the legend was drawn for, for tick

    def seen(self, names, now):
        """The feed hears which sections are on screen, and keeps what it knows."""
        if self.feed:
            self.feed.shown(names, now)
            self.feed.save(now)

    def scan(self, plan, now):
        """On a plan, the transcripts read on for at most SCAN_BUDGET; their spend once they are all read (and the
        week's), else None. With an API key nothing is read."""
        spend = getattr(self, 'spend', None)
        if not spend or plan['mode'] != 'plan':
            return None
        fail_open(spend.step, False, plan['start'], now)
        return spend.total() if spend.ready and spend.start == plan['start'] else None

    def countdown(self, now, column=False):
        """Seconds to the next read, for the main page's legend; None elsewhere, and in column mode (no room)."""
        return reload_in(self.loaded, now) if self.view == 'main' and not column else None

    def tick(self, now=None):
        """Between frames, about once a second: the transcripts' scan reads on while it has not caught up, and the
        legend's countdown moves, only its row written again. True when the scan has just caught up or the countdown
        has run out: the loop then draws a frame, which reads everything again."""
        now = time.time() if now is None else now
        plan, spend = (self.data or {}).get('plan'), getattr(self, 'spend', None)
        if plan and plan['mode'] == 'plan' and spend and not spend.ready:
            if fail_open(spend.step, False, plan['start'], now):
                return True
        if not self.drawn or not getattr(self, 'foot', None):
            return False
        cols, rows, written, placed = self.drawn
        phone, column = self.foot
        left = self.countdown(now, column)
        if left == 0:
            return True
        foot, spans = legend_parts(self.view, cols - 1, self.held, phone, column, left)
        if foot == written[-1]:
            return False
        written = written[:-1] + [foot]
        sys.stdout.write(repaint(self.drawn, cols, rows, written, placed))
        sys.stdout.flush()
        self.drawn = (cols, rows, written, placed)
        if spans:
            self.acts[rows - 1] = dict(key='legend', keys=spans)
        return False

    def draw_main_held(self, data, s, cols, rows, now, ui):
        """The main page, one draw: while Claude is mid-turn it keeps the sections it last drew (the masthead
        stays live), so nothing jumps under your eye; an idle frame, or a held one past HOLD_MAX, draws fresh
        and keeps what it drew."""
        held = held_sections(self.hold, bool(s and s.get('busy')), now)
        lines, pictures = draw_main(data, s, cols, rows, now, ui, hold=held, hold_quiet=(self.hold or {}).get('quiet'),
                                    feed=getattr(self, 'feed', None))
        if held is None:  # an empty page is never held: it may only be waiting for the carry to land
            self.hold = dict(sections=lines.sections, quiet=lines.quiet, at=now) if lines.sections else None
        return lines, pictures

    def like(self, act):
        """A click on an item's heart: it is liked, or no longer, kept in KEEP/likes.jsonl and on this machine only."""
        likes = (self.data or {}).get('likes') or {}
        key = like_key(act)
        keep_like(key, act.get('source'), act.get('title'), not likes.get(key, (False, 0))[0], time.time())
        if self.data is not None:
            self.data['likes'] = read_likes()

    def scroll(self, step):
        """The wheel, or j and k: the page in view moves, never past its top."""
        if self.view == 'summary' and (self.data or {}).get('summary'):
            self.data['summary']['top'] = max(0, self.data['summary']['top'] + step)
        elif self.view == 'threads':
            self.threads_top = max(0, self.threads_top + step)
        elif self.view == 'info':
            self.info_top = max(0, self.info_top + step)
        elif self.view == 'main':
            self.main_top = max(0, self.main_top + step)

    def click(self, row, col=0):
        """One click opens a row; a second click on it puts it in Claude's prompt, unsent. On a dream's
        answers, the half clicked is the answer: kept in the dream log, and "ask Claude" also typed in. A click
        on the tape is on no row."""
        edge = getattr(self, 'edge', None)
        if edge and col >= edge[0] and row < edge[1]:
            return
        act = self.acts.get(row)
        if act and act.get('keys') is not None:  # the legend: the label clicked, as if its key were pressed
            pressed = next((key for start, end, key in act['keys'] if start <= col < end), None)
            if pressed:
                press(self, pressed)
            return
        heart = act and act.get('heart')
        if heart and heart[0] <= col < heart[1]:  # the heart: liked, or not any more; the row stays as it was
            return self.like(act)
        if act and act.get('lost') and act.get('spans'):
            return lost_click(self, act, col)
        span = act and next((answer for start, end, answer in act.get('spans') or () if start <= col < end), None)
        if span:
            keep(dict(kind='answer', id=act['dream'], answer=span, at=time.time(), by='pane'))
            if span == 'ask_claude':
                agent = agent_pane(self.agent)
                went = send_text(agent, act['send'])
                self.sent = (act['key'], went, time.time())
                focus_back()
            self.answered = (act['dream'], span, time.time())
        elif not act or act.get('spans'):
            self.open = None
        elif act['key'].startswith('view:'):
            self.view, self.open = act['key'][5:], None
            if self.view == 'info':  # opened at its top, as the i key opens it
                self.info_top = 0
        elif act['key'].startswith('fold:'):
            self.folds ^= {act['key'][5:]}
        elif act.get('back') and getattr(self, 'feed', None):  # a row of Earlier: its section back on top, drawn now
            self.feed.back(act['back'], time.time())
            self.open, self.hold, self.main_top = None, None, 0
        elif act['key'] != self.open:
            self.open, self.sent = act['key'], None
            self.anchor = (act['key'], row) if self.view == 'main' else None
        else:
            agent = agent_pane(self.agent)
            went = send_text(agent, act['send'])
            self.sent = (act['key'], went, time.time())
            if went and act.get('dream'):  # a dream sent whole is handed to Claude, as its "ask Claude" half
                keep(dict(kind='answer', id=act['dream'], answer='ask_claude', at=time.time(), by='pane'))
                self.answered = (act['dream'], 'ask_claude', time.time())
            if went and act.get('lost'):  # a thread sent whole goes as its side task: kept as sent, as that half is
                keep_lost(act['lost'], 'sent', time.time(), project_of(getattr(self.session, 'path', None)), how='side')
            focus_back()  # it is in Claude's prompt now: the next keys belong there


# ---------------------------------------------------------------- lost and found

LOST_LOG = os.path.join(KEEP, 'lost-found.jsonl')  # drop / later, one line each, keyed by the thread's text; and
# sent, a thread's ask typed into Claude's prompt, which only the value line counts
LOST_ANSWERS = ('drop', 'later')  # what sets a thread aside; a sent line is none of them
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
           'see', 'get', 'got', 'let', 'use', 'way', 'go', 'yes', 'ok', 'okay', 'please', 'thank', 'thanks', 'good',
           # the verbs and the asking every go shares: "install PostgreSQL" does not answer "install Redis"
           'install', 'run', 'push', 'merge', 'build', 'fix', 'check', 'verify', 'update', 'approval', 'approve',
           'deploy', 'commit', 'test', 'start', 'stop', 'add', 'remove', 'open', 'close', 'send', 'ship', 'proceed',
           'await', 'wait', 'user', 'next', 'now', 'then', 'done', 'with', 'before', 'after', 'still', 'needs', 'need'}
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


HANDED_OFF = re.compile(r'\s*Lost and found: “(.+)” went quiet after \d{1,2}:\d\d\.(.*)', re.S)  # lost_asks' words, sent


def lost_handoffs(typed):
    """The prompts that handed a thread on: [(time, quoted words, how)], from what was typed and sent, so text
    left unsent, erased or edited out of the asks' wording never counts. how is 'sub' or 'side' by the ask's
    wording, else None. The quote may end cut (fit's …): words match it, not letters."""
    out = []
    for when, words in typed or ():
        m = HANDED_OFF.match(words or '')
        if m:
            rest = m.group(2)
            how = 'sub' if 'background subagent' in rest else 'side' if 'side task' in rest else None
            out.append((when, words_of(m.group(1).rstrip('…')), how))
    return out


def handed_why(when, how):
    at = datetime.fromtimestamp(when).strftime('%H:%M')
    return {'sub': f'handed to a subagent {at}', 'side': f'handed as a side task {at}'}.get(how, f'handed to Claude {at}')


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
        after = [(when, words) for when, words in sorted(typed) if when > t['last'] and not HANDED_OFF.match(words or '')][:2]
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


def project_of(path):
    """A transcript's project: the folder Claude Code keeps it in, one per working directory."""
    return os.path.basename(os.path.dirname(path)) if path else None


def lost_log(project=None):
    """What you did with each thread in this project: key -> (drop or later, when). The last line for a key
    wins; a line kept before answers named their project counts in every project. A sent line is no answer: a
    thread's ask typed into Claude's prompt leaves a drop or a later before it standing."""
    said = {}
    try:
        with open(LOST_LOG, encoding='utf-8') as handle:
            for line in handle:
                try:
                    r = json.loads(line)
                    if r.get('project') not in (None, project) or r.get('answer') not in LOST_ANSWERS:
                        continue
                    said[r['key']] = (r['answer'], float(r['at']))
                except (ValueError, KeyError, TypeError, AttributeError):
                    continue
    except OSError:
        pass
    return said


def keep_lost(key, answer, at, project=None, **more):
    private.append(LOST_LOG, private.dumps(dict(key=key, answer=answer, at=at, project=project, **more)) + '\n', root=KEEP)


def lost_found(summaries, typed=(), carried=(), project=None):
    """The threads, each marked hidden: "drop" for good, "handed" once a prompt sent its hand-off (lost_asks'
    words, typed and submitted after the thread first showed), "later" until the next compaction. A thread set
    aside for later is also the one thing prep-compact hands the summary, as a reminder. The session
    before this one adds its threads, less those this session's own summaries already carry; such a
    thread passes its answer on, so a drop made before the restart still holds."""
    threads, said = lost_threads(summaries, typed), dict(lost_log(project))
    own = [(t, words_of(t['text'])) for t in threads]
    for c in carried:
        match = next((t for t, words in own if same_words(words_of(c['text']), words)), None)
        if match is None:
            threads.append(dict(c))  # a copy: the pane's thread may be writing the list it came from
        elif c['key'] in said and match['key'] not in said:
            said[match['key']] = said[c['key']]
    threads.sort(key=lambda t: (t['rank'], -t['last_seen']))
    handed = {}
    for when, quoted, how in lost_handoffs(typed):  # each names one thread: the one its quote matches best
        tied = [(len(quoted & words) / len(quoted | words), t) for t in threads if t['first_seen'] < when
                for words in [words_of(t['text'])] if same_words(quoted, words)]
        if tied:
            handed[id(max(tied, key=lambda pair: pair[0])[1])] = (when, how)
    newest = max((when for when, _ in summaries), default=0)
    for t in threads:
        answer, at = said.get(t['key'], (None, 0))
        t['hidden'] = answer if answer == 'drop' or (answer == 'later' and at > newest) else None
        t['later'] = answer == 'later'
        if id(t) in handed and answer != 'drop':
            t.update(hidden='handed', state='handed', why=handed_why(*handed[id(t)]))
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
    for _, other in sorted(found, reverse=True)[:200]:  # past headless ones too: each costs one 64 KB read
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
    typed = [(when, words) for when, words in prev.typed if not HANDED_OFF.match(words or '')]  # a hand-off is answered
    after = [(when, words) for when, words in typed if when > since and (words or '').strip()]
    last = typed[-1] if typed else None
    ask = last_ask(prev.said[1]) if prev.said else None
    if last and last[0] > since and last[0] > prev.turn_end:
        text, waiting, rank = f"Cut off before Claude finished answering: “{fit(one_line(last[1]), 200)}”", True, 0
    elif ask and prev.said[0] > since and (not last or prev.said[0] > last[0]):
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
                key=hashlib.sha256(f'stopped {prev.id}'.encode()).hexdigest()[:12])


def previous_read(path, started):
    """The Session before this one, its transcript read, or None (previous_session found none)."""
    before = previous_session(path, started)
    if not before:
        return None
    prev = Session(os.path.basename(before)[:-6], path=before)
    prev.read_transcript()
    return prev


def carried_threads(path, started, prev=None):
    """What a restart would lose, read from the session before this one, no model: the threads its newest
    summary carried or dropped with no answer, plus what came after that summary (stopped_at), which is
    the newest work and the likeliest to be unanswered. A session that never compacted still gives that.
    Given prev (a Session already read), nothing is read a second time."""
    prev = prev or previous_read(path, started)
    if not prev:
        return []
    before = prev.path
    said = lost_log(project_of(before))
    out = []
    # what that session itself carried in and nobody dropped: a second restart loses nothing either
    for t in lost_found(prev.summaries, prev.typed, read_carried(prev.id) or (), project_of(before)):
        if t['hidden'] in ('drop', 'handed') or t['key'] in said and said[t['key']][0] == 'drop':
            continue
        if t['state'] in ('open', 'lost'):
            out.append(dict(t, state='carried', rank=0 if t['waiting'] else 1, came_from=prev.id[:8],
                            why=f"session {prev.id[:8]}, {'open' if t['state'] == 'open' else 'quiet'} when it ended"))
        elif t['state'] == 'carried':
            out.append({k: v for k, v in t.items() if k not in ('hidden', 'later')})
    tail = stopped_at(prev)
    return out + ([tail] if tail else [])


PREVIOUS_DIR = 'previous'  # KEEP/previous/<session>.json: the facts of the session before, for the default view
TASKS = os.path.join(HOME, '.claude', 'tasks')  # <session>/<N>.json: {subject, status}, as csr-engine's Stop hook reads
OPEN_TASK = ('in_progress', 'pending')
SERIES_MAX = 2000  # context points kept from the last session; more are thinned


def left_todos(session_id, con=None, tasks=None):
    """[dict(subject, status)] a session left open (status in OPEN_TASK), in_progress first, then by file
    number: tasks/<id>/<N>.json, else the todos of the newest session_episode reflection csr-engine stored for it.
    A file that cannot be read is skipped; no database, or no table, gives []."""
    if not session_id or not re.fullmatch(r'[\w-]+', session_id):
        return []
    tasks, found = tasks or TASKS, []  # TASKS read now, so a stand-in folder can be put in its place
    try:
        names = os.listdir(os.path.join(tasks, session_id))
    except OSError:
        names = []
    for name in names:
        stem, ext = os.path.splitext(name)
        if ext != '.json' or not stem.isdigit():
            continue
        try:
            with open(os.path.join(tasks, session_id, name), encoding='utf-8') as handle:
                task = json.load(handle)
            if task.get('status') in OPEN_TASK and isinstance(task.get('subject'), str) and task['subject'].strip():
                found.append((int(stem), dict(subject=one_line(task['subject'], 300), status=task['status'])))
        except (OSError, ValueError, AttributeError):
            continue
    if not found and con is not None:
        try:
            row = con.execute("select content from reflections where tags like '%session_episode%' and tags like ? "
                              "order by timestamp desc limit 1", (f'%conv_{session_id}%',)).fetchone()
            for k, todo in enumerate(json.loads(row[0]).get('todos') or [] if row else []):
                if isinstance(todo, dict) and todo.get('status') in OPEN_TASK and isinstance(todo.get('content'), str) \
                        and todo['content'].strip():
                    found.append((k, dict(subject=one_line(todo['content'], 300), status=todo['status'])))
        except (sqlite3.Error, ValueError, AttributeError, TypeError):
            return []
    return [task for _, task in sorted(found, key=lambda f: (f[1]['status'] != 'in_progress', f[0]))]


def thin_series(points, cuts, limit):
    """At most limit points plus two per segment: every k-th point, and each segment's first and last (a
    segment runs from one compaction to the next)."""
    if len(points) <= limit:
        return points
    keep = set(range(0, len(points), -(-len(points) // limit)))
    times = [p[0] for p in points]
    edges = [0] + [bisect.bisect_left(times, c) for c in sorted(cuts)] + [len(points)]
    for low, high in zip(edges, edges[1:]):
        if high > low:
            keep |= {low, high - 1}
    return [points[i] for i in sorted(keep)]


def previous_facts(prev):
    """The facts the default view draws about the session before this one, JSON-safe, so the transcript (tens
    of MB) is read once. dict(id, path, mtime, title, first, last, active, prompts, compacts, peak, floor,
    learned, handed, asked, series [[t, context]], prompt_times, compactions [[t, pre, post]], todos,
    said, typed). learned and handed are None when the database cannot be read."""
    main = sorted(prev.main)
    last = max([t for t, _ in main] + list(prev.prompts) + list(prev.compacts) + [prev.said[0] if prev.said else 0,
                                                                                 prev.typed[-1][0] if prev.typed else 0])
    floor, measured = floor_of(prev.compact_sizes)
    try:
        con = connect()
        try:
            sid = prev.id
            learned = con.execute('select count(*) from chunks where conversation_id=?', (sid,)).fetchone()[0]
            handed = handoffs([epoch(r[0]) for r in con.execute(
                'select retrieved_at from retrieval_events where session_id=?', (sid,))]) \
                + con.execute('select count(*) from correction_deliveries where target_session_id=?', (sid,)).fetchone()[0]
            todos = left_todos(sid, con)
        finally:
            con.close()
    except Exception:  # sqlite3.Error, or anything a stand-in raises: the facts without the database's counts
        learned = handed = None
        todos = left_todos(prev.id)
    try:
        mtime_ = os.stat(prev.path).st_mtime
    except OSError:
        mtime_ = 0
    points = thin_series([[round(t, 1), c] for t, c in main], prev.compacts, SERIES_MAX)
    cut = lambda pair: [pair[0], one_line(pair[1], 400)] if pair else None
    return dict(id=prev.id, path=prev.path, mtime=mtime_, title=prev.title, first=prev.started, last=last,
                active=Clock(list(prev.prompts) + [t for t, _ in main], last).total, prompts=len(prev.prompts),
                compacts=len(prev.compacts), peak=max([c for _, c in main] + [0]), floor=floor if measured else None,
                learned=learned, handed=handed, asked=len(prev.asked), series=points,
                prompt_times=[round(t, 1) for t in prev.prompts],
                compactions=[[t, pre, post] for t, (pre, post, _) in zip(prev.compacts, prev.compact_sizes)],
                todos=todos, said=cut(prev.said), typed=cut(prev.typed[-1] if prev.typed else None))


def read_previous(session_id):
    """The facts kept for a session's previous session: a dict ({} when there was none), or None when none are kept."""
    try:
        with open(os.path.join(KEEP, PREVIOUS_DIR, f'{session_id}.json'), encoding='utf-8') as handle:
            got = json.load(handle)
        return got if isinstance(got, dict) else None
    except (OSError, ValueError):
        return None


def keep_json(folder, name, record):
    private.write_atomic(os.path.join(KEEP, folder, f'{name}.json'), private.dumps(record), root=KEEP)


def carry_into(session):
    """In the background, once per session: the session before this one, read once, gives its threads (kept for
    --lost too) and the facts the default view draws. What is already kept is read back, never worked out
    again: by now that earlier session may have been resumed and no longer count as the one before. Kept
    first, then shown, so the two always agree. A pane opened before the first prompt has no transcript yet:
    the live session's record gives the folder and the start."""
    try:
        record = session.record or {}
        path = session.path or (os.path.join(project_folder(record['cwd']), f'{session.id}.jsonl') if record.get('cwd') else None)
        started = session.started or record.get('started') or 0.0
        carried, facts = read_carried(session.id), read_previous(session.id)
        if carried is None or facts is None:
            prev = previous_read(path, started)
            if carried is None:
                carried = carried_threads(path, started, prev) if prev else []
                keep_json(CARRY_DIR, session.id, carried)
            if facts is None:
                facts = previous_facts(prev) if prev else {}
                keep_json(PREVIOUS_DIR, session.id, facts)
        session.carried, session.previous = carried, facts
    except Exception:  # a carry that fails leaves this session's own threads, as before
        session.carried = session.carried or []
        session.previous = {}


def read_carried(session_id):
    """The carry kept for a session, or None when none was kept."""
    try:
        with open(os.path.join(KEEP, CARRY_DIR, f'{session_id}.json'), encoding='utf-8') as handle:
            got = json.load(handle)
        return got if isinstance(got, list) else None
    except (OSError, ValueError):
        return None


def lost_json(session_id):
    """--lost: the threads of this session a person has not dropped, for the pane's own tools and the
    compaction hook."""
    session = Session(session_id) if session_id else None
    if session:
        session.read_transcript()
    carried = (read_carried(session_id) or []) if session else []
    return [t for t in lost_found(session.summaries if session else [], session.typed if session else [], carried,
                                  project_of(session and session.path)) if t['hidden'] not in ('drop', 'handed')]


def lost_asks(thread):
    """The two things a thread can be asked to become, as the words typed into Claude's prompt (unsent)."""
    quiet = f"Lost and found: “{fit(thread['text'], 240)}” went quiet after {datetime.fromtimestamp(thread['last_seen']).strftime('%H:%M')}."
    return dict(
        side=one_line(f"{quiet} Revisit it as a side task: finish or close it in a few steps without derailing the "
                      f"current work, then tell me in one line."),
        sub=one_line(f"{quiet} Hand it to a background subagent with a five-part spec (objective, files, interfaces, "
                     f"constraints, verification), keep working on the current task, and report its result when it lands."))


def lost_sections(s, now, most=3):
    """The Lost and found section: the top `most` threads that went quiet or wait on you, once there is any."""
    every = (s or {}).get('lost') or []
    shown = [t for t in every if not t['hidden'] and t['rank'] < 3]  # rank is the thread's kind: lost, or open and waiting
    when = lambda t: datetime.fromtimestamp(t).strftime('%m-%d %H:%M')
    items = []
    for k, t in enumerate(shown[:most]):
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
    return [dict(name='lost', title='Lost and found', right=f'{len(shown)} quiet · f all {len(every)}', items=items, empty='',
                 count=len(shown), at=latest(t['last_seen'] for t in shown[:most]),
                 waits=any(t['waiting'] for t in shown[:most]), brief=f'{len(shown)} quiet')]


LOST_ASKS = (('side', '→ side task'), ('sub', '→ subagent'), ('drop', '✗ drop'), ('later', 'later'))


def lost_rows(item, width, ui, is_open):
    """A thread's answers under its row (the top thread's always, any other's once opened), two to a row;
    each row is its own block, so a click knows the thread and, by its column, the answer."""
    if not item.get('lost') or not (item.get('top') or is_open):
        return []
    at = gutter(ui)
    sent = ui.get('sent')
    half = (width - at) // 2
    rows, pairs = [], (LOST_ASKS[:2], LOST_ASKS[2:])
    if sent and sent[0] == item['key']:  # the asks' row says where the words went; drop and later stay a click away
        said = "✓ now in Claude's prompt" if sent[1] else "✗ couldn't reach Claude's pane"
        rows, pairs = [([' ' * at + ink('faint') + fit(said, width - at) + OFF], None)], pairs[1:]
    for pair in pairs:
        text, spans = ' ' * at, []
        for k, (answer, label) in enumerate(pair):
            spans.append((at + k * half, at + (k + 1) * half, answer))
            text += ink('turn') + BOLD + label[0] + OFF + ink('quiet') + fit(label[1:], half - 2).ljust(half - 1) + OFF
        rows.append(([text], dict(key=f"{item['key']}:{pair[0][0]}", lost=item['lost'], item=item['key'],
                                  asks=item['asks'], spans=spans)))
    return rows


def lost_click(pane, act, col):
    """A click on a thread's answers: drop and later are kept in the log; the two asks are typed into
    Claude's prompt, unsent, and kept as sent (the value line counts the threads you took up)."""
    pick = next((answer for start, end, answer in act['spans'] if start <= col < end), None)
    project = project_of(getattr(getattr(pane, 'session', None), 'path', None))
    if pick in LOST_ANSWERS:
        keep_lost(act['lost'], pick, time.time(), project)
    elif pick:
        agent = agent_pane(pane.agent)
        went = send_text(agent, act['asks'][pick])
        pane.sent = (act['item'], went, time.time())
        if went:
            keep_lost(act['lost'], 'sent', time.time(), project, how=pick)
        focus_back()  # it is in Claude's prompt now: the next keys belong there


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


ERROR_LOG = os.path.join(KEEP, 'pane-errors.log')  # each failed frame's traceback, newest last
LOG_MAX = 1 << 20  # past this many bytes the log moves to pane-errors.log.1 and starts again
BACKOFF = (1, 2, 5, 10, 30)  # seconds before each retry after a frame fails, the last one repeated
RELAUNCH_AFTER = 3  # frames failed in a row before the pane starts again through its guard
GUARD = os.path.join(os.path.dirname(os.path.realpath(__file__)), 'pane_guard.py')
GUARDED = 'CSR_PANE_GUARD'  # pane_guard.py sets it: the pane runs under the guard
RELAUNCH = 75  # the exit that asks the guard to start the pane again (EX_TEMPFAIL)


def log_error(text, path=None):
    """Append one failure to the error log, which first moves aside to .1 once it passes LOG_MAX. Never raises."""
    path = path or ERROR_LOG
    try:
        if os.path.getsize(path) >= LOG_MAX:
            os.replace(path, path + '.1')
    except OSError:
        pass  # no log yet, or nowhere to write one
    try:
        private.append(path, private.redact(text if text.endswith('\n') else text + '\n'), root=KEEP)
    except OSError:
        pass


def error_summary(error):
    """The failure, short: 'NameError: TAPE_MIN · pane.py:3871', the deepest line of this file it ran through."""
    said = (str(error).splitlines() or [''])[0]
    missing = re.fullmatch(r"name '(\w+)' is not defined", said)
    said = missing.group(1) if missing else said
    frames = traceback.extract_tb(error.__traceback__)
    mine = [f for f in frames if os.path.realpath(f.filename) == os.path.realpath(__file__)] or frames
    where = f'{os.path.basename(mine[-1].filename)}:{mine[-1].lineno}' if mine else ''
    return ' · '.join(x for x in (type(error).__name__ + (f': {fit(said, 60)}' if said else ''), where) if x)


class Boundary:
    """What keeps the pane alive when a frame fails: the failure as a card over the top row, the last good frame
    left under it; its traceback in ERROR_LOG; the next try put off by BACKOFF. RELAUNCH_AFTER failures in a
    row ask for a fresh start through the guard."""

    def __init__(self, clock=None):
        self.failures, self.due, self.last = 0, 0.0, None
        self.clock = clock or time.time

    def ready(self):
        """Whether the next try is due: always, until a frame fails."""
        return not self.failures or self.clock() >= self.due

    def run(self, call, *args):
        """call(*args); True when it ran. An Exception from it is caught, logged, carded, and backed off."""
        try:
            call(*args)
        except Exception as error:  # SystemExit and KeyboardInterrupt still end the pane
            self.failures += 1
            wait = BACKOFF[min(self.failures, len(BACKOFF)) - 1]
            self.due, self.last = self.clock() + wait, error_summary(error)
            log_error(f"{datetime.now().isoformat(timespec='seconds')} pid {os.getpid()} failure {self.failures}: "
                      f"{self.last}\n{traceback.format_exc()}")
            sys.stdout.write(self.card(wait))
            sys.stdout.flush()
            return False
        self.failures, self.last = 0, None
        return True

    def card(self, wait):
        """The error card at the top in vermilion, wrapped over at most 4 rows: what failed, where, and when it
        tries again. The rows under it keep the last good frame."""
        cols = shutil.get_terminal_size((60, 30)).columns
        text = f'CSR pane error · {self.last} · retry in {wait}s'
        colours = '\x1b[48;2;%d;%d;%dm\x1b[38;2;%d;%d;%dm' % (PIGMENT['vermilion'] + PIGMENT['chalk']) + BOLD
        return ''.join(f'\x1b[{row + 1};1H' + colours + ' ' + line + '\x1b[K' + OFF
                       for row, line in enumerate(wrap(text, max(1, cols - 2), 4)))


def restart(source, failed=None):
    """Start the pane again with this file as it is now, in place, so herdr and the band above Claude's prompt
    still find its pid. Under the guard: a hot reload runs the file again, a failure exits RELAUNCH for the guard
    to retry, failed (what failed) its last line on stderr for the guard to show. Outside it, both go through
    the guard, which catches an edit that fails as it loads."""
    argv = [sys.executable] + (['-I'] if sys.flags.isolated else [])
    if os.environ.get(GUARDED) == '1':
        if failed:
            sys.stderr.write(f'{failed}\n')
            sys.exit(RELAUNCH)
        os.execv(sys.executable, argv + [source] + sys.argv[1:])
    if os.path.exists(GUARD):
        os.execv(sys.executable, argv + [GUARD] + sys.argv[1:])
    os.execv(sys.executable, argv + [source] + sys.argv[1:])


HEARTBEAT_EVERY = 5  # seconds between touches of the pane's pid file; statusline.cjs takes one older than 30 s as gone
HEARTBEAT_STALL = 120  # seconds the loop may not come round (one frame running) before the pane is taken as hung


class Heartbeat:
    """Touches the pane's pid file every HEARTBEAT_EVERY seconds from a thread of its own, so a frame that runs long
    (a transcript read, a busy database) never ages it past statusline.cjs's 30 s. PR #317 review: it was touched only
    after a frame returned, and Claude's status line went back under the prompt. The loop says it came round
    (alive); with no word from it for `stall` seconds the pane is hung, the file ages, and the line goes back
    under the prompt. Stopped on the way out, by a quit or a reload."""

    def __init__(self, path, every=None, stall=None):
        self.path = path
        self.every = HEARTBEAT_EVERY if every is None else every
        self.stall = HEARTBEAT_STALL if stall is None else stall
        self.seen = time.monotonic()
        self.stopped = threading.Event()
        self.thread = threading.Thread(target=self.beat, name='csr-pane-heartbeat', daemon=True)

    def start(self):
        if self.path:
            self.thread.start()
        return self

    def alive(self):
        self.seen = time.monotonic()

    def beat(self):
        while not self.stopped.wait(self.every):
            if time.monotonic() - self.seen > self.stall:
                continue
            try:
                os.utime(self.path)
            except OSError:
                pass

    def stop(self):
        self.stopped.set()
        if self.thread.is_alive():
            self.thread.join(timeout=1)


def watch(root, limit, session_id, agent=None):
    pane = Pane(root, limit, session_id, agent)
    source = os.path.realpath(__file__)
    loaded = seen = mtime(source)
    reload = failed = pressed = False
    boundary = Boundary()
    redraw = {'now': True}
    signal.signal(signal.SIGWINCH, lambda *_: redraw.update(now=True))
    for ending in (signal.SIGTERM, signal.SIGHUP):  # closed or killed: still clear up below
        signal.signal(ending, lambda *_: sys.exit(0))
    mine = kept_file(PANES, session_id)
    if mine:  # while this file names a live pid, Claude's status lines draw here, not under its prompt
        private.write_atomic(mine, json.dumps(dict(pid=os.getpid())), root=KEEP)
    beat = Heartbeat(mine).start()  # a pid file older than HEARTBEAT_S (statusline.cjs) names a pane that is gone
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd) if os.isatty(fd) else None
    if saved:
        tty.setcbreak(fd)
    sys.stdout.write('\x1b]0;CSR\x07\x1b[?25l' + BLACK + (MOUSE_ON if saved else ''))
    try:
        last, last_key = 0.0, 0.0
        while True:
            beat.alive()
            # hot reload: an edit to this file, once it has stood still a tick and compiles, runs in place
            now_seen = mtime(source)
            if now_seen != loaded and now_seen == seen:
                pane.held, loaded = compiles(source), now_seen
                if pane.held is None:
                    reload = True
                    break
                redraw['now'] = True
            seen = now_seen
            redraw['now'] = redraw['now'] or bool(fail_open(compaction_gone, False, pane))  # a compaction ended: draw it now
            failing = boundary.failures > 0  # then a frame is tried when its retry is due, or at a key
            if (boundary.ready() or pressed) if failing else (redraw['now'] or time.time() - last > 2):
                if not boundary.run(pane.frame, redraw['now'] or failing):
                    pane.shown = pane.drawn = None  # the screen is no longer the last frame: the next one starts clear
                redraw['now'], last, pressed = False, time.time(), False
                if boundary.failures >= RELAUNCH_AFTER:
                    reload = failed = True
                    break
            elif not failing and fail_open(pane.tick, False):  # between frames: the countdown, its row alone
                redraw['now'] = True  # it ran out: the next turn of the loop reads and draws
            try:
                ready, _, _ = select.select([sys.stdin] if saved else [], [], [], 1.0)
            except InterruptedError:
                continue
            if not ready:
                continue
            data = os.read(fd, 1024).decode('utf-8', 'replace')
            pressed = True
            for button, x, y, kind in re.findall(r'\x1b\[<(\d+);(\d+);(\d+)([Mm])', data):
                if kind == 'M' and int(button) == 0:  # a left press; releases pass
                    fail_open(pane.click, None, int(y) - 1, int(x) - 1)
                    redraw['now'] = True
                elif kind == 'M' and int(button) in (64, 65):  # the wheel, up or down
                    fail_open(pane.scroll, None, WHEEL if int(button) == 65 else -WHEEL)
                    redraw['now'] = True
            keys = re.sub(r'\x1b\[<[\d;]*[Mm]', '', data).lower()
            typing = time.time() - last_key < 1.0  # another key a moment ago: these are words, not commands
            last_key = time.time() if keys else last_key
            if keys == 'q' and not typing:
                break
            if keys == '\x1b':
                pane.open, pane.tall, pane.main_top, redraw['now'] = None, False, 0, True
            if fail_open(lambda: summary_keys(pane, keys) or threads_keys(pane, keys) or info_keys(pane, keys), False):
                redraw['now'] = True
            elif pane.view == 'main' and any(k in keys for k in ('j', 'k', ' ', '\x1b[a', '\x1b[b')):
                fail_open(pane.scroll, None, -WHEEL if any(k in keys for k in ('k', '\x1b[a')) else 4 * WHEEL if ' ' in keys else WHEEL)
                redraw['now'] = True
            for key in keys:
                if key in ('d', 'l'):
                    pane.view, pane.open = ('main' if key == 'd' else 'log'), None
                    redraw['now'] = True
                elif key == 'r':  # read now; the countdown starts again
                    reload_now(pane)
                    redraw['now'] = True
    except KeyboardInterrupt:
        pass
    finally:
        beat.stop()
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
        restart(source, failed and (boundary.last or 'frames kept failing'))


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
    mode.add_argument('--compact-plan', action='store_true', help="the payback of compacting now, and what CSR's compactions did")
    mode.add_argument('--compact-usual', action='store_true', help='how long your own /compact takes, for the card')
    parser.add_argument('--ctx', type=int, help='the context now, in tokens (with --compact-plan)')
    args = parser.parse_args()
    if args.compact_plan:
        print(json.dumps(fail_open(compact_plan_json, None, args.sid, args.ctx or 0)))
        return
    if args.compact_usual:
        print(json.dumps(keep_compact_usual(time.time())))
        return
    if args.coach:
        print(json.dumps(coach_terms()))
        return
    if args.keep:
        record = json.loads(args.keep)
        if record.get('kind') in ('dream', 'look', 'chapter', 'ysk', 'compact'):
            record['repo'] = pane_root(args.repo)
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
    root = pane_root(args.repo)
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
        data['git'] = bool(repo_root(root))  # False: remembered, but no commits to show changes from
        data['sample'] = sample_dream()  # the mod shows it while its own options say dreams are off
        print(json.dumps(public(data)))
    elif args.png:
        now, cell = time.time(), (16, 34)
        data = load(root, args.limit, args.sid)
        data.update(look=last_look(root), chapter=last_chapter(root, args.sid), story=session_story(args.sid),
                    readings=how_claude_read(args.sid), limits=account_limits(now), sts=csr_status(now))
        data['claude_fields'], data['claude_at'] = kept_status(args.sid, now)
        data['claude_line'] = claude_status(args.sid, now)
        os.makedirs(args.png, exist_ok=True)
        if args.sid:
            session = Session(args.sid)
            session.record = session_record(args.sid)
            carry_into(session)  # here, not in a thread: the picture needs the session before this one
            s = session.load()
            open(os.path.join(args.png, 'session.png'), 'wb').write(session_png(s, 40, cell, now))
            lines, pictures = draw_main(data, s, 46, 30, now)
            open(os.path.join(args.png, 'screen.txt'), 'w', encoding='utf-8').write('\n'.join(lines) + '\n')
            if s.get('last_session'):
                png = last_png(s['last_session'], 43, 3, cell, tint('last', ROW))
                if png:
                    open(os.path.join(args.png, 'last.png'), 'wb').write(png)
        print(args.png)
    else:
        if not root:
            print('not a git repository, and CSR has no sessions from here:', args.repo)
            return
        watch(root, args.limit, args.sid, args.agent)


if __name__ == '__main__':
    main()
