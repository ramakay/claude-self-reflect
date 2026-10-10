"""The CSR pane and the dreamer's memory, read from csr-engine.

Read-only against csr-engine, no model calls here. The pane is a page of paper (one picture, its own
type, where the terminal draws pictures; text elsewhere), what a senior architect wants first:
  Touching now         the code Claude reads and edits, by how much of the rest depends on it.
  Decisions at risk    past sessions' work on code a later commit changed: what they concluded may not hold.
  Your rules           corrections you made and approaches you dropped in this repo.
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
    dreams = load_dream_log(root)
    week = [d for d in dreams if d['at'] >= time.time() - 7 * 86400]
    return dict(repo=root, total=len(shifts), shifts=shifts[:limit], changes=changes, dreams_on=dreams_on(),
                recalled=recalled, 
                changes_total=len(changes), dreams=dreams, dreams_7d=len(week),
                dream_tokens_7d=sum(d.get('tokens') or 0 for d in week),
                corrections_carried_7d=carried, shifts_told_7d=told, reminded_7d=carried + told)


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
    return dict({k: v for k, v in data.items() if k not in ('shifts', 'changes', 'dreams')},
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


def brief(con, root, session_id):
    """What CSR remembers near this session, with receipts, for the dreamer's prompt."""
    session = Session(session_id)
    session.read_transcript()
    touched = [path for path, _ in session.files.most_common(8)]
    files = [path for path in touched if path.startswith(root + '/')]
    out = []
    if touched:
        out.append('Files this session worked on: ' + ', '.join(
            f"{os.path.relpath(path, root) if path in files else '/'.join(path.split('/')[-3:])} ({session.files[path]}×)"
            for path in touched) + '.')
    notes, seen = [], set()
    for path in files[:6]:
        for note in notes_for(con, root, path, session_id):
            if note['id'] not in seen:
                seen.add(note['id'])
                notes.append(note)
    changed = [n for n in notes if n['kind'] == 'shift'][:5]
    rules = [n for n in notes if n['kind'] == 'rule'][:3]
    if changed:
        out += ['', "Code that changed after one of the person's past sessions worked on it:"]
        out += [f"- {n['line']} [commit {n['oid'][:7]}, session {n['conversation'][:8]}]" for n in changed]
    if rules:
        out += ['', 'Rules the person set for these files:']
        out += [f"- {n['line']} [session {n['conversation'][:8]}]" for n in rules]
    project = os.path.basename(root)
    corrections = con.execute(
        "select ts, quote, session_id from intent_events where kind = 'correction' and session_id != ?1 "
        "and (project = ?2 or project like ?2 || '-%') order by ts desc limit 3", (session_id, project)).fetchall()
    if corrections:
        out += ['', 'Corrections CSR recorded in this repo lately:']
        out += [f"- {short_date(epoch(ts)) if epoch(ts) else (ts or '')[:10]}: {quoted(quote, 200)} [session {sid[:8]}]"
                for ts, quote, sid in corrections]
    counts, near = voice_and_lines(con, session_id)
    if near:
        out += ['', 'Lines the person wrote in past sessions that sit close to this work:']
        out += [f"- {short_date(when)} ({project}): {quoted(part, 220)} [session {sid[:8]}]"
                for _, when, sid, project, part in near]
    if counts:
        out += ['', 'How the person talks to the agent, from their prompt history: ' + ', '.join(
            f'"{name}" {n:,}×' for name, n in counts.most_common()) + '.']
    dreams = load_dream_log(root)[:8]
    if dreams:
        out += ['', 'Dreams already offered here, and what the person answered:']
        out += [f"- {short_date(d['at'])}: {quoted(d['line'], 200)} [{d['answer'] or 'no answer'}]" for d in dreams]
    return '\n'.join(out).strip()


# ---------------------------------------------------------------- what CSR remembered for you

EDITS = ('Edit', 'Write', 'MultiEdit', 'NotebookEdit')
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


