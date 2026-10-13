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


# ---------------------------------------------------------------- the machine envelope
#
# What the plugin puts in front of a model or into a prompt box, and nowhere else, so that what it wrote is never read back
# as the person's words (csr-engine's provenance guard knows the same grammar, registry entry plugin_agents_v1):
#
#   [[CSR:AGENTS:v1]]
#   <one compact JSON object>
#   [[/CSR:AGENTS:v1]]
#
# Exactly three physical lines and a final newline. The JSON escapes every newline and every [ and ] inside a string
# ([, ]), so a report can never write a delimiter. Two payloads exist: the agents' context (the brief's) and the
# handoff of a request to a background agent (Lost and found's). This module is the one formatter; hooks/agents.ts parses.
OPEN = '[[CSR:AGENTS:v1]]'
CLOSE = '[[/CSR:AGENTS:v1]]'
MAX_ENVELOPE = 16384  # UTF-8 bytes, the three lines together
HANDOFF_INSTRUCTION = "Copy this envelope unchanged into the background agent's prompt."
CONTEXT_KEYS = {'kind', 'session', 'at', 'agents'}
CONTEXT_AGENT_KEYS = {'id', 'parent', 'type', 'model', 'status', 'edits', 'overlaps', 'completion'}
COMPLETION_KEYS = {'turn_id', 'reason', 'answer'}
HANDOFF_KEYS = {'kind', 'session', 'request', 'thread', 'instruction'}
REQUEST = re.compile(r'[A-Za-z0-9_.:-]{1,128}')


def escape_brackets(dumped):
    """Compact JSON with every [ and ] that sits inside a string written as \\u005b and \\u005d; the brackets of the JSON
    itself (an array) are left."""
    out, in_string, escaped = [], False, False
    for char in dumped:
        if in_string:
            if escaped:
                escaped = False
            elif char == '\\':
                escaped = True
            elif char == '"':
                in_string = False
            elif char == '[':
                out.append('\\u005b')
                continue
            elif char == ']':
                out.append('\\u005d')
                continue
        elif char == '"':
            in_string = True
        out.append(char)
    return ''.join(out)


def string_ok(value, most):
    return isinstance(value, str) and len(value) <= most and not CONTROL.search(value.replace('\n', '').replace('\t', ''))


def valid_context(payload):
    """Whether a parsed object is a context payload within the bounds of the agents file."""
    if not isinstance(payload, dict) or set(payload) != CONTEXT_KEYS or payload['kind'] != 'context':
        return False
    if not (isinstance(payload['session'], str) and SESSION.fullmatch(payload['session'])) or not number(payload['at']):
        return False
    agents = payload['agents']
    if not isinstance(agents, list) or len(agents) > MAX_AGENTS:
        return False
    for row in agents:
        if not isinstance(row, dict) or set(row) != CONTEXT_AGENT_KEYS or not agent_id(row['id']):
            return False
        if not (row['parent'] is None or agent_id(row['parent'])) or row['status'] not in STATUSES:
            return False
        if not all(row[k] is None or string_ok(row[k], 300) for k in ('type', 'model')):
            return False
        for key in ('edits', 'overlaps'):
            if not isinstance(row[key], list) or len(row[key]) > MAX_PATHS or not all(string_ok(p, 300) for p in row[key]):
                return False
        done = row['completion']
        if done is not None and not (isinstance(done, dict) and set(done) == COMPLETION_KEYS and agent_id(done['turn_id'])
                                     and done['reason'] in REASONS and string_ok(done['answer'], 700)):
            return False
    return True


def valid_handoff(payload):
    """Whether a parsed object is a handoff payload: only its keys, and its instruction word for word."""
    return (isinstance(payload, dict) and set(payload) == HANDOFF_KEYS and payload['kind'] == 'handoff'
            and isinstance(payload['session'], str) and bool(SESSION.fullmatch(payload['session']))
            and isinstance(payload['request'], str) and bool(REQUEST.fullmatch(payload['request']))
            and isinstance(payload['thread'], str) and bool(re.fullmatch(r'[0-9a-f]{12}', payload['thread']))
            and payload['instruction'] == HANDOFF_INSTRUCTION)


def valid_payload(payload):
    return valid_context(payload) or valid_handoff(payload)


