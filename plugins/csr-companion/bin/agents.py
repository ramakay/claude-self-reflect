"""The agents of a session, as the plugin's hooks keep them for the pane.

hooks/agents.ts writes <HOME>/.claude-self-reflect/csr-companion/agents/<session>.json from the typed events of Claude
Code (agent.spawn, tool.call, turn.step, turn.complete, $.agent.list()). This module reads that file back, checked: a
file of the wrong version or session, malformed, too big, a link, or unreadable is no file, and reads as no agents.
It also puts the agents in tree order for the pane's In flight section.

Nothing here writes, spawns a process or reads a transcript. The file is a bounded projection of what the hooks know,
not the record of what happened: a missing agent in it is not a finished one.
"""
import json
import os
import re
import stat

VERSION = 1
MAX_BYTES = 16384
MAX_AGENTS = 12
MAX_PATHS = 8
STALL_SECONDS = 10 * 60  # a running agent no event has touched for this long is shown as stalled
RECENT_SECONDS = 30 * 60  # a completion this recent keeps In flight open
SESSION = re.compile(r'[A-Za-z0-9_-]{1,128}')
AGENT_ID = re.compile(r'[A-Za-z0-9_.:@-]{1,128}')
STATUSES = ('pending', 'running', 'waiting', 'idle', 'completed', 'failed', 'killed', 'unknown')
ACTIVE = ('pending', 'running', 'waiting')
REASONS = ('answer', 'aborted', 'refusal', 'error')
DISPOSITIONS = ('pending', 'claimed', 'looked', 'skipped', 'failed')
CONTROL = re.compile(r'[\x00-\x08\x0b-\x1f\x7f-\x9f]')


def empty():
    """The projection of a session with no agents on record."""
    return dict(agents=[], omitted_agents=0, at=None)


def number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and value == value and abs(value) < 1e18


def count(value):
    return isinstance(value, int) and not isinstance(value, bool) and 0 <= value < 10 ** 9


def text(value, most):
    """A string from the file with no control left in it, at most `most` characters; None when it is not a string."""
    return CONTROL.sub('', value)[:most] if isinstance(value, str) else None


def agent_id(value):
    return isinstance(value, str) and bool(AGENT_ID.fullmatch(value))


def paths(value):
    if not isinstance(value, list) or len(value) > MAX_PATHS or not all(isinstance(item, str) for item in value):
        return None
    return [CONTROL.sub('', item)[:300] for item in value]


def completion(value):
    """A row's completion, checked; False when it is malformed (None is a row with none)."""
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != {'key', 'turnId', 'at', 'reason', 'answer', 'disposition'}:
        return False
    if not (isinstance(value['key'], str) and len(value['key']) <= 600 and agent_id(value['turnId']) and number(value['at'])
            and value['reason'] in REASONS and isinstance(value['answer'], str) and value['disposition'] in DISPOSITIONS):
        return False
    return dict(key=value['key'], turn_id=value['turnId'], at=value['at'], reason=value['reason'],
                answer=CONTROL.sub('', value['answer'])[:700], disposition=value['disposition'])


def row_of(raw):
    """One agent of the file, checked and cleaned; None when it is not a well-formed row."""
    keys = ('id', 'parent', 'description', 'type', 'model', 'status', 'started', 'first_seen', 'last_at', 'tools', 'tokens',
            'edits', 'edits_omitted', 'overlaps', 'overlaps_omitted', 'last', 'last_tool', 'current_turn', 'completion', 'listed')
    if not isinstance(raw, dict) or set(raw) != set(keys):
        return None
    done = completion(raw['completion'])
    edits, overlaps = paths(raw['edits']), paths(raw['overlaps'])
    texts = {k: (None if raw[k] is None else text(raw[k], 300)) for k in ('description', 'type', 'model', 'last', 'last_tool')}
    if (not agent_id(raw['id']) or not (raw['parent'] is None or agent_id(raw['parent'])) or raw['status'] not in STATUSES
            or texts['description'] is None or texts['last'] is None
            or any(raw[k] is not None and texts[k] is None for k in ('type', 'model', 'last_tool'))
            or not (raw['started'] is None or number(raw['started'])) or not number(raw['first_seen']) or not number(raw['last_at'])
            or not all(count(raw[k]) for k in ('tools', 'tokens', 'edits_omitted', 'overlaps_omitted'))
            or edits is None or overlaps is None or done is False
            or not (raw['current_turn'] is None or agent_id(raw['current_turn'])) or not isinstance(raw['listed'], bool)):
        return None
    return dict(id=raw['id'], parent=raw['parent'], status=raw['status'], started=raw['started'], first_seen=raw['first_seen'],
                last_at=raw['last_at'], tools=raw['tools'], tokens=raw['tokens'], edits=edits, edits_omitted=raw['edits_omitted'],
                overlaps=overlaps, overlaps_omitted=raw['overlaps_omitted'], current_turn=raw['current_turn'],
                completion=done, listed=raw['listed'], **texts)


