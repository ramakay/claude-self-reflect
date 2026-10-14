"""Codex runs and its week, read from synthetic rollout files (never a real ~/.codex)."""
import importlib.util
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

BIN = Path(__file__).resolve().parents[1] / 'bin'


def load(name):
    spec = importlib.util.spec_from_file_location(name, BIN / f'{name}.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


codex_runs = load('codex_runs')
pane = load('pane')

NOW = datetime(2026, 10, 13, 12, 0, 0, tzinfo=timezone.utc).timestamp()
WEEK_END = NOW + 3 * 86400


def iso(t):
    return datetime.fromtimestamp(t, timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.000Z')


def meta(session='S1', at=NOW - 600, cwd='/work/proj', **more):
    return dict(timestamp=iso(at), type='session_meta', payload=dict(id=session, timestamp=iso(at), cwd=cwd, model_provider='openai', **more))


def context(model='gpt-6-astra', effort='xhigh', cwd='/work/proj', at=NOW - 590):
    return dict(timestamp=iso(at), type='turn_context', payload=dict(turn_id='t1', cwd=cwd, model=model, effort=effort))


def tokens(total, at=NOW - 30, limits=None, **usage):
    info = dict(total_token_usage=dict(input_tokens=total - 10, cached_input_tokens=total // 2, output_tokens=10,
                                       reasoning_output_tokens=3, total_tokens=total, **usage))
    payload = dict(type='token_count', info=info)
    if limits is not None:
        payload['rate_limits'] = limits
    return dict(timestamp=iso(at), type='event_msg', payload=payload)


def week(percent=43.0, where='primary', resets=WEEK_END, limit='codex', minutes=10080):
    window = dict(used_percent=percent, window_minutes=minutes, resets_at=int(resets))
    other = dict(used_percent=5.0, window_minutes=300, resets_at=int(NOW + 3600))
    both = {'primary': other, 'secondary': window} if where == 'secondary' else {'primary': window, 'secondary': other}
    return dict(limit_id=limit, limit_name=None, **both, credits=None, plan_type='pro')


class Folder(unittest.TestCase):
    def setUp(self):
        self.home = tempfile.mkdtemp(prefix='csr-codex-')
        self.addCleanup(shutil.rmtree, self.home, ignore_errors=True)
        self.n = 0

    def write(self, records, name=None, age=5, day='2026/10/13', raw=''):
        folder = os.path.join(self.home, '.codex', 'sessions', day)
        os.makedirs(folder, exist_ok=True)
        self.n += 1
        path = os.path.join(folder, name or f'rollout-2026-10-13T11-50-00-{self.n:04d}.jsonl')
        with open(path, 'w') as handle:
            handle.write(''.join(json.dumps(r) + '\n' for r in records) + raw)
        os.utime(path, (NOW - age, NOW - age))
        return path

    def snapshot(self, index=None):
        return (index or codex_runs.CodexRolloutIndex(self.home)).refresh(NOW)


class Rollouts(Folder):
    def test_live_uses_mtime_under_60_seconds(self):
        self.write([meta('S1'), context(), tokens(1000)], age=59)
        self.write([meta('S2'), context(), tokens(2000)], age=60)
        self.write([meta('S3'), context(), tokens(3000)], age=3600)
        got = self.snapshot()
        self.assertEqual(got['live'], 1)
        self.assertEqual([row['identity'] for row in got['rows']], ['S1'])

    def test_model_effort_from_turn_context(self):
        records = [meta('S1', cwd='/old/cwd'), context('gpt-5', 'low', cwd='/mid'), context('gpt-6-astra', 'xhigh', cwd='/work/proj'), tokens(5)]
        self.write(records)
        (row,) = self.snapshot()['rows']
        self.assertEqual((row['model'], row['effort'], row['cwd']), ('gpt-6-astra', 'xhigh', '/work/proj'))  # the latest context
        self.assertEqual(row['minutes'], 10)  # since the session's start, 600 s before
        # model_provider is not a model: with no context, and none in the meta, they are unknown, not guessed
        self.write([meta('S4')], age=5)
        fresh = {r['identity']: r for r in codex_runs.CodexRolloutIndex(self.home).refresh(NOW)['rows']}
        self.assertEqual((fresh['S4']['model'], fresh['S4']['effort'], fresh['S4']['tokens']), (None, None, None))
        # an older Codex wrote them in session_meta: used when no turn context says otherwise
        self.write([meta('S5', model='gpt-5.1', effort='high')])
        old = {r['identity']: r for r in codex_runs.CodexRolloutIndex(self.home).refresh(NOW)['rows']}
        self.assertEqual((old['S5']['model'], old['S5']['effort']), ('gpt-5.1', 'high'))

    def test_week_window_can_be_primary_or_secondary(self):
        for where in ('primary', 'secondary'):
            shutil.rmtree(os.path.join(self.home, '.codex'), ignore_errors=True)
            self.write([meta(), context(), tokens(10, limits=week(61.5, where))])
            got = self.snapshot()['week']
            self.assertEqual((got['percent'], got['level']), (61.5, None), where)
        for percent, level in ((74.9, None), (75.0, 'amber'), (89.9, 'amber'), (90.0, 'red'), (100.0, 'red')):
            shutil.rmtree(os.path.join(self.home, '.codex'))
            self.write([meta(), tokens(10, limits=week(percent))])
            self.assertEqual(self.snapshot()['week']['level'], level, percent)
        shutil.rmtree(os.path.join(self.home, '.codex'))
        self.write([meta(), tokens(10, limits=week(40, minutes=300))])  # a 5-hour window is not the week
        self.assertIsNone(self.snapshot()['week'])

    def test_newest_codex_limit_wins(self):
        self.write([meta('S1'), tokens(1, at=NOW - 500, limits=week(10.0)), tokens(2, at=NOW - 100, limits=week(30.0))], age=500)
        self.write([meta('S2'), tokens(1, at=NOW - 50, limits=week(55.0))], age=50)
        self.write([meta('S3'), tokens(1, at=NOW - 10, limits=week(99.0, limit='codex_other'))], age=10)  # another limit
        got = self.snapshot()
        self.assertEqual(got['week']['percent'], 55.0)  # never summed (10+30+55), never an older run's higher figure
        # a reading whose window has reset is last week's: unavailable, not zero
        shutil.rmtree(os.path.join(self.home, '.codex'))
        self.write([meta(), tokens(1, at=NOW - 100, limits=week(80.0, resets=NOW - 1))])
        self.assertIsNone(self.snapshot()['week'])
        self.assertEqual(codex_runs.week_line(None, 2), 'Codex week unavailable · 2 live')

    def test_cumulative_tokens_are_not_summed(self):
        self.write([meta(), tokens(1000), tokens(2500, at=NOW - 20), tokens(4000, at=NOW - 10)])
        self.write([meta('S2'), tokens(700)], age=10)
        got = {row['identity']: row['tokens'] for row in self.snapshot()['rows']}
        self.assertEqual(got, {'S1': 4000, 'S2': 700})  # the latest total of each, not 1000+2500+4000, cached not added again
        # one session found in two files is one run
        self.write([meta('S1'), tokens(3000)], age=4)
        again = self.snapshot(codex_runs.CodexRolloutIndex(self.home))
        self.assertEqual(sorted(row['identity'] for row in again['rows']), ['S1', 'S2'])

    def test_partial_line_and_rotation(self):
        path = self.write([meta(), context()], age=5)
        index = codex_runs.CodexRolloutIndex(self.home)
        self.assertEqual(index.refresh(NOW)['rows'][0]['tokens'], None)
        line = json.dumps(tokens(1234)) + '\n'
        with open(path, 'a') as handle:
            handle.write(line[:40])  # a line still being written
        os.utime(path, (NOW - 3, NOW - 3))
        index.checked = None
        self.assertEqual(index.refresh(NOW)['rows'][0]['tokens'], None)
        with open(path, 'a') as handle:
            handle.write(line[40:])
            handle.write('not json at all\n{"type": "event_msg", "payload": {"type": "token_count", "info": 5}}\n')
        os.utime(path, (NOW - 2, NOW - 2))
        index.checked = None
        self.assertEqual(index.refresh(NOW)['rows'][0]['tokens'], 1234)  # the line is taken once whole; the bad ones skipped
        # rotation: the file is replaced by a shorter one
        with open(path, 'w') as handle:
            handle.write(json.dumps(meta('S9')) + '\n' + json.dumps(tokens(77)) + '\n')
        os.utime(path, (NOW - 1, NOW - 1))
        index.checked = None
        rows = index.refresh(NOW)['rows']
        self.assertEqual([(r['identity'], r['tokens']) for r in rows], [('S9', 77)])
        # a file removed is forgotten; a link is not followed
        os.unlink(path)
        index.checked = None
        self.assertEqual(index.refresh(NOW)['rows'], [])
        target = os.path.join(self.home, 'elsewhere.jsonl')
        with open(target, 'w') as handle:
            handle.write(json.dumps(meta('LINKED')) + '\n')
        os.symlink(target, os.path.join(os.path.dirname(path), 'rollout-2026-10-13T11-50-00-link.jsonl'))
        os.utime(target, (NOW - 1, NOW - 1))
        index.checked = None
        self.assertEqual(index.refresh(NOW)['rows'], [])

    def test_a_conversation_is_never_kept(self):
        body = 'SECRET-BODY please do not keep this ' * 50
        message = dict(timestamp=iso(NOW - 40), type='response_item', payload=dict(type='message', role='user', content=[dict(type='input_text', text=body)]))
        huge = dict(timestamp=iso(NOW - 35), type='response_item', payload=dict(type='function_call_output', output='X' * (codex_runs.MAX_LINE + 10)))
        self.write([meta(), context(), message, huge, tokens(9)])
        index = codex_runs.CodexRolloutIndex(self.home)
        got = index.refresh(NOW)
        self.assertEqual(got['rows'][0]['tokens'], 9)  # a line too long to read does not stop the ones after it
        self.assertNotIn('SECRET-BODY', repr([vars(run) for run in index.runs.values()]) + repr(got))

    def test_absent_codex_is_hidden(self):
        self.assertIsNone(codex_runs.codex_snapshot(self.home, NOW))
        self.assertIsNone(pane.flight_section(None, NOW, None, codex_runs.codex_snapshot(self.home, NOW)))
        os.makedirs(os.path.join(self.home, '.codex'))  # present but no run: the week reads unavailable
        got = codex_runs.codex_snapshot(self.home, NOW)
        self.assertEqual((got['rows'], got['live'], got['week']), ([], 0, None))
        section = pane.flight_section(None, NOW, None, got)
        self.assertEqual([item['title'] for item in section['items']], ['Codex week unavailable · 0 live'])
        codex_runs._INDEXES.pop(self.home, None)

    def test_codex_reader_never_spawns_processes(self):
        source = (BIN / 'codex_runs.py').read_text(encoding='utf-8')
        self.assertIsNone(re.search(r'^\s*(?:import|from)\s+(?:subprocess|multiprocessing|pty|shlex)\b', source, re.M))
        self.assertIsNone(re.search(r'\b(?:os\.(?:system|popen|exec\w*|spawn\w*|fork\w*)|Popen|posix_spawn|__import__)\b', source))
        calls = []
        real = subprocess.Popen
        subprocess.Popen = lambda *a, **k: calls.append(a) or real(*a, **k)
        try:
            self.write([meta(), context(), tokens(1, limits=week())], age=3)
            self.snapshot()
            codex_runs.codex_snapshot(self.home, NOW)
        finally:
            subprocess.Popen = real
            codex_runs._INDEXES.pop(self.home, None)
        self.assertEqual(calls, [])


class TheRows(Folder):
    def test_the_section_says_the_week_and_each_live_run(self):
        self.write([meta('S1', at=NOW - 12 * 60), context('gpt-6-astra', 'xhigh', '/work/proj'), tokens(1_250_000, limits=week(91.0))], age=5)
        self.write([meta('S2', cwd=None), tokens(40)], age=7)
        got = self.snapshot()
        section = pane.flight_section(None, NOW, None, got)
        self.assertEqual((section['name'], section['right']), ('flight', '2 live'))
        week_item, first, second = section['items']
        self.assertTrue(re.fullmatch(r'Codex week 91% · resets \w{3} \d\d:\d\d · 2 live', week_item['title']), week_item['title'])
        self.assertEqual(week_item['tone'], 'red')
        titles = {item['title']: item['body'] for item in (first, second)}
        self.assertEqual(titles['Codex · gpt-6-astra · xhigh'], ['12m · proj · 1.2M tokens'.replace('proj', 'work/proj')])
        self.assertEqual(titles['Codex · model unknown · effort unknown'], ['10m · folder unknown · 40 tokens'])
        self.assertTrue(all(item['nosend'] for item in section['items']))  # rollout facts are not Claude's to be told
        self.assertTrue(section['active'])
        # amber below 90, quiet below 75
        self.assertEqual(pane.flight_section(None, NOW, None, dict(rows=[], live=0, week=dict(percent=80.0, resets_at=WEEK_END, level='amber')))['items'][0]['tone'], 'amber')
        quiet = pane.flight_section(None, NOW, None, dict(rows=[], live=0, week=dict(percent=10.0, resets_at=None, level=None)))['items'][0]
        self.assertEqual((quiet['title'], quiet['quiet']), ('Codex week 10% · 0 live', True))

    def test_codex_rows_follow_claudes_agents_in_one_section(self):
        file_agents = dict(agents=[], omitted_agents=0, at=None)
        section = pane.flight_section(file_agents, NOW, None, dict(rows=[], live=0, week=None))
        self.assertEqual([item['key'] for item in section['items']], ['codex:week'])
        rendered = pane.item_rows(dict(key='k', title='Codex week 91% · 1 live', tone='red', lines=1), 'flight', 60, False)
        self.assertIn('\x1b[1m', rendered[0])
