"""The agents of a session: hooks/agents.ts's redaction held to bin/private.py's table, and (below) the pane's side of them."""
import json
import os
import re
import subprocess
from pathlib import Path

from test_r5 import typescript, unavailable  # noqa: F401  the compiler fixture the hook regressions share

ROOT = Path(__file__).resolve().parents[1]
TABLE = ROOT / 'tests' / 'redaction_table.json'

HOST = r'''
const fs = require('node:fs'), vm = require('node:vm')
const ts = require(process.argv[1])
const source = fs.readFileSync(process.argv[2], 'utf8')
const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText
const mod = { exports: {} }
vm.runInThisContext(`(function(require,module,exports){${js}\n})`)((name) => { throw new Error('unexpected import ' + name) }, mod, mod.exports)
const table = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'))
const wrong = table.filter(([input, expected]) => mod.exports.redactAgentValue(input) !== expected)
process.stdout.write(JSON.stringify(wrong.map(([input, expected]) => [input, expected, mod.exports.redactAgentValue(input)])))
'''


def test_agents_ts_redaction_matches_the_helpers_table(typescript):
    """The same table private.redact_value is held to (test_private.py): every pair comes out the same through
    hooks/agents.ts's redactAgentValue, which adds only KEY/TOKEN/SECRET assignment lines and 32-digit hex runs."""
    ran = subprocess.run(['node', '-e', HOST, typescript, str(ROOT / 'hooks' / 'agents.ts'), str(TABLE)],
                         capture_output=True, text=True, timeout=30)
    assert ran.returncode == 0, ran.stdout + ran.stderr
    assert json.loads(ran.stdout) == []


def test_the_hooks_test_holds_a_copy_of_the_table():
    """tests/agents.test.ts cannot import a .json file, so it carries the table: this keeps the copy whole."""
    text = (ROOT / 'tests' / 'agents.test.ts').read_text(encoding='utf-8')
    block = text[text.index('const table: Array<[string, string]> = ['):]
    block = block[:block.index('\n]\n')]
    copy = [json.loads(line.strip().rstrip(',')) for line in block.splitlines()[1:]]
    assert copy == json.loads(TABLE.read_text(encoding='utf-8'))


# ---------------------------------------------------------------- the agents file, and In flight

import importlib.util
import shutil
import sqlite3
import stat
import sys
import tempfile
import types
import unittest

BIN = ROOT / 'bin'