def remembered_items(con, recalls, questions, session_id):
    """What CSR handed Claude this session, newest first: one row per past session or reflection, and
    each time Claude asked CSR itself."""
    items = {}
    for at, kind, memory, conversation, parent, _ in recalls:
        key, about = memory_of(con, kind, memory, conversation, parent)
        if not about or key == session_id:
            continue
        item = items.setdefault(key, dict(kind='recall', key=key, at=0, text=about[0], when=about[1], project=about[2],
                                          times=0))
        item['at'], item['times'] = max(item['at'], epoch(at) or 0), item['times'] + 1
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
                    if isinstance(part, dict) and part.get('type') == 'tool_use':
                        self.tools[part.get('name')] += 1
                        path = (part.get('input') or {}).get('file_path')
                        if isinstance(path, str) and path.startswith('/'):
                            self.files[os.path.realpath(path)] += 1
                            if part.get('name') in EDITS:
                                self.edited.add(os.path.realpath(path))
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
                    files=dict(self.files), edited=sorted(self.edited))


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

# pigment's light-ground inks (its nyt archetype): the pane is a page of paper, black ink on it, and
# each section keeps one pigment for its icon, its label and its marks, dark enough to read as text.
GROUND = (251, 251, 249)
INK = dict(you=(23, 25, 28), ctx=(44, 90, 160), learn=(35, 122, 92), turn=(200, 68, 42), dream=(138, 100, 20),
           recall=(35, 122, 92), asked=(94, 86, 160), sepia=(111, 86, 66), rule=(0, 0, 0, 34),
           quiet=(94, 94, 89), faint=(141, 140, 134), hair=(226, 225, 219))
SECTION = dict(session='you', touching='ctx', risk='turn', rules='sepia', dreams='dream', memory='recall',
               changed='quiet', remembered='recall', kept='turn')
GLYPH = dict(session='◉', touching='‹›', risk='~', rules='◆', dreams='☾', memory='↑', changed='~', remembered='↑',
             kept='◆')  # where no pictures are drawn
TINT = 0.08  # an opened row: its section's ink at this strength on the paper
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


def session_png(s, cols, cell, now):
    """Three lanes over active session time: your prompts, Claude's context, CSR's memory at work."""
    image, draw = canvas(cols, 3, cell)
    w, lane = image.width, image.height / 3
    learned, gave = s['learned'], s['gave']
    clock = Clock(s['prompts'] + [r[0] for r in s['replies']], now)
    x = lambda t: 2 * SS + clock.at(t) * (w - 4 * SS)
    for i in (1, 2):
        draw.line([(0, i * lane), (w, i * lane)], fill=INK['rule'], width=SS)
    for b in clock.breaks:
        bx = 2 * SS + b / clock.total * (w - 4 * SS)
        for y in range(0, int(3 * lane), 6 * SS):
            draw.line([(bx, y), (bx, y + 2 * SS)], fill=(255, 255, 255, 70), width=SS)
    for t in s['compacts']:
        draw.line([(x(t), lane), (x(t), 2 * lane)], fill=(255, 255, 255, 110), width=SS)
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


_ICONS = {}


