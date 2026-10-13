"""Codex runs beside the session, read from Codex's own rollout files and nothing else.

  <HOME>/.codex/sessions/**/rollout-*.jsonl

Three kinds of record are read, for their metadata only, and no conversation text is kept:
  session_meta                      id, start time, cwd (and a model or effort, where an older Codex wrote one there)
  turn_context                      the latest model, effort and cwd
  event_msg / token_count           the run's cumulative total_tokens, and the plan's rate limits

No Codex program is started and no process is inspected; there is no database, no network and no model call. This module
imports no way to start a process, and a test holds it to that. A run is live while its rollout was written less than
LIVE_SECONDS ago. The week line is the newest valid reading of the "codex" limit's 10080-minute window, which Codex
writes as `primary` or `secondary` (it is not assumed to be either); percentages of different runs are never added.

Files are read incrementally: each remembers where its last read stopped, only whole lines are taken (a line still being
written waits for its newline), a file that shrank or changed is read again from its start, and a line that is not JSON
is skipped.
"""
import json
import os
import re
from datetime import datetime, timedelta, timezone

LIVE_SECONDS = 60
WEEK_MINUTES = 10080
AMBER, RED = 75, 90  # percent of the week at which the line turns amber, then red
LIMIT_ID = 'codex'
KEEP_DAYS = 8  # a rollout older than this tells nothing of the week and of no live run
EVERY = 2.0  # seconds between two scans of the folder; a frame in between reads what the last found
MAX_LINE = 1 << 20  # a line longer than this (a tool's whole output) is skipped unread
HEAD = 400  # characters of a line looked at to tell whether it is a record worth parsing
KINDS = re.compile(r'"type"\s*:\s*"(session_meta|turn_context|event_msg)"')
COUNT = re.compile(r'"type"\s*:\s*"token_count"')


def epoch(value):
    """A time given as epoch seconds (or milliseconds), or an ISO date and time, as seconds; None when it is neither."""
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return value / 1000 if value > 1e11 else float(value)
    if not isinstance(value, str) or not value.strip():
        return None
    text = value.strip().replace(' ', 'T', 1)
    if text.endswith('Z'):
        text = text[:-1] + '+00:00'
    try:
        when = datetime.fromisoformat(text)
    except ValueError:
        return None
    return (when if when.tzinfo else when.replace(tzinfo=timezone.utc)).timestamp()


def text_of(value, most=200):
    """A short string from a record, no control in it; None when it is not a non-empty string."""
    if not isinstance(value, str) or not value.strip():
        return None
    return re.sub(r'[\x00-\x1f\x7f-\x9f]', '', value).strip()[:most] or None


def count_of(value):
    return int(value) if isinstance(value, (int, float)) and not isinstance(value, bool) and 0 <= value < 1e15 else None


def percent_of(value):
    return float(value) if isinstance(value, (int, float)) and not isinstance(value, bool) and 0 <= value <= 1000 else None


class Run:
    """What is known of one rollout file. No text of the conversation is held."""

    def __init__(self):
        self.offset, self.size, self.mtime, self.inode = 0, 0, 0.0, None
        self.identity, self.started = None, None
        self.model = self.effort = self.cwd = None  # the latest turn_context's, else session_meta's
        self.meta_model = self.meta_effort = self.meta_cwd = None
        self.tokens = None
        self.week = None  # (record time, percent, resets_at or None)

    def take(self, record):
        kind, payload = record.get('type'), record.get('payload')
        if not isinstance(payload, dict):
            return
        at = epoch(record.get('timestamp'))
        if kind == 'session_meta':
            self.identity = text_of(payload.get('id') or payload.get('session_id'), 100) or self.identity
            self.started = epoch(payload.get('timestamp')) or at or self.started
            self.meta_cwd = text_of(payload.get('cwd'), 400) or self.meta_cwd
            self.meta_model = text_of(payload.get('model'), 80) or self.meta_model
            self.meta_effort = text_of(payload.get('effort') or payload.get('reasoning_effort'), 20) or self.meta_effort
        elif kind == 'turn_context':
            self.model = text_of(payload.get('model'), 80) or self.model
            self.effort = text_of(payload.get('effort') or payload.get('reasoning_effort'), 20) or self.effort
            self.cwd = text_of(payload.get('cwd'), 400) or self.cwd
        elif kind == 'event_msg' and payload.get('type') == 'token_count':
            info = payload.get('info')
            total = info.get('total_token_usage') if isinstance(info, dict) else None
            tokens = count_of(total.get('total_tokens')) if isinstance(total, dict) else None
            if tokens is not None:
                self.tokens = tokens  # cumulative: the latest is the run's, never a sum
            reading = week_reading(payload.get('rate_limits'), at)
            if reading and (self.week is None or reading[0] >= self.week[0]):
                self.week = reading

    def view(self):
        return dict(identity=self.identity, started=self.started or self.mtime, model=self.model or self.meta_model,
                    effort=self.effort or self.meta_effort, cwd=self.cwd or self.meta_cwd, tokens=self.tokens)


def week_reading(limits, at):
    """(time, percent, resets_at) from a token_count's rate_limits when it is the codex limit with a 10080-minute window,
    in `primary` or `secondary`; else None."""
    if not isinstance(limits, dict) or limits.get('limit_id') != LIMIT_ID:
        return None
    for name in ('primary', 'secondary'):
        window = limits.get(name)
        if not isinstance(window, dict) or window.get('window_minutes') != WEEK_MINUTES:
            continue
        percent = percent_of(window.get('used_percent'))
        if percent is None:
            continue
        resets = epoch(window.get('resets_at'))
        if resets is None and at is not None and isinstance(window.get('resets_in_seconds'), (int, float)):
            resets = at + window['resets_in_seconds']
        return (at if at is not None else 0.0, percent, resets)
    return None