def format_agents_context(payload):
    """The envelope for a payload, three lines and a final newline. ValueError for a payload that is not a context or a
    handoff, or that does not fit MAX_ENVELOPE bytes: nothing is cut here (context_envelope fits a context first)."""
    if not valid_payload(payload):
        raise ValueError('not an agents context or handoff payload')
    body = escape_brackets(json.dumps(payload, ensure_ascii=False, separators=(',', ':')))
    text = f'{OPEN}\n{body}\n{CLOSE}\n'
    if len(text.encode('utf-8')) > MAX_ENVELOPE:
        raise ValueError('the envelope is over 16,384 bytes')
    return text


def parse_envelopes(text):
    """Every complete valid envelope in `text`: [(start, end, payload)], offsets in characters; `end` is past the closing
    line and its newline (the end of the text when the text stops there). A delimiter must start its line (a leading
    byte-order mark is allowed at the very start of the text), the JSON is one line, the closing line follows it, LF and
    CRLF both end lines, and the payload must be valid and the three lines within MAX_ENVELOPE bytes. Anything less is not
    an envelope and is left where it is."""
    if OPEN not in text:
        return []
    lines = text.split('\n')
    starts, at = [], 0
    for line in lines:
        starts.append(at)
        at += len(line) + 1
    found, i = [], 0
    while i + 2 < len(lines):
        head = lines[i][:-1] if lines[i].endswith('\r') else lines[i]
        if i == 0 and head.startswith('\ufeff'):
            head = head[1:]
        middle = lines[i + 1][:-1] if lines[i + 1].endswith('\r') else lines[i + 1]
        tail = lines[i + 2][:-1] if lines[i + 2].endswith('\r') else lines[i + 2]
        if head == OPEN and tail == CLOSE and '\r' not in middle:
            try:
                payload = json.loads(middle)
            except (ValueError, RecursionError):
                payload = None
            size = len(f'{OPEN}\n{middle}\n{CLOSE}\n'.encode('utf-8', 'surrogatepass'))
            if payload is not None and size <= MAX_ENVELOPE and valid_payload(payload):
                last = i + 2 == len(lines) - 1
                end = len(text) if last else starts[i + 2] + len(lines[i + 2]) + 1
                found.append((starts[i], end, payload))
                i += 3
                continue
        i += 1
    return found


def strip_envelopes(text):
    """The text without its complete valid envelopes and their final newline; every other byte as it was."""
    if not isinstance(text, str) or OPEN not in text:
        return text
    out, at = [], 0
    for start, end, _ in parse_envelopes(text):
        out.append(text[at:start])
        at = end
    out.append(text[at:])
    return ''.join(out)


def context_payload(projection, session):
    """The context payload for the agents file's projection (read_agents): every agent with its status, files and, when it
    finished with a report, that report. Failed, killed, interrupted and empty runs carry none: they are context, never
    news."""
    rows = []
    for row in projection['agents'][:MAX_AGENTS]:
        done = row['completion']
        report = dict(turn_id=done['turn_id'], reason='answer', answer=done['answer'][:700]) \
            if done and done['reason'] == 'answer' and done['answer'].strip() else None
        rows.append(dict(id=row['id'], parent=row['parent'], type=row['type'], model=row['model'], status=row['status'],
                         edits=list(row['edits']), overlaps=list(row['overlaps']), completion=report))
    return dict(kind='context', session=session, at=int(projection['at'] or 0), agents=rows)


def context_envelope(projection, session):
    """The context envelope for the brief, fitted: reports shortened and then dropped, then the last agents, until it is
    within MAX_ENVELOPE bytes. '' when there are no agents."""
    payload = context_payload(projection, session)
    if not payload['agents']:
        return ''
    for most in (700, 400, 160, 40, 0):
        for row in payload['agents']:
            if row['completion'] is not None:
                row['completion']['answer'] = row['completion']['answer'][:most]
                if not most:
                    row['completion'] = None
        try:
            return format_agents_context(payload)
        except ValueError as error:
            if not str(error).startswith('the envelope'):
                raise
    while payload['agents']:
        payload['agents'].pop()
        try:
            return format_agents_context(payload) if payload['agents'] else ''
        except ValueError:
            continue
    return ''


# ---------------------------------------------------------------- handing a Lost and found thread to a background agent
#
# Two files per session, each with one writer, so neither can overwrite the other:
#   handoffs/<session>.drafts.jsonl   the pane's: one line for each click on "-> subagent" (request, thread, drafted_at)
#   handoffs/<session>.json           the hooks': what became of each request that was sent, hooks/agents.ts associateHandoff
# A request is only ever tied to an agent by the id in its envelope (see the grammar above), never by likeness of words or
# "the next agent spawned". Ids and words only: no prompt, no report.
HANDOFF_OUTCOMES = ('draft', 'submitted', 'bound', 'succeeded', 'failed')
HANDOFFS_MAX = 40
THREAD = re.compile(r'[0-9a-f]{12}')
HANDOFF_KEYS_ALL = ('request', 'session', 'thread', 'drafted_at', 'submitted_at', 'spawn_tool_use_id', 'agent_id', 'turn_id',
                    'outcome', 'ended_at', 'reason')