def draw_icon(draw, name, box, color, width):
    """A section's icon in pigment's hairline (one ink, round strokes) inside box = (x, y, size)."""
    x0, y0, size = box
    at = lambda fx, fy: (x0 + fx * size, y0 + fy * size)

    def stroke(*points, thin=1.0):
        pts, w = [at(*p) for p in points], max(1, round(width * thin))
        draw.line(pts, fill=color, width=w, joint='curve')
        for x, y in (pts[0], pts[-1]):  # round caps
            draw.ellipse([x - w / 2, y - w / 2, x + w / 2, y + w / 2], fill=color)

    def disc(cx, cy, r, fill):
        (x, y), (edge, _) = at(cx, cy), at(cx + r, cy)
        draw.ellipse([x - (edge - x), y - (edge - x), edge, y + (edge - x)], fill=fill)

    if name == 'session':  # a pulse: your prompts and Claude's replies over time
        stroke((0.0, 0.56), (0.26, 0.56), (0.38, 0.18), (0.54, 0.9), (0.66, 0.4), (0.74, 0.56), (1.0, 0.56))
    elif name == 'touching':  # code: the brackets around what Claude has open
        stroke((0.36, 0.14), (0.06, 0.5), (0.36, 0.86))
        stroke((0.64, 0.14), (0.94, 0.5), (0.64, 0.86))
    elif name in ('memory', 'remembered'):  # handed up to Claude, out of the tray of past sessions
        stroke((0.1, 0.56), (0.1, 0.9), (0.9, 0.9), (0.9, 0.56))
        stroke((0.5, 0.7), (0.5, 0.08))
        stroke((0.27, 0.31), (0.5, 0.08), (0.73, 0.31))
    elif name == 'dreams':  # a crescent and a spark
        disc(0.42, 0.56, 0.4, color)
        disc(0.6, 0.4, 0.34, draw.paper)
        stroke((0.84, 0.04), (0.84, 0.36), thin=0.7)
        stroke((0.68, 0.2), (1.0, 0.2), thin=0.7)
    elif name in ('rules', 'kept'):  # a bookmark
        stroke((0.26, 0.06), (0.74, 0.06), (0.74, 0.94), (0.5, 0.7), (0.26, 0.94), (0.26, 0.06))
    else:  # risk, changed: a commit on its line
        stroke((0.0, 0.5), (0.28, 0.5))
        stroke((0.72, 0.5), (1.0, 0.5))
        disc(0.5, 0.5, 0.24, color)
        disc(0.5, 0.5, 0.24 - width / size, draw.paper)


_ICONS = {}


def icon_png(name, cell):
    """A section's icon, two cells wide, for the text pane in a pixel terminal."""
    if (name, cell) in _ICONS:
        return _ICONS[name, cell]
    image, draw = canvas(2, 1, cell)
    draw.paper = (0, 0, 0, 0)
    size = min(image.width, image.height) * 0.8
    draw_icon(draw, name, ((image.width - size) / 2, (image.height - size) / 2, size), INK[SECTION[name]][:3] + (255,),
              max(2 * SS, round(size * 0.1)))
    _ICONS[name, cell] = shrink(image)
    return _ICONS[name, cell]




# ---------------------------------------------------------------- the screen

DIM, BOLD, OFF = '\x1b[2m', '\x1b[1m', '\x1b[0m'
BLACK, GROUND_BACK = '\x1b]11;rgb:fb/fb/f9\x1b\\', '\x1b]111\x1b\\'  # the pane's own paper, and the terminal's back
MOUSE_ON, MOUSE_OFF = '\x1b[?1000h\x1b[?1006h', '\x1b[?1000l\x1b[?1006l'  # clicks, as SGR reports


def ink(name):
    r, g, b = INK[name][:3]
    return f'\x1b[38;2;{r};{g};{b}m'


def band(name):
    r, g, b = (round(p + (i - p) * TINT) for i, p in zip(INK[SECTION[name]][:3], GROUND))
    return f'\x1b[48;2;{r};{g};{b}m'


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


def opened(rows, name, detail, width, ui, key):
    """A row a click opened: its whole text, what more CSR knows, and what a second click does."""
    sent = ui.get('sent')
    if sent and sent[0] == key:
        hint = "✓ now in Claude's prompt" if sent[1] else "✗ couldn't reach Claude's pane"
    else:
        hint = "↳ click again → Claude's prompt"
    room = width - INDENT
    more = [' ' * INDENT + DIM + line + OFF for text in detail for line in wrap(text, room, 2)][:4]
    last = ' ' * INDENT + ink(SECTION[name]) + fit(hint, room) + OFF
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


def heading(name, title, right, width):
    """A section's head in the text pane: its mark and title in its ink, a count at the right; no band."""
    lead = f' {ink(SECTION[name])}{GLYPH[name]}{OFF} '
    used = 1 + len(GLYPH[name]) + 1
    room = width - used
    gap = room - len(title) - len(right) - 1
    if gap < 1:
        right, gap = '', room - len(title) - 1
    return lead + ink(SECTION[name]) + BOLD + fit(title, room - 1) + OFF + ' ' * max(1, gap) + ink('faint') + right + OFF






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