def check(raw, session):
    """The projection a parsed file holds, or empty() when it is not this session's file of this version."""
    if not isinstance(raw, dict) or set(raw) != {'version', 'session', 'at', 'agents', 'omitted_agents'}:
        return empty()
    if raw['version'] != VERSION or raw['session'] != session or not number(raw['at']) or not count(raw['omitted_agents']):
        return empty()
    if not isinstance(raw['agents'], list) or len(raw['agents']) > MAX_AGENTS:
        return empty()
    rows = [row_of(item) for item in raw['agents']]
    if any(row is None for row in rows) or len({row['id'] for row in rows}) != len(rows):
        return empty()
    return dict(agents=rows, omitted_agents=raw['omitted_agents'], at=raw['at'])


def read_agents(path, session):
    """The agents file at `path` for `session`, checked; empty() for any file that is not exactly that: another session's,
    another version, malformed, over MAX_BYTES, a link or not a regular file, or unreadable."""
    if not (isinstance(session, str) and SESSION.fullmatch(session)) or not path:
        return empty()
    try:
        if not stat.S_ISREG(os.lstat(path).st_mode):  # lstat does not follow: a link is not a regular file
            return empty()
        with open(path, 'rb') as handle:
            raw = handle.read(MAX_BYTES + 1)
    except OSError:
        return empty()
    if len(raw) > MAX_BYTES:
        return empty()
    try:
        return check(json.loads(raw.decode('utf-8')), session)
    except (ValueError, UnicodeDecodeError, RecursionError):
        return empty()


def is_active(row):
    return row['status'] in ACTIVE


def stalled(row, now):
    """Running, and no event for STALL_SECONDS. Waiting, idle and pending are not stalled; the list never moves last_at."""
    return row['status'] == 'running' and now - row['last_at'] / 1000 >= STALL_SECONDS


def no_report(row):
    """An agent that answered with nothing: ended, and not a report to look at."""
    done = row['completion']
    return bool(done and done['reason'] == 'answer' and not done['answer'].strip())


def reported(row):
    """The agent's own report, or '': only a completed run's answer, never a failed or killed one's."""
    done = row['completion']
    return done['answer'].strip() if done and done['reason'] == 'answer' else ''


def tree(agents):
    """The rows depth-first, [(row, depth, parent_unknown)]: active ones first among siblings, then the newest. A parent
    the file does not hold leaves its child a root marked unknown (it is not attached to the main loop); a cycle is cut
    where it closes, so every row is drawn once."""
    by_id = {row['id']: row for row in agents}
    order = sorted(agents, key=lambda row: (not is_active(row), -row['last_at'], row['id']))
    children, roots = {}, []
    for row in order:
        parent = row['parent']
        if parent is not None and parent in by_id and parent != row['id']:
            children.setdefault(parent, []).append(row)
        else:
            roots.append(row)
    out, seen = [], set()

    def visit(row, depth):
        if row['id'] in seen:
            return
        seen.add(row['id'])
        out.append((row, depth, row['parent'] is not None and row['parent'] not in by_id))
        for child in children.get(row['id'], ()):
            visit(child, depth + 1)

    for row in roots:
        visit(row, 0)
    for row in order:  # what a cycle left out of every root's reach
        visit(row, 0)
    return out


def recent_completion(agents, now):
    """The newest completion time (seconds) of a report within RECENT_SECONDS, else None."""
    times = [row['completion']['at'] / 1000 for row in agents if row['completion'] and now - row['completion']['at'] / 1000 <= RECENT_SECONDS]
    return max(times, default=None)