class CodexRolloutIndex:
    """The rollout files under <home>/.codex/sessions, read a little at a time and kept as metadata."""

    def __init__(self, home=None):
        self.home = home if home is not None else os.environ.get('HOME', '')
        self.root = os.path.join(self.home, '.codex')
        self.sessions = os.path.join(self.root, 'sessions')
        self.runs = {}  # path -> Run
        self.checked = None

    def present(self):
        return os.path.isdir(self.root)

    def paths(self, now):
        """The rollout files worth reading: written within KEEP_DAYS, found without following links. Date folders
        (YYYY/MM/DD) older than that are not entered."""
        cutoff = now - KEEP_DAYS * 86400
        floor = (datetime.fromtimestamp(cutoff) - timedelta(days=1)).date()
        found = []
        for folder, dirs, files in os.walk(self.sessions, followlinks=False):
            parts = os.path.relpath(folder, self.sessions).split(os.sep)
            dirs[:] = sorted(d for d in dirs if not self.old(parts, d, floor))
            for name in files:
                if name.startswith('rollout-') and name.endswith('.jsonl'):
                    path = os.path.join(folder, name)
                    try:
                        info = os.lstat(path)
                    except OSError:
                        continue
                    if info.st_mode & 0o170000 == 0o100000 and info.st_mtime >= cutoff:
                        found.append((path, info))
        return found

    @staticmethod
    def old(parts, name, floor):
        """A date folder (year, then month, then day, by depth) wholly before `floor`; any other folder is not."""
        chain = [] if parts == ['.'] else parts
        if len(chain) > 2 or not name.isdigit() or not all(part.isdigit() for part in chain):
            return False
        key = tuple(int(part) for part in chain) + (int(name),)
        return key < (floor.year, floor.month, floor.day)[:len(key)]

    def read(self, path, info):
        """New whole lines of one rollout into its Run."""
        run = self.runs.get(path)
        if run is None or run.inode != info.st_ino or info.st_size < run.offset:
            run = self.runs[path] = Run()  # a file new, replaced or truncated is read from its start
        run.inode, run.mtime = info.st_ino, info.st_mtime
        if info.st_size == run.offset:
            run.size = info.st_size
            return run
        try:
            with open(path, 'rb') as handle:
                handle.seek(run.offset)
                while True:
                    start = handle.tell()
                    line = handle.readline(MAX_LINE + 1)
                    if not line:
                        break
                    if len(line) > MAX_LINE and not line.endswith(b'\n'):
                        while line and not line.endswith(b'\n'):  # a line too long to read: past its end, unparsed
                            line = handle.readline(MAX_LINE)
                        if not line:
                            handle.seek(start)
                            break
                        run.offset = handle.tell()
                        continue
                    if not line.endswith(b'\n'):
                        handle.seek(start)  # a line still being written: its newline is not here yet
                        break
                    run.offset = handle.tell()
                    head = line[:HEAD].decode('utf-8', 'replace')
                    found = KINDS.search(head)
                    if not found or (found.group(1) == 'event_msg' and not COUNT.search(head)):
                        continue
                    try:
                        record = json.loads(line)
                    except ValueError:
                        continue
                    if isinstance(record, dict):
                        run.take(record)
        except OSError:
            pass
        run.size = info.st_size
        return run

    def refresh(self, now):
        """Scan, at most every EVERY seconds; the runs seen, and the week."""
        if self.checked is not None and 0 <= now - self.checked < EVERY:
            return self.snapshot(now)
        self.checked = now
        seen = {}
        for path, info in self.paths(now):
            seen[path] = self.read(path, info)
        self.runs = seen  # a file gone is forgotten
        return self.snapshot(now)

    def snapshot(self, now):
        """dict(rows=[the live runs], live=K, week=dict(percent, resets_at, level) or None)."""
        newest = {}
        for run in self.runs.values():
            key = run.identity or id(run)
            if key not in newest or run.mtime > newest[key].mtime:
                newest[key] = run  # one session found in two files is one run
        live = [run for run in newest.values() if 0 <= now - run.mtime < LIVE_SECONDS]
        rows = []
        for run in sorted(live, key=lambda r: -(r.started or r.mtime)):
            row = run.view()
            row['minutes'] = max(0, int((now - row['started']) // 60)) if row['started'] else None
            rows.append(row)
        readings = [run.week for run in self.runs.values() if run.week]
        week = max(readings, key=lambda r: r[0], default=None)
        reading = None
        if week and (week[2] is None or week[2] > now):  # past its reset the figure is the last week's: unavailable
            level = 'red' if week[1] >= RED else 'amber' if week[1] >= AMBER else None
            reading = dict(percent=week[1], resets_at=week[2], level=level)
        return dict(rows=rows, live=len(rows), week=reading)


_INDEXES = {}


def codex_snapshot(home, now):
    """The Codex section's facts, or None when there is no <home>/.codex (the section is then not drawn). One index per
    home is kept, so the next frame reads only what was written since."""
    index = _INDEXES.get(home)
    if index is None:
        index = _INDEXES[home] = CodexRolloutIndex(home)
    if not index.present():
        _INDEXES.pop(home, None)
        return None
    return index.refresh(now)


def week_line(week, live, now=None):
    """'Codex week 43% · resets Sun 09:00 · 2 live'; 'Codex week unavailable' while there is no current reading."""
    if not week:
        return f'Codex week unavailable · {live} live'
    resets = ''
    if week['resets_at']:
        resets = ' · resets ' + datetime.fromtimestamp(week['resets_at']).strftime('%a %H:%M')
    percent = week['percent']
    return f"Codex week {percent:.0f}%{resets} · {live} live"