def main_path(path):
    """A worktree's file as the main checkout names it: the code graph and the ledger know that path."""
    for mark in NESTED:
        if mark in path:
            head, tail = path.split(mark, 1)
            return head + '/' + tail.split('/', 1)[1] if '/' in tail else path
    return path


def file_facts(con, path, session_id):
    """How much of the code depends on a file, and how many of your past sessions worked on it."""
    callers, symbols = con.execute('select coalesce(sum(r.in_degree), 0), count(n.id) from code_nodes n '
                                   'left join code_node_rank r on r.node_id = n.id where n.file=?', (path,)).fetchone()
    top = con.execute('select n.name, r.in_degree from code_nodes n join code_node_rank r on r.node_id = n.id '
                      'where n.file=? and r.in_degree > 0 and n.kind != \'import\' order by r.in_degree desc limit 3',
                      (path,)).fetchall()
    shaped = con.execute('select count(distinct s) from (select first_conv_id s from code_nodes where file=?1 '
                         'union select last_conv_id from code_nodes where file=?1 '
                         'union select session_id from episode_anchors where file=?1) where s != \'\' and s != ?2',
                         (path, session_id or '')).fetchone()[0]
    return dict(callers=callers, symbols=symbols, top=top, shaped=shaped)


def short_path(rel, parts=3):
    return '/'.join(rel.split('/')[-parts:])


