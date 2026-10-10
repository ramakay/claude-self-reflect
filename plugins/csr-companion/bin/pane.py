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
  pane.py --repo PATH --watch [--sid ID] [--agent PANE]  full-screen pane: d main, l log, q quits;
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
    dreams, look = load_dream_log(root), last_look(root)
    week = [d for d in dreams if d['at'] >= time.time() - 7 * 86400]
    return dict(repo=root, total=len(shifts), shifts=shifts[:limit], changes=changes, dreams_on=dreams_on(),
                recalled=recalled, 
                changes_total=len(changes), dreams=dreams, look=look, dreams_7d=len(week),
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
    return dict({k: v for k, v in data.items() if k not in ('shifts', 'changes', 'dreams', 'look')},
                shifts=[shift(s) for s in data['shifts']], changes=changes, dreams=dreams)


# ---------------------------------------------------------------- the dreamer's log and memory

def keep(record):
    """One line in the dream log: a dream the mod showed, or what the person answered to it."""
    os.makedirs(KEEP, exist_ok=True)
    with open(DREAM_LOG, 'a', encoding='utf-8') as handle:
        handle.write(json.dumps(record, ensure_ascii=False) + '\n')


SETTLED = ('helpful', 'not_relevant', 'ask_claude', 'thought_of_it', 'got_it', 'dismiss', 'ignored', 'turn_off')


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
)


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

class Session:
    """This session, read as it goes: the transcript from where it was left, CSR's rows fresh."""

    def __init__(self, session_id):
        self.id = session_id
        found = glob.glob(os.path.join(HOME, '.claude', 'projects', '*', f'{session_id}.jsonl'))
        self.path = found[0] if found else None
        self.offset = 0
        self.replies = {}  # message id -> (time, context tokens, output tokens)
        self.prompts, self.compacts, self.asked, self.questions = [], [], [], []
        self.tools, self.files, self.edited = Counter(), Counter(), set()
        self.shell, self.last = Counter(), {}  # files a command named; when Claude last handled each
        self.talk = []  # (time, words) the person typed or Claude wrote, newest last; no tool output
        self.title = None

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
            if kind == 'assistant':
                message = entry.get('message') or {}
                usage = message.get('usage') or {}
                context = sum(usage.get(k) or 0 for k in
                              ('input_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens'))
                if message.get('id') and when:
                    self.replies[message['id']] = (when, context, usage.get('output_tokens') or 0,
                                                   usage.get('cache_creation_input_tokens') or 0)
                for part in message.get('content') or []:
                    if isinstance(part, dict) and part.get('type') == 'text' and when and not entry.get('isSidechain'):
                        self.talk.append((when, part.get('text') or ''))
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
                    if not entry.get('isSidechain'):
                        self.talk.append((when, own_words('\n'.join(texts))))
            elif kind == 'attachment' and when:
                queued = entry.get('attachment') or {}
                text = str(queued.get('prompt') or '').strip()
                if queued.get('type') == 'queued_command' and queued.get('humanTurn') and text \
                        and not text.startswith(('<', '/')):
                    self.prompts.append(when)
            elif kind == 'system' and entry.get('subtype') == 'compact_boundary' and when:
                self.compacts.append(when)
            elif kind == 'ai-title' and entry.get('aiTitle'):
                self.title = entry['aiTitle']
        del self.talk[:-200]

    def load(self):
        self.read_transcript()
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
                    compacts=sorted(self.compacts), asked=sorted(self.asked), tools=self.tools, learned=learned,
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
               changed='quiet', remembered='recall', kept='turn')
GLYPH = dict(session='◉', touching='‹›', risk='~', rules='◆', dreams='☾', memory='↑', changed='~', remembered='↑',
             kept='◆')  # where no pictures are drawn