def load(name):
    spec = importlib.util.spec_from_file_location(name, BIN / f'{name}.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


pane = load('pane')
agents_lib = pane.agents_lib

SESSION = 'sess-a'
NOW = 1_800_000_000.0  # seconds; the file's own times are milliseconds
MS = NOW * 1000

PROJECT = r'''
const fs = require('node:fs'), vm = require('node:vm')
const ts = require(process.argv[1])
const mod = { exports: {} }
const source = fs.readFileSync(process.argv[2], 'utf8')
const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText
vm.runInThisContext(`(function(require,module,exports){${js}\n})`)((name) => { throw new Error('unexpected import ' + name) }, mod, mod.exports)
const input = JSON.parse(fs.readFileSync(0, 'utf8'))
process.stdout.write(JSON.stringify(mod.exports.projectAgentsFile(input.state, input.now)))
'''


def rec(agent, **over):
    """An agent as the hooks' state keeps it (types/index.d.ts, AgentRec)."""
    base = dict(id=agent, parent=None, description='Scan the repo', type='Explore', model='claude-opus-5', status='running',
                started=MS - 5 * 60_000, first_seen=MS - 5 * 60_000, last_at=MS - 10_000, tools=3, tokens=1200, last='Reading the files',
                last_tool='Read', current_turn='turn-1', completion=None, listed=True, paths={}, seenTools=[], seenSteps=[])
    return dict(base, **over)


def state_of(*rows, main=None):
    return dict(version=1, session=SESSION, rows={row['id']: row for row in rows}, spawns={}, bound={}, main=main or {},
                mainOmitted=0, seen=[], pending={}, listedAt=None, fileAt=None, fileDue=None)


def completion(agent, turn='turn-1', answer='The report.', reason='answer', at=MS - 60_000):
    return dict(key=json.dumps([SESSION, agent, turn], separators=(',', ':')), turnId=turn, at=at, reason=reason, answer=answer,
                disposition='pending' if reason == 'answer' and answer.strip() else 'skipped')


def projected(typescript, state, now=MS):
    ran = subprocess.run(['node', '-e', PROJECT, typescript, str(ROOT / 'hooks' / 'agents.ts')], input=json.dumps(dict(state=state, now=now)),
                         capture_output=True, text=True, timeout=60)
    assert ran.returncode == 0, ran.stdout + ran.stderr
    return ran.stdout


class Folder(unittest.TestCase):
    def setUp(self):
        self.home = tempfile.mkdtemp(prefix='csr-agents-')
        self.addCleanup(shutil.rmtree, self.home, ignore_errors=True)
        self.path = os.path.join(self.home, '.claude-self-reflect', 'csr-companion', 'agents', f'{SESSION}.json')

    def keep(self, text):
        """The text through bin/private.py, as the hooks write it."""
        env = dict(os.environ, HOME=self.home)
        ran = subprocess.run([sys.executable, '-I', str(BIN / 'private.py'), 'write', self.path, '--redact-json'], input=text,
                             capture_output=True, text=True, env=env, timeout=30)
        self.assertEqual(ran.returncode, 0, ran.stderr)
        return open(self.path, 'rb').read()


@unittest.skipUnless(shutil.which('node'), 'Node is unavailable')
class TheFile(Folder):
    def compiler(self):
        for parent in ROOT.parents:
            candidate = parent / 'node_modules/typescript/lib/typescript.js'
            if candidate.is_file():
                return str(candidate)
        found = subprocess.run(['npm', 'root', '-g'], capture_output=True, text=True).stdout.strip()
        for candidate in [Path(found) / 'typescript/lib/typescript.js', *Path(found).glob('*/node_modules/typescript/lib/typescript.js'),
                          *Path(found).glob('@*/*/node_modules/typescript/lib/typescript.js')]:
            if candidate.is_file():
                return str(candidate)
        if os.environ.get('CI'):
            self.fail('CI installs a pinned typescript before pytest')
        self.skipTest('An installed TypeScript compiler is unavailable')

    def test_agents_file_is_private_redacted_and_under_16k(self):
        parent = rec('p1', description='Plan the work: password=hunter2', status='running', model='é' * 70)
        kids = [rec(f'c{n}', parent='p1', last='界' * 300, status='completed', last_at=MS - n * 1000,
                    completion=completion(f'c{n}', answer='界' * 700),
                    paths={str(k): f'/repo/src/sk-0123456789abcdefghij/file{k}.ts' for k in range(12)}) for n in range(7)]
        more = [rec(f'x{n}', status='running', last='STRIPE_KEY=live_abc123 ' + 'long ' * 80) for n in range(6)]
        text = projected(self.compiler(), state_of(parent, *kids, *more))
        self.assertLessEqual(len(text.encode('utf-8')), 16384)
        kept = self.keep(text)
        self.assertLessEqual(len(kept), 16384)
        self.assertEqual(stat.S_IMODE(os.stat(self.path).st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(os.stat(os.path.dirname(self.path)).st_mode), 0o700)
        raw = json.loads(kept)
        self.assertLessEqual(len(raw['agents']), 12)
        self.assertEqual(raw['version'], 1)
        for secret in (b'hunter2', b'sk-0123', b'live_abc123'):
            self.assertNotIn(secret, kept)
        again = agents_lib.read_agents(self.path, SESSION)
        self.assertEqual(len(again['agents']), len(raw['agents']))
        self.assertGreater(raw['omitted_agents'], 0)  # 14 agents, room for 12
        ids = [row['id'] for row in raw['agents']]
        self.assertLess(ids.index('p1'), min(ids.index(f'c{n}') for n in range(7) if f'c{n}' in ids))  # a parent before its children

    def test_redaction_precedes_truncation(self):
        """A secret that straddles the cut is redacted whole first: cut first, its head would be left behind."""
        secret = 'sk-' + 'ABCDEFGHIJKLMNOP' * 2
        row = rec('a1', last='a' * 230 + ' ' + secret, description='d' * 150 + ' ' + secret)
        row['completion'] = completion('a1', answer='r' * 690 + ' ' + secret)
        text = projected(self.compiler(), state_of(row))
        self.assertNotIn('sk-', text)
        self.assertNotIn('ABCDEFG', text)
        self.assertIn('[redacted]', text)

    def test_overlap_beyond_eight_edits_is_flagged(self):
        from_agent = {str(k): f'/repo/f{k:02d}.ts' for k in range(12)}
        main = {'7': '/repo/f10.ts'}  # the main loop wrote the eleventh, as the state keys it
        row = rec('a1', paths={fp: path for fp, path in from_agent.items()})
        # fingerprints are the hooks' own: ask them for the real ones
        probe = r'''
const fs = require('node:fs'), vm = require('node:vm')
const ts = require(process.argv[1]), mod = { exports: {} }
vm.runInThisContext(`(function(require,module,exports){${ts.transpileModule(fs.readFileSync(process.argv[2], 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText}\n})`)(() => { throw 1 }, mod, mod.exports)
process.stdout.write(JSON.stringify(JSON.parse(process.argv[3]).map(p => mod.exports.fingerprint(p))))
'''
        prints = json.loads(subprocess.run(['node', '-e', probe, self.compiler(), str(ROOT / 'hooks' / 'agents.ts'),
                                            json.dumps([from_agent[str(k)] for k in range(12)])], capture_output=True, text=True).stdout)
        row = rec('a1', paths={fp: from_agent[str(k)] for k, fp in enumerate(prints)})
        got = json.loads(projected(self.compiler(), state_of(row, main={prints[10]: '/repo/f10.ts'})))['agents'][0]
        self.assertEqual(got['overlaps'], ['/repo/f10.ts'])
        self.assertIn('/repo/f10.ts', got['edits'])  # the overlap leads the eight shown
        self.assertEqual((len(got['edits']), got['edits_omitted'], got['overlaps_omitted']), (8, 4, 0))
        item = pane.flight_item(agents_lib.check(dict(version=1, session=SESSION, at=MS, omitted_agents=0, agents=[got]), SESSION)['agents'][0],
                                0, False, NOW)
        self.assertEqual(item['flag'], 'also edited by Claude this session: repo/f10.ts')

    def test_one_parent_and_seven_children_fit_with_their_reports(self):
        parent = rec('p1')
        kids = [rec(f'c{n}', parent='p1', status='completed', completion=completion(f'c{n}', answer=f'Report {n}. ' + 'word ' * 120))
                for n in range(7)]
        raw = json.loads(projected(self.compiler(), state_of(parent, *kids)))
        self.assertEqual([row['id'] for row in raw['agents']][0], 'p1')
        self.assertEqual(len(raw['agents']), 8)
        self.assertEqual(raw['omitted_agents'], 0)
        self.assertTrue(all(row['completion']['answer'].startswith('Report') for row in raw['agents'][1:]))


class FailsOpen(Folder):
    def write(self, value, raw=None):
        os.makedirs(os.path.dirname(self.path), exist_ok=True)
        with open(self.path, 'w' if raw is None else 'wb') as handle:
            handle.write(json.dumps(value) if raw is None else raw)

    def good(self):
        row = {k: v for k, v in rec('a1').items() if k not in ('paths', 'seenTools', 'seenSteps')}
        row.update(edits=[], edits_omitted=0, overlaps=[], overlaps_omitted=0)
        return dict(version=1, session=SESSION, at=MS, agents=[row], omitted_agents=0)

    def test_corrupt_or_wrong_session_agents_file_fails_open(self):
        self.write(self.good())
        self.assertEqual([row['id'] for row in pane.agents_of(SESSION, os.path.dirname(self.path))['agents']], ['a1'])
        empty = agents_lib.empty()
        folder = os.path.dirname(self.path)
        broken = []
        for name, change in {'version': lambda f: f.update(version=2), 'session': lambda f: f.update(session='other'),
                             'extra key': lambda f: f.update(extra=1), 'status': lambda f: f['agents'][0].update(status='done'),
                             'tools': lambda f: f['agents'][0].update(tools='3'), 'bool count': lambda f: f['agents'][0].update(tools=True),
                             'id': lambda f: f['agents'][0].update(id='../x'), 'duplicate': lambda f: f['agents'].append(dict(f['agents'][0])),
                             'too many': lambda f: f.update(agents=[dict(f['agents'][0], id=f'a{n}') for n in range(13)]),
                             'completion': lambda f: f['agents'][0].update(completion=dict(key='k')),
                             'edits': lambda f: f['agents'][0].update(edits=['x'] * 9)}.items():
            doc = self.good()
            change(doc)
            self.write(doc)
            if pane.agents_of(SESSION, folder) != empty:
                broken.append(name)
        self.assertEqual(broken, [])
        for raw in (b'{"version":1,', b'\xff\xfe', b'[]', b'null', b'', b'{' * 5000):
            self.write(None, raw)
            self.assertEqual(pane.agents_of(SESSION, folder), empty, raw)
        self.write(self.good(), None)  # oversized: valid JSON past 16,384 bytes
        doc = self.good()
        doc['agents'][0]['last'] = 'x' * 20000
        self.write(doc)
        self.assertEqual(pane.agents_of(SESSION, folder), empty)
        self.assertEqual(pane.agents_of('../../etc/passwd', folder), empty)  # a session id is never a path
        self.assertEqual(pane.agents_of(None, folder), empty)
        self.assertEqual(pane.agents_of('missing', folder), empty)

    def test_a_link_a_directory_and_an_unreadable_file_read_as_no_agents(self):
        folder = os.path.dirname(self.path)
        os.makedirs(folder)
        target = os.path.join(self.home, 'real.json')
        with open(target, 'w') as handle:
            json.dump(self.good(), handle)
        os.symlink(target, self.path)
        self.assertEqual(pane.agents_of(SESSION, folder), agents_lib.empty())
        os.unlink(self.path)
        os.mkdir(self.path)
        self.assertEqual(pane.agents_of(SESSION, folder), agents_lib.empty())
        os.rmdir(self.path)
        self.write(self.good())
        os.chmod(self.path, 0)
        try:
            if os.geteuid() != 0:
                self.assertEqual(pane.agents_of(SESSION, folder), agents_lib.empty())
        finally:
            os.chmod(self.path, 0o600)


def projection(*rows):
    doc = dict(version=1, session=SESSION, at=MS, agents=[], omitted_agents=0)
    for row in rows:
        shaped = {k: v for k, v in row.items() if k not in ('paths', 'seenTools', 'seenSteps')}
        shaped.update({k: shaped.get(k, v) for k, v in dict(edits=[], edits_omitted=0, overlaps=[], overlaps_omitted=0).items()})
        doc['agents'].append(shaped)
    return agents_lib.check(doc, SESSION)


def stand_in():
    """A stand-in CSR with no rules on record, a new connection each time (page_main closes the one it is given)."""
    con = sqlite3.connect(':memory:')
    con.execute('create table intent_events (ts text, quote text, session_id text, transcript_path text, byte_start int, project text, kind text)')
    return con


class InFlight(unittest.TestCase):
    def test_in_flight_tree_and_recent_finished_rows(self):
        family = projection(rec('p1'), *[rec(f'c{n}', parent='p1', last_at=MS - (n + 1) * 1000, last=f'Child {n} is reading') for n in range(5)])
        section = pane.flight_section(family, NOW)
        self.assertEqual((section['name'], section['title'], section['right']), ('flight', 'In flight', '6 running'))
        titles = [item['title'] for item in section['items']]
        self.assertEqual(titles[0], 'Explore · claude-opus-5')
        self.assertTrue(all(title.startswith('└ ') for title in titles[1:4]))
        self.assertEqual(titles[4], '› and 2 more')  # four shown, the rest a click away
        self.assertEqual(section['items'][0]['body'], ['5m · 3 tools · Reading the files'])
        self.assertEqual(section['items'][1]['body'], ['  5m · 3 tools · Child 0 is reading'])
        every = pane.flight_section(family, NOW, dict(folds={'flight-more'}))
        self.assertEqual(len(every['items']), 7)  # the six agents and the fold that closes it again
        self.assertEqual(every['items'][-1]['title'], '⌄ fewer')
        # nothing running and no report for 30 minutes: one folded row, the rows a click away
        old = projection(rec('p1', status='completed', last_at=MS - 3_600_000, completion=completion('p1', at=MS - 3_000_000)),
                         rec('c1', parent='p1', status='completed', last_at=MS - 3_500_000))
        folded = pane.flight_section(old, NOW)
        self.assertEqual([item['key'] for item in folded['items']], ['fold:flight'])
        self.assertTrue(folded['items'][0]['title'].startswith('› 2 agents finished'))
        self.assertEqual(len(pane.flight_section(old, NOW, dict(folds={'flight'}))['items']), 3)
        # a report that came 5 minutes ago keeps it open
        fresh = projection(rec('p1', status='completed', last_at=MS - 300_000, completion=completion('p1', at=MS - 300_000)))
        self.assertEqual([item['key'] for item in pane.flight_section(fresh, NOW)['items']], ['flight:p1'])
        self.assertIsNone(pane.flight_section(agents_lib.empty(), NOW))

    def test_failed_killed_stalled_and_empty_agents_are_told_apart(self):
        got = projection(rec('a1', status='failed', completion=completion('a1', reason='error', answer='')),
                         rec('a2', status='killed', completion=completion('a2', reason='aborted', answer='')),
                         rec('a3', status='completed', completion=completion('a3', answer='   ')),
                         rec('a4', status='running', last_at=MS - 15 * 60_000),
                         rec('a5', status='waiting', last_at=MS - 3 * 3_600_000),
                         rec('a6', status='running', last_at=MS - 60_000))
        flags = {item['key']: item['flag'] for item in pane.flight_section(got, NOW, dict(folds={'flight-more'}))['items'] if item['key'].startswith('flight:')}
        self.assertEqual(flags, {'flight:a1': 'failed', 'flight:a2': 'killed', 'flight:a3': 'no report', 'flight:a4': 'stalled 15m',
                                 'flight:a5': '', 'flight:a6': ''})
        self.assertIn('failed', pane.flight_item(got['agents'][0], 0, False, NOW)['title'])
        # no last words: the name of the tool it is using
        silent = projection(rec('a7', last='', last_tool='Bash'))
        self.assertEqual(pane.flight_item(silent['agents'][0], 0, False, NOW)['body'], ['5m · 3 tools · using Bash'])

    def test_a_row_with_an_unknown_parent_is_a_root_marked_so_and_a_cycle_is_cut(self):
        got = projection(rec('c1', parent='gone'), rec('a', parent='b'), rec('b', parent='a'))
        tree = agents_lib.tree(got['agents'])
        self.assertEqual(sorted(row['id'] for row, _, _ in tree), ['a', 'b', 'c1'])  # each once
        self.assertEqual({row['id']: unknown for row, _, unknown in tree}['c1'], True)
        self.assertEqual({row['id']: depth for row, depth, _ in tree}['c1'], 0)  # not attached to the main loop

    def test_report_is_agent_words_and_never_sendable(self):
        long = 'Found it. ' + 'detail ' * 200
        got = projection(rec('a1', status='completed', completion=completion('a1', answer=long)))
        item = pane.flight_section(got, NOW)['items'][0]
        self.assertEqual(item['detail'][0], 'What it found — the agent’s words')
        self.assertLessEqual(len(item['detail'][1]), 700)
        self.assertTrue(item['nosend'])
        self.assertEqual(item['send'], '')
        rows = pane.opened(['row'], 'flight', item['detail'], 60, {}, item['key'], indent=6, whole=True, hint=item['hint'])
        self.assertIn('click again to fold', rows[-1])
        self.assertNotIn("Claude's prompt", ''.join(rows))
        sent = []
        saved = pane.herdr, pane.agent_pane, pane.focus_back
        pane.herdr = lambda *argv: sent.append(argv) or ''
        pane.agent_pane, pane.focus_back = lambda given: 'w1:p1', lambda: None
        try:
            box = pane.Pane('/repo', 10, None)
            box.acts = {3: item}
            box.click(3)
            self.assertEqual(box.open, item['key'])
            box.click(3)  # a second click on any other row would send it
            box.click(3)
        finally:
            pane.herdr, pane.agent_pane, pane.focus_back = saved
        self.assertEqual(sent, [])
        self.assertIsNone(box.sent)

    def test_page_main_puts_in_flight_after_the_story_and_before_what_if(self):
        saved, pane.connect = pane.connect, stand_in
        try:
            s = dict(id=SESSION, prompts=[], replies=[], compacts=[], remembered=[], gave=dict(recalls=[], carried=[], dreams=[]),
                     project='csr-engine', lost=[])
            story = dict(text='The session built it.', now='Built.', rev=1, by='', at=NOW - 10)
            data = dict(repo='/tmp/csr-engine', dreams=[], chapter=dict(at=1000.0, of=5), dreams_on=True, look=None, story=story,
                        infinite=None, agents=projection(rec('p1')))
            names = [x['name'] for x in pane.page_main(data, s, NOW)['sections']]
            self.assertEqual(names, ['story', 'flight', 'dreams', 'remembered', 'coach', 'rules'])
            data['agents'] = agents_lib.empty()
            names = [x['name'] for x in pane.page_main(data, s, NOW)['sections']]
            self.assertEqual(names, ['story', 'dreams', 'remembered', 'coach', 'rules'])
        finally:
            pane.connect = saved

    def test_in_flight_is_fresh_while_the_other_sections_are_held(self):
        held = [dict(name='story', title='Story', items=[dict(key='story', title='t')], right='', empty=''),
                dict(name='flight', title='In flight', items=[dict(key='flight:old', title='old')], right='', empty=''),
                dict(name='dreams', title='What if…', items=[dict(key='d', title='d')], right='', empty='')]
        data = dict(repo='/tmp/csr-engine', dreams=[], chapter=None, dreams_on=False, look=None, story=None, infinite=None,
                    agents=projection(rec('p1')))
        saved, pane.connect = pane.connect, stand_in
        try:
            s = dict(id=SESSION, prompts=[], replies=[], compacts=[], remembered=[], gave=dict(recalls=[], carried=[], dreams=[]),
                     project='csr-engine', lost=[], turn_end=1)
            lines, _ = pane.draw_main(data, s, 100, 40, NOW, {}, hold=held)
        finally:
            pane.connect = saved
        names = [x['name'] for x in lines.sections]
        self.assertEqual(names, ['story', 'flight', 'dreams'])
        self.assertEqual([i['key'] for i in lines.sections[1]['items']], ['flight:p1'])  # the held one is replaced by the live one