def page_main(data, s, now):
    """What a senior architect wants beside the agent, most first:
      Touching now      the code Claude reads and edits, by how much of the rest depends on it
      Decisions at risk past sessions' work on code that a later commit changed: what they concluded may not hold
      Your rules        corrections you made and approaches you dropped, in this repo
      What if…          the dreamer's challenges
    The past sessions CSR handed Claude are counted in the masthead."""
    root = data['repo']
    con = connect()
    try:
        files = {}
        for path, n in (s.get('files') or {}).items():
            main = main_path(path)
            if not main.startswith(root + '/') or any(mark in main for mark in NESTED):
                continue
            seen = files.setdefault(main, dict(path=main, uses=0, edited=False))
            seen['uses'] += n
            seen['edited'] = seen['edited'] or path in (s.get('edited') or ())
        touching, risks = [], {}
        for seen in files.values():
            rel = seen['path'][len(root) + 1:]
            facts = file_facts(con, seen['path'], s['id'])
            notes = notes_for(con, root, seen['path'], s['id'])
            moved = [n for n in notes if n['kind'] == 'shift']
            for note in moved:
                risks.setdefault(note['id'], note)
            meta = ['edited' if seen['edited'] else 'read']
            if facts['callers']:
                meta.append(f"{facts['callers']} call sites")
            if facts['shaped']:
                meta.append(f"{facts['shaped']} past sessions")
            top = ', '.join(f'{name} ({n})' for name, n in facts['top'])
            detail = ([f'Most depended on: {top}.'] if top else []) + [n['line'] for n in notes[:2]]
            send = (f"Before we change {rel}: {facts['callers']} call sites depend on it" +
                    (f", most on {', '.join(name for name, _ in facts['top'])}" if facts['top'] else '') + '. ' +
                    ' '.join(n['line'] for n in notes[:2]))
            touching.append(dict(key='file:' + rel, title=short_path(rel), mono=True, meta=' · '.join(meta),
                                 flag=f'{len(moved)} changed since' if moved else '', detail=detail,
                                 send=one_line(send), rank=(not seen['edited'], -facts['callers'], -seen['uses'])))
        touching.sort(key=lambda item: item['rank'])

        at_risk = [dict(key='note:' + n['id'], title=n['line'], meta=f"{n['oid'][:7]} · {short_date(n['at'])}",
                        detail=[n['text']], send=one_line(n['text'])) for n in risks.values()]
        for change in data['changes']:
            if len(at_risk) >= 4:
                break
            latest = max(change['shifts'], key=lambda x: x['at'])
            symbols = list(dict.fromkeys(x['symbol'] or os.path.basename(x['file']) for x in change['shifts']))
            files_moved = list(dict.fromkeys(short_path(x['file'], 2) for x in change['shifts']))
            key = f"change:{change['conversation']}"
            if any(item['key'] == key for item in at_risk):
                continue
            title = (f"{', '.join(symbols[:2])}{' +' + str(len(symbols) - 2) if len(symbols) > 2 else ''} "
                     f"in {files_moved[0]}" + (f' and {len(files_moved) - 1} more files' if len(files_moved) > 1 else ''))
            at_risk.append(dict(
                key=key, title=title,
                meta=f"your {short_date(change['talked'])} session · changed in {latest['oid'][:7]} on {latest['date'][5:]}",
                detail=[f"That session: {quoted(change['title'], 120)}",
                        f"Commit {latest['oid'][:7]} {quoted(latest['subject'], 80)}"],
                send=one_line(f"Our {short_date(change['talked'])} session ({quoted(change['title'], 80)}) worked on "
                              f"{', '.join(symbols[:4])}; commit {latest['oid'][:7]} {quoted(latest['subject'], 70)} changed "
                              f"them on {latest['date']}. What that session concluded may no longer hold.")))

        projects = {p for p in (s.get('project'), os.path.basename(root)) if p}
        marks = ','.join('?' * len(projects))
        rules = []
        for when, kind, quote, session in con.execute(
                f"select ts, kind, quote, session_id from intent_events where project in ({marks}) "
                f"and kind in ('correction', 'abandoned') order by ts desc limit 5", tuple(projects)).fetchall():
            day = short_date(epoch(when)) if epoch(when) else (when or '')[:10]
            here = ' · this session' if session == s['id'] else ''
            rules.append(dict(key=f'rule:{when}:{quote[:30]}', title=quote,
                              meta=('you corrected' if kind == 'correction' else 'dropped') + f' · {day}{here}',
                              detail=[f'Session {session[:8]}. CSR keeps it and hands it to Claude when the work comes near.'],
                              send=one_line(f"A standing rule from {day}: “{quote}”" if kind == 'correction' else
                                            f"On {day} we dropped an approach: “{quote}”")))
    finally:
        con.close()

    week = sum(1 for d in data['dreams'] if d['at'] >= now - 7 * 86400)
    dreams = [dict(key='dream:' + d['id'], title=d['line'],
                   meta=' · '.join(x for x in (short_date(d['at']), ANSWER_WORD.get(d['answer'], '')) if x),
                   detail=[d.get('title') or ''] + [re.sub(r'^[*-]\s*', '', line) for line in
                                                    (d.get('text') or '').splitlines() if line.strip()][:2],
                   send=one_line(f"CSR's dreamer asked: “{d['line']}”")) for d in data['dreams'][:3]]
    clock = Clock(s['prompts'] + [r[0] for r in s['replies']], now)
    facts = [f"{len(s['prompts'])} prompts", span(clock.total)]
    if s['compacts']:
        facts.append(f"{len(s['compacts'])} compacts")
    facts.append(f"{len(s['gave']['recalls']) + len(s['gave']['carried'])} memories handed to Claude")
    return dict(head=dict(kicker='CSR · this session', title=s.get('title') or 'This session', facts=' · '.join(facts)),
                sections=[
        dict(name='touching', title='Touching now', right=f'{len(touching)} files' if touching else '', items=touching[:5],
             empty='Nothing yet. As Claude reads and edits code here, each file shows how much depends on it '
                   'and what your past sessions did there.'),
        dict(name='risk', title='Decisions at risk', right=f"{data['changes_total']}" if data['changes_total'] else '',
             items=at_risk[:4], empty='No commit has changed code your past sessions worked on.'),
        dict(name='rules', title='Your rules', right='', items=rules[:4],
             empty='No corrections yet. When you tell Claude "no, not that", CSR keeps it here.'),
        dict(name='dreams', title='What if…', right=('on' + (f' · {week} this week' if week else '')) if data.get('dreams_on') else 'off',
             items=dreams, empty=('The dreamer asks when CSR has something new near this work.' if data.get('dreams_on') else
                                  'A side agent that asks "what if…?" while Claude works. Turn it on in /config.')),
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
                sections=[dict(name='changed', title='Changed since', right=str(data['total']), items=items,
                               empty='Nothing changed under past conversations here.')])


MARK_WORD = {'changed': '~', 'removed': '−', 'came back': '+'}

# ---------------------------------------------------------------- paper: the page drawn as one picture

PAPER_FONTS = dict(
    sans=['/System/Library/Fonts/SFNS.ttf', '/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf', 'DejaVuSans.ttf'],
    serif=['/System/Library/Fonts/NewYork.ttf', '/System/Library/Fonts/Supplemental/Charter.ttc',
           '/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf', 'DejaVuSerif.ttf'],
    mono=['/System/Library/Fonts/SFNSMono.ttf', '/System/Library/Fonts/Menlo.ttc',
          '/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf', 'DejaVuSansMono.ttf'])
_FONTS = {}


def paper_font(kind, size, weight=None, axes=None):
    key = (kind, size, weight, axes)
    if key not in _FONTS:
        from PIL import ImageFont
        font = None
        for candidate in PAPER_FONTS[kind]:
            try:
                font = ImageFont.truetype(candidate, size)
                break
            except OSError:
                continue
        if font is None:
            font = ImageFont.load_default(size)
        try:
            if axes:  # New York: a text optical size, as its display default draws H without a crossbar
                font.set_variation_by_axes(list(axes))
            elif weight:
                font.set_variation_by_name(weight)
        except Exception:  # not a variable font: the regular weight stands
            pass
        _FONTS[key] = font
    return _FONTS[key]


def wrap_px(text, font, width, most):
    """Words to lines no wider than width pixels, at most `most` of them, an ellipsis if cut."""
    words, lines, cur = ' '.join(str(text).split()).split(' '), [], ''
    for i, word in enumerate(words):
        trial = f'{cur} {word}' if cur else word
        if font.getlength(trial) <= width or not cur:
            cur = trial
            continue
        lines.append(cur)
        cur = word
        if len(lines) == most:
            cur = None
            break
    if cur:
        lines.append(cur)
    cut = len(lines) > most or cur is None
    lines = lines[:most]
    if cut and lines:
        last = lines[-1]
        while last and font.getlength(last + '…') > width:
            last = last[:-1]
        lines[-1] = last.rstrip() + '…'
    return lines


def mix(color, amount):
    return tuple(round(p + (c - p) * amount) for c, p in zip(color[:3], GROUND))


LEGEND = dict(main='l  changed since your sessions   ·   q  quit',
              log='d  back   ·   ~ changed   − removed   + back   ·   q  quit')


def paper_png(page, cols, rows, cell, ui, footer=''):
    """The page on paper, cols x rows cells: black ink, hairline rules, an icon and a pigment per section,
    type sized to the cell, not to the terminal's font. Returns the PNG and where each row a click acts
    on lies, as (top, bottom, item) in pixels."""
    from PIL import Image, ImageDraw
    w, h = cols * cell[0], rows * cell[1]
    image = Image.new('RGB', (w, h), GROUND)
    draw = ImageDraw.Draw(image)
    draw.paper = GROUND
    body = max(11, round(cell[1] * 0.4))
    small = round(body * 0.82)
    label = round(body * 0.7)
    line, gap = round(body * 1.5), round(body * 0.6)
    f_body, f_small = paper_font('sans', body), paper_font('sans', small)
    f_label = paper_font('sans', label, 'Semibold')
    f_mono, f_title = paper_font('mono', round(body * 0.92)), paper_font('serif', round(body * 1.3), axes=(28, 600, 0))
    pad = round(body * 1.2)
    x0, x1 = pad, w - pad
    hits, y = [], pad
    ink_, quiet, faint, hair = INK['you'][:3], INK['quiet'][:3], INK['faint'][:3], INK['hair'][:3]
    if footer:  # the legend, small, in the last row: the terminal's font draws nothing on this page
        h -= cell[1]
        draw.line([(x0, h), (x1, h)], fill=hair, width=max(1, round(body / 14)))
        draw.text((x0, h + cell[1] * 0.62), footer, font=paper_font('sans', round(body * 0.78)), fill=faint, anchor='ls')

    def text(x, baseline, words, font, fill):
        draw.text((x, baseline), words, font=font, fill=fill, anchor='ls')

    def tracked(x, baseline, words, font, fill, track):
        for ch in words:
            text(x, baseline, ch, font, fill)
            x += font.getlength(ch) + track
        return x

    head = page['head']
    y += label
    tracked(x0, y, head['kicker'].upper(), f_label, faint, label * 0.12)
    y += round(label * 0.6)
    for words in wrap_px(head['title'], f_title, x1 - x0, 2):
        y += round(body * 1.6)
        text(x0, y, words, f_title, ink_)
    for words in wrap_px(head['facts'], f_small, x1 - x0, 2):
        y += round(small * 1.5)
        text(x0, y, words, f_small, quiet)
    y += gap

    for section in page['sections']:
        color = INK[SECTION[section['name']]][:3]
        if y + line * 3 > h:
            break
        y += gap
        draw.line([(x0, y), (x1, y)], fill=hair, width=max(1, round(body / 14)))
        y += round(label * 2.1)
        icon = round(label * 1.25)
        draw_icon(draw, section['name'], (x0, y - icon + round(label * 0.12), icon), color, max(1, round(body / 9)))
        tracked(x0 + icon + round(label * 0.7), y, section['title'].upper(), f_label, color, label * 0.12)
        if section['right']:
            text(x1 - f_small.getlength(section['right']), y, section['right'], f_small, faint)
        y += round(label * 0.5)
        if not section['items']:
            for words in wrap_px(section['empty'], f_small, x1 - x0, 3):
                y += round(small * 1.45)
                text(x0, y, words, f_small, faint)
            y += round(gap * 0.6)
            continue
        indent = x0 + round(body * 1.1)
        for item in section['items']:
            is_open = ui.get('open') == item['key']
            font = f_mono if item.get('mono') else f_body
            title = wrap_px(item['title'].replace('`', ''), font, x1 - indent, 6 if is_open else 2)
            meta = item.get('meta', '')
            extra = []
            if is_open:
                extra = [words for d in item.get('detail') or [] if d
                         for words in wrap_px(d.replace('`', ''), f_small, x1 - indent, 3)][:6]
            need = round(gap * 0.7) + len(title) * line + round(small * 1.5) * (1 + len(extra) + (1 if is_open else 0))
            if y + need > h - round(pad * 0.5):
                more = len(section['items']) - section['items'].index(item)
                y += round(small * 1.5)
                text(indent, y, f'+{more} more', f_small, faint)
                break
            top = y + round(gap * 0.35)
            if is_open:
                draw.rounded_rectangle([x0 - round(body * 0.5), top, x1 + round(body * 0.5), y + need + round(gap * 0.3)],
                                       radius=round(body * 0.4), fill=mix(color, TINT))
            y += round(gap * 0.7)
            for i, words in enumerate(title):
                y += line
                if i == 0:
                    r = max(2, round(body * 0.16))
                    cy = y - round(body * 0.34)
                    draw.ellipse([x0 + r, cy - r, x0 + 3 * r, cy + r], fill=color)
                text(indent, y, words, font, ink_)
            y += round(small * 1.5)
            x = indent
            if item.get('flag'):
                text(x, y, item['flag'], f_small, INK['turn'][:3])
                x += f_small.getlength(item['flag'] + ' · ')
                if meta:
                    text(x - f_small.getlength(' · '), y, ' · ', f_small, faint)
            text(x, y, fit(meta, 200), f_small, faint)
            for words in extra:
                y += round(small * 1.5)
                text(indent, y, words, f_small, quiet)
            if is_open:
                sent = ui.get('sent')
                hint = (("✓ now in Claude's prompt" if sent[1] else "✗ couldn't reach Claude's pane")
                        if sent and sent[0] == item['key'] else "click again → Claude's prompt")
                y += round(small * 1.5)
                text(indent, y, hint, f_small, color)
            hits.append((top, y + round(gap * 0.3), item))
        y += round(gap * 0.4)
    out = io.BytesIO()
    image.save(out, 'PNG', optimize=True)
    return out.getvalue(), hits


def page_lines(page, width, rows, ui):
    """The page as text, for a terminal that draws no pictures: the same sections and rows."""
    head = page['head']
    out = Lines([' ' + ink('faint') + fit(head['kicker'].upper(), width - 1) + OFF,
                 ' ' + BOLD + fit(head['title'], width - 1) + OFF,
                 ' ' + ink('quiet') + fit(head['facts'], width - 1) + OFF])
    room = dict(touching=9, risk=7, rules=6, dreams=6, memory=5, changed=rows)
    for section in page['sections']:
        out.put([''])
        part = Lines([heading(section['name'], section['title'], section['right'], width)])
        if not section['items']:
            part.put(['   ' + ink('faint') + l + OFF for l in wrap(section['empty'], width - 3, 2)])
        else:
            blocks = []
            for item in section['items']:
                is_open = ui.get('open') == item['key']
                tail = ' · '.join(x for x in (item.get('flag'), item.get('meta')) if x)
                rows_ = entry('', ink(SECTION[section['name']]) + '•', item['title'].replace('`', ''), tail, width,
                              rows=6 if is_open else 2)
                if is_open:
                    rows_ = opened(rows_, section['name'], item.get('detail') or [], width, ui, item['key'])
                blocks.append((rows_, item))
            fit_blocks(part, blocks, room.get(section['name'], 6), ui, tail=False)
        base = len(out)
        out.extend(part)
        out.acts.update({base + i: act for i, act in part.acts.items()})
    return out


def draw_page(page, cols, rows, ui, view='main'):
    """The page for the screen: a picture of paper where the terminal draws pictures, its legend in it,
    text elsewhere. Either way, which screen rows a click acts on."""
    if PIXELS:
        cell = cell_size()
        png, hits = paper_png(page, cols, rows, cell, ui, footer=LEGEND[view])
        lines = Lines([''] * (rows - 1))
        for row in range(rows - 1):
            middle = (row + 0.5) * cell[1]
            for top, bottom, item in hits:
                if top <= middle <= bottom:
                    lines.acts[row] = item
        return lines, [(0, 0, cols, rows, png)]
    return page_lines(page, max(24, cols - 1), rows, ui), []


def draw_main(data, s, cols, rows, now, ui=None):
    if s is None:
        s = dict(id=None, title=None, prompts=[], replies=[], compacts=[], gave=dict(recalls=[], carried=[], dreams=[]),
                 remembered=[], sessions=0, project=None, files={}, edited=[])
    return draw_page(page_main(data, s, now), cols, rows, ui or {})


def draw_log(data, cols, rows, ui=None):
    return draw_page(page_log(data), cols, rows, ui or {}, view='log')






def legend(view, width):
    text = 'l changed since your sessions · q' if view == 'main' else '~ changed  - removed  + back · d back · q'
    return DIM + fit(text, width) + OFF


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

    def frame(self, force):
        cols, rows = shutil.get_terminal_size((60, 30))
        now = time.time()
        try:
            if self.data is None or now - self.loaded > 60:
                self.data, self.loaded = load(self.root, self.limit, self.session and self.session.id), now
            self.data['dreams'] = load_dream_log(self.root)  # the mod appends while we watch
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
        ui = dict(open=self.open, sent=self.sent)
        sig = json.dumps([self.view, cols, rows, self.data['total'], self.data['dreams_on'], self.open, self.sent,
                          [(d['id'], d['answer']) for d in self.data['dreams']],
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

    def click(self, row):
        """One click opens a row; a second click on it puts it in Claude's prompt, unsent."""
        act = self.acts.get(row)
        if not act:
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
            for button, _, y, kind in re.findall(r'\x1b\[<(\d+);(\d+);(\d+)([Mm])', data):
                if kind == 'M' and int(button) == 0:  # a left press; releases and the wheel pass
                    pane.click(int(y) - 1)
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
        if record.get('kind') == 'dream':
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