DARK = (PIGMENT['payne'], PIGMENT['night'])  # the masthead's ground, from its first row (Claude's own status
# line) to its last (the gauges' legend), each row a step darker in CIELAB; payne sits clear of a dark
# terminal's own ground (herdr's is 40), so the masthead keeps its edge.
ON_DARK = dict(text=PIGMENT['chalk'], label=blend(PIGMENT['chalk'], PIGMENT['payne'], 0.45), you=PIGMENT['chalk'],
               ctx=PIGMENT['indigo_lit'], learn=PIGMENT['viridian_lit'], recall=PIGMENT['viridian_lit'],
               turn=PIGMENT['vermilion_lit'], dream=PIGMENT['ochre_lit'], asked=PIGMENT['violet_lit'],
               rule=(255, 255, 255, 40))
TINT = 0.15  # an opened row: its section's ink at this strength on the paper
HEAD = 0.10  # a section's heading row
ROW = 0.04  # its items, one tint for all of them
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
    if name == 'touching':  # the files Claude has open: a sheet of code on its folder, one line just edited
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
    image.alpha_composite(icon, ((w - icon.width) // 2, (h - icon.height) // 2))
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
    return [tinted(row, band(name)) for row in rows + more + [last]]


STAR = ink('asked') + '☾' + OFF  # CSR's mark, as in the band above Claude's prompt
MARK = {'changed': '\x1b[33m~', 'removed': '\x1b[31m-', 'came back': '\x1b[32m+'}
ANSWER_MARK = dict(helpful='\x1b[32m✓', ask_claude='\x1b[36m→', thought_of_it='\x1b[2m=', not_relevant='\x1b[31m✗',
                   got_it='\x1b[35m·', dismiss='\x1b[2m·', ignored='\x1b[2m·', turn_off='\x1b[2m○')
ANSWER_WORD = dict(helpful='helpful', ask_claude='asked Claude', thought_of_it="you'd thought of it",
                   not_relevant='not relevant', got_it='read', dismiss='dismissed', ignored='passed',
                   turn_off='turned dreams off')


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


def page_main(data, s, now):
    """What a senior architect wants beside the agent, the star first, each shown only with something in it:
      What if…          the dreamer's question, its reasoning shown, answered from here
      Handed to Claude  what CSR's memory gave Claude this session: past sessions and notes
      Your rules        corrections you typed yourself in this repo
    Above them, the session's gauges: your prompts, Claude's context and CSR's memory at work. Code that
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

    week = sum(1 for d in data['dreams'] if d['at'] >= now - 7 * 86400)
    dreams = []
    newest = data['dreams'][0] if data['dreams'] else None
    starred = bool(newest) and newest['answer'] in (None, 'ignored', 'dismiss', 'got_it')  # still yours to answer
    look = data.get('look')
    if data.get('dreams_on') and look and not starred:
        when = stamp(look['at'], now)[0]
        every = look.get('every')
        dreams.append(dict(key='look', quiet=True, lines=2,
                           title=f"No open question. Last look {'just now' if when == 'now' else when}: "
                                 f"{LOOKED.get(look['outcome'], look['outcome'])}.",
                           meta=f"next after {every} of Claude's steps, if CSR has something new" if every else '',
                           detail=['The dreamer looks only when CSR remembers something new near this work, '
                                   'waits longer each time it finds nothing, and asks only what stands on '
                                   'another session.'],
                           send='/shifts dreams'))
    for k, d in enumerate(data['dreams'][:3 - len(dreams)]):
        reasons = [re.sub(r'^[*-]\s*', '', line) for line in (d.get('text') or '').splitlines() if line.strip()]
        send = one_line(f"{DREAMER_SAID}: “{d['line']}” Its reasoning: {d.get('title') or ''}. {' '.join(reasons[:3])}")
        meta = ' · '.join(x for x in (short_date(d['at']), ANSWER_WORD.get(d['answer'], '')) if x)
        if k == 0 and starred:  # the star: its question whole, its reasoning shown, its answers a click away
            dreams.append(dict(key='dream:' + d['id'], dream=d['id'], title=d['line'], lines=3,
                               body=[d.get('title') or ''] + reasons[:1], answers=True, detail=reasons[1:3],
                               send=send))
        else:
            dreams.append(dict(key='dream:' + d['id'], title=d['line'], lines=1, meta=meta,
                               detail=[d.get('title') or ''] + reasons[:2], send=send))

    # Only what Claude reached for: a hand-off it then opened, or a question it asked CSR itself; at most
    # two rows. The rest stays behind one row with the count, which opens to the list.
    handed, passive = [], []
    for item in s.get('remembered') or []:
        if item['kind'] == 'recall':
            where = ' · '.join(x for x in (from_when(item.get('when'), now), item.get('project'),
                                           f"{item['times']}×" if item['times'] > 1 else '') if x)
            gist = re.sub(r'\*\*|(?:^|\s)#+\s*', ' ', item['text']).strip()  # a note's markdown, as words
            if item.get('opened') and len(handed) < 2:
                handed.append(dict(recall_act(item, now), title=gist, meta='opened · ' + where))
            else:
                passive.append(f"{gist} · {where}" if where else gist)
        elif len(handed) < 2:
            handed.append(dict(key=f"asked:{item['at']}", title=item['text'], meta='Claude asked CSR',
                               detail=[f"Through {item.get('tool') or 'a CSR tool'}."],
                               send=one_line(f"Claude asked CSR: “{item['text']}”")))
    given = handoffs(s['gave']['recalls']) + len(s['gave']['carried']) if s.get('gave') else 0
    opened = sum(1 for item in s.get('remembered') or [] if item.get('opened'))
    if given:
        handed.append(dict(key='handed:all', quiet=True, lines=1,
                           title=f"{given} handed · {f'{opened} opened' if opened else 'none opened'}",
                           detail=passive[:5] or ['Nothing left to list.'],
                           send=one_line('CSR handed you these this session: ' + '; '.join(passive[:5]))))

    clock = Clock(s['prompts'] + [r[0] for r in s['replies']], now)
    facts = [f"{len(s['prompts'])} prompts", span(clock.total)]
    if s['compacts']:
        facts.append(f"{len(s['compacts'])} compacts")
    return dict(head=dict(facts=' · '.join(facts), session=s if s.get('learned') else None,
                          status=data.get('claude_line')),
                quiet='Nothing to report yet. A section shows here once CSR has something for it.',
                sections=[
        dict(name='dreams', title='What if…', right=('on' + (f' · {week} this week' if week else '')) if data.get('dreams_on') else 'off',
             items=dreams, empty=''),
        dict(name='remembered', title='Handed to Claude', right='', items=handed, empty=''),
        dict(name='rules', title='Your rules', right='', items=rules, empty=''),
    ])


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
    title = wrap(item['title'].replace('`', ''), room, 6 if is_open else item.get('lines', 2))
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
    if head.get('title'):
        top.append(' ' + BOLD + fit(head['title'], width - 1) + OFF)
    top.append(' ' + fit(head['facts'], width - 1))
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
    built = []
    for section in page['sections']:
        blocks = []
        for item in section['items']:
            is_open = ui.get('open') == item['key']
            rows_ = item_rows(item, section['name'], width, is_open)
            if is_open:
                rows_ = opened(rows_, section['name'], item.get('detail') or [], width, ui, item['key'], indent=ITEM)
            blocks.append((rows_, item))
            blocks += answer_rows(item, width, ui)
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
    return draw_page(page_main(data, s, now), cols, rows, ui or {})


def draw_log(data, cols, rows, ui=None):
    return draw_page(page_log(data), cols, rows, ui or {})






def legend(view, width):
    """The bottom row: its keys in lamp black, what each does in pencil; a click on it does the first."""
    keys = [('l', 'changes since your sessions'), ('q', 'close')] if view == 'main' else \
        [('d', 'back'), ('q', 'close')]
    marks = '' if view == 'main' else '~ changed  - removed  + back   '
    words = [(key, said) for key, said in keys]
    while len(marks) + sum(len(k) + len(w) + 4 for k, w in words) > width and len(words[0][1]) > 7:
        words[0] = (words[0][0], words[0][1].rsplit(' ', 1)[0])
    shown = '   '.join(f'{BOLD}{ink("you")}{key}{OFF}{ink("faint")} {said}{OFF}' for key, said in words)
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

    def frame(self, force):
        cols, rows = shutil.get_terminal_size((60, 30))
        now = time.time()
        try:
            if self.data is None or now - self.loaded > 60:
                self.data, self.loaded = load(self.root, self.limit, self.session and self.session.id), now
            self.data['dreams'] = load_dream_log(self.root)  # the mod appends while we watch
            self.data['look'] = last_look(self.root)
            self.data['claude_line'] = claude_status(self.session and self.session.id, now)
            s = self.session.load() if self.session and self.view == 'main' else None
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
        ui = dict(open=self.open, sent=self.sent, answered=self.answered)
        sig = json.dumps([self.view, cols, rows, self.data['total'], self.data['dreams_on'], self.open, self.sent,
                          self.answered,
                          self.data.get('claude_line'),
                          [(d['id'], d['answer']) for d in self.data['dreams']], self.data.get('look'),
                          s and [len(s['replies']), len(s['prompts']), len(s['learned']['chunks']), s['title'],
                                 len(s['learned']['turns']), len(s['gave']['recalls']), len(s['remembered'])]])
        if sig == self.shown and not force:
            return
        self.shown = sig
        if self.view == 'main':
            lines, pictures = draw_main(self.data, s, cols, rows, now, ui)
        else:
            lines, pictures = draw_log(self.data, cols, rows, ui)
        self.acts = {row: act for row, act in getattr(lines, 'acts', {}).items() if row < rows - 1}
        self.acts[rows - 1] = dict(key='view:log' if self.view == 'main' else 'view:main')
        lines = lines[:rows - 1]
        whole = any(h >= rows for _, _, _, h, _ in pictures)  # a paper page draws its own legend
        screen = ['\x1b_Ga=d,d=A\x1b\\', '\x1b[H\x1b[2J', '\n'.join(lines)]
        if not whole:
            screen += [f'\x1b[{rows};1H', legend(self.view, cols - 1)]
        screen += [kitty(png, r, c, w, h) for r, c, w, h, png in pictures if r + h <= rows]
        screen.append(f'\x1b[{rows};{cols}H')
        sys.stdout.write(''.join(screen))
        sys.stdout.flush()

    def click(self, row, col=0):
        """One click opens a row; a second click on it puts it in Claude's prompt, unsent. On a dream's
        answers, the half clicked is the answer: kept in the dream log, and "ask Claude" also typed in."""
        act = self.acts.get(row)
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
        elif act['key'] != self.open:
            self.open, self.sent = act['key'], None
        else:
            agent = agent_pane(self.agent)
            went = bool(agent) and herdr('pane', 'send-text', agent, act['send']) is not None
            self.sent = (act['key'], went, time.time())
        focus_back()


def watch(root, limit, session_id, agent=None):
    pane = Pane(root, limit, session_id, agent)
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
            if redraw['now'] or time.time() - last > (2 if pane.sent else 10):
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
                if kind == 'M' and int(button) == 0:  # a left press; releases and the wheel pass
                    pane.click(int(y) - 1, int(x) - 1)
                    redraw['now'] = True
            keys = re.sub(r'\x1b\[<[\d;]*[Mm]', '', data).lower()
            if 'q' in keys:
                break
            if keys == '\x1b':
                pane.open, redraw['now'] = None, True
            for key in keys:
                if key in ('d', 'l'):
                    pane.view, pane.open = ('main' if key == 'd' else 'log'), None
                    redraw['now'] = True
    except KeyboardInterrupt:
        pass
    finally:
        if mine:  # only while it still names this pane: a pane opened in its place has written its own
            try:
                with open(mine, encoding='utf-8') as handle:
                    if json.load(handle).get('pid') == os.getpid():
                        os.remove(mine)
            except (OSError, ValueError, AttributeError):
                pass
        if saved:
            termios.tcsetattr(fd, termios.TCSADRAIN, saved)
        sys.stdout.write('\x1b_Ga=d,d=A\x1b\\' + (MOUSE_OFF if saved else '') + GROUND_BACK + '\x1b[?25h\n')


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
    mode.add_argument('--keep', help='a dream or an answer, as JSON, for the dream log')
    args = parser.parse_args()
    if args.keep:
        record = json.loads(args.keep)
        if record.get('kind') in ('dream', 'look'):
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