def handoff_of(raw, session):
    """One handoff of the state file, checked; None when malformed or another session's."""
    if not isinstance(raw, dict) or set(raw) != set(HANDOFF_KEYS_ALL) or raw['session'] != session:
        return None
    ids = ('spawn_tool_use_id', 'agent_id', 'turn_id')
    if not (isinstance(raw['request'], str) and REQUEST.fullmatch(raw['request']) and isinstance(raw['thread'], str)
            and THREAD.fullmatch(raw['thread']) and number(raw['drafted_at']) and raw['outcome'] in HANDOFF_OUTCOMES):
        return None
    if not all(raw[k] is None or number(raw[k]) for k in ('submitted_at', 'ended_at')):
        return None
    if not all(raw[k] is None or (isinstance(raw[k], str) and len(raw[k]) <= 200 and not CONTROL.search(raw[k])) for k in ids + ('reason',)):
        return None
    return dict(raw)


def read_handoffs(state_path, drafts_path, session):
    """Every handoff of a session, oldest first, as the hooks last wrote them with the pane's drafts that were never sent
    added as 'draft'. A file that is not this session's, malformed or too big reads as having none."""
    if not (isinstance(session, str) and SESSION.fullmatch(session)):
        return []
    out = []
    try:
        if state_path and stat.S_ISREG(os.lstat(state_path).st_mode) and os.lstat(state_path).st_size <= 4 * MAX_BYTES:
            with open(state_path, 'rb') as handle:
                raw = json.loads(handle.read().decode('utf-8'))
            if isinstance(raw, dict) and set(raw) == {'version', 'session', 'handoffs'} and raw['version'] == VERSION \
                    and raw['session'] == session and isinstance(raw['handoffs'], list) and len(raw['handoffs']) <= HANDOFFS_MAX:
                rows = [handoff_of(item, session) for item in raw['handoffs']]
                out = [row for row in rows if row]
    except (OSError, ValueError, UnicodeDecodeError, RecursionError):
        out = []
    known = {row['request'] for row in out}
    try:
        if drafts_path and stat.S_ISREG(os.lstat(drafts_path).st_mode):
            with open(drafts_path, 'rb') as handle:
                lines = handle.read(4 * MAX_BYTES).decode('utf-8', 'replace').splitlines()
            for line in lines[-HANDOFFS_MAX * 4:]:
                try:
                    item = json.loads(line)
                except ValueError:
                    continue
                if (isinstance(item, dict) and isinstance(item.get('request'), str) and REQUEST.fullmatch(item['request'])
                        and isinstance(item.get('thread'), str) and THREAD.fullmatch(item['thread']) and number(item.get('drafted_at'))
                        and item['request'] not in known):
                    known.add(item['request'])
                    out.append(dict(request=item['request'], session=session, thread=item['thread'], drafted_at=item['drafted_at'],
                                    submitted_at=None, spawn_tool_use_id=None, agent_id=None, turn_id=None, outcome='draft',
                                    ended_at=None, reason=None))
    except (OSError, UnicodeDecodeError):
        pass
    return sorted(out, key=lambda row: row['drafted_at'])[-HANDOFFS_MAX:]


def handoff_envelope(session, request, thread):
    """The envelope a click on "-> subagent" appends to the unsent prompt."""
    return format_agents_context(dict(kind='handoff', session=session, request=request, thread=thread, instruction=HANDOFF_INSTRUCTION))


def decide(handoffs):
    """What the handoffs of one thread say of it: None (none was sent), or (word, record) for the newest handoff that was
    sent. A draft nobody sent says nothing; a retry is a newer request, and the newest decides, so an earlier failure
    never reopens a thread whose later handoff went well. word is 'unlinked' (sent, but tied to no agent), 'handed'
    (an agent has it, or it reported) or 'failed'."""
    sent = [row for row in handoffs if row['outcome'] != 'draft']
    if not sent:
        return None
    last = max(sent, key=lambda row: (row['submitted_at'] or row['drafted_at'], row['drafted_at']))
    word = {'submitted': 'unlinked', 'bound': 'handed', 'succeeded': 'handed', 'failed': 'failed'}[last['outcome']]
    return word, last
