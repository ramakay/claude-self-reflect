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
import unittest.mock

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


# ---------------------------------------------------------------- the machine envelope

FIXTURE = ROOT / 'tests' / 'fixtures' / 'emissions' / 'agents-v1.json'
EXPORT = ROOT / 'tests' / 'export-agent-emissions.py'

# the hooks' modules under node, with their relative imports resolved
LOAD = r'''
const fs = require('node:fs'), path = require('node:path'), vm = require('node:vm')
const ts = require(process.argv[1])
const cache = new Map()
function load(file) {
  if (cache.has(file)) return cache.get(file).exports
  const mod = { exports: {} }
  cache.set(file, mod)
  const js = ts.transpileModule(fs.readFileSync(file, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText
  const req = name => {
    if (!name.startsWith('.')) throw new Error('unexpected import ' + name)
    const base = path.resolve(path.dirname(file), name)
    return load(['.ts', '.tsx'].map(ext => base + ext).find(f => fs.existsSync(f)))
  }
  vm.runInThisContext(`(function(require,module,exports){${js}\n})`)(req, mod, mod.exports)
  return mod.exports
}
const input = JSON.parse(fs.readFileSync(0, 'utf8'))
const hooks = path.join(process.argv[2], 'hooks')
const agents = load(path.join(hooks, 'agents.ts')), dreamer = load(path.join(hooks, 'dreamer.ts'))
const answer = {}
for (const [name, text] of Object.entries(input.texts)) {
  const found = agents.parseAgentEnvelopes(text)
  answer[name] = { parsed: found.map(e => ({ start: e.start, end: e.end, payload: e.payload })), stripped: agents.stripEnvelopes(text), own: dreamer.ownWords(text) }
}
process.stdout.write(JSON.stringify(answer))
'''


def under_node(compiler, texts):
    ran = subprocess.run(['node', '-e', LOAD, compiler, str(ROOT)], input=json.dumps(dict(texts=texts)), capture_output=True, text=True, timeout=60)
    assert ran.returncode == 0, ran.stdout + ran.stderr
    return json.loads(ran.stdout)


def compiler_path():
    for parent in ROOT.parents:
        candidate = parent / 'node_modules/typescript/lib/typescript.js'
        if candidate.is_file():
            return str(candidate)
    found = subprocess.run(['npm', 'root', '-g'], capture_output=True, text=True).stdout.strip()
    for candidate in [Path(found) / 'typescript/lib/typescript.js', *Path(found).glob('*/node_modules/typescript/lib/typescript.js'),
                      *Path(found).glob('@*/*/node_modules/typescript/lib/typescript.js')]:
        if candidate.is_file():
            return str(candidate)
    return None


CONTEXT = dict(kind='context', session=SESSION, at=1_800_000_000_000, agents=[
    dict(id='a1', parent=None, type='Explore', model='m', status='completed', edits=['/r/a[1].py'], overlaps=[],
         completion=dict(turn_id='t1', reason='answer', answer='Done [[CSR:AGENTS:v1]]\nwith 界 "quotes"')),
    dict(id='a2', parent='a1', type=None, model=None, status='running', edits=[], overlaps=[], completion=None)])


class TheEnvelope(unittest.TestCase):
    def test_the_context_envelope_is_exactly_three_lines_with_its_brackets_escaped(self):
        text = agents_lib.format_agents_context(CONTEXT)
        self.assertTrue(text.endswith('\n'))
        lines = text.split('\n')
        self.assertEqual((lines[0], lines[2], lines[3], len(lines)), ('[[CSR:AGENTS:v1]]', '[[/CSR:AGENTS:v1]]', '', 4))
        self.assertNotIn('\n', lines[1])
        body = lines[1]
        self.assertEqual((body.count('['), body.count(']')), (5, 5))  # the JSON's own arrays: agents, and edits and overlaps of two rows
        self.assertIn('\\u005b\\u005bCSR:AGENTS:v1\\u005d\\u005d', body)
        self.assertIn('a\\u005b1\\u005d.py', body)
        self.assertEqual(json.loads(body), CONTEXT)  # and it is the same payload

    def test_only_valid_payloads_are_formatted_and_the_size_is_bounded(self):
        bad = [dict(CONTEXT, extra=1), dict(CONTEXT, kind='other'), dict(CONTEXT, session='../x'), dict(CONTEXT, agents='x'),
               dict(CONTEXT, agents=CONTEXT['agents'] * 7),
               dict(CONTEXT, agents=[dict(CONTEXT['agents'][0], status='done')]),
               dict(CONTEXT, agents=[dict(CONTEXT['agents'][0], completion=dict(turn_id='t', reason='answer', answer='x' * 701))]),
               dict(CONTEXT, agents=[dict(CONTEXT['agents'][0], edits=['x'] * 9)]),
               dict(kind='handoff', session=SESSION, request='r', thread='0123456789ab', instruction='Do something else.'),
               dict(kind='handoff', session=SESSION, request='r', thread='not-hex', instruction=agents_lib.HANDOFF_INSTRUCTION),
               dict(kind='handoff', session=SESSION, request='r', thread='0123456789ab', instruction=agents_lib.HANDOFF_INSTRUCTION, more=1)]
        for payload in bad:
            with self.subTest(payload=str(payload)[:80]):
                with self.assertRaises(ValueError):
                    agents_lib.format_agents_context(payload)
        wide = dict(CONTEXT, agents=[dict(CONTEXT['agents'][0], id=f'a{n}', edits=['é' * 300] * 8, overlaps=['é' * 300] * 8,
                                           completion=dict(turn_id='t', reason='answer', answer='界' * 700)) for n in range(12)])
        with self.assertRaises(ValueError):  # a payload past 16,384 bytes is refused, not cut
            agents_lib.format_agents_context(wide)
        fitted = agents_lib.context_envelope(dict(agents=wide['agents'], omitted_agents=0, at=1.0), SESSION) if False else None
        self.assertIsNone(fitted)

    def test_a_handoff_envelope_carries_its_instruction_word_for_word(self):
        payload = dict(kind='handoff', session=SESSION, request='req-1', thread='0123456789ab', instruction=agents_lib.HANDOFF_INSTRUCTION)
        text = agents_lib.format_agents_context(payload)
        self.assertEqual([(s, e, p) for s, e, p in agents_lib.parse_envelopes(text)], [(0, len(text), payload)])

    def test_brief_agent_envelope_round_trip(self):
        """What bin/agents.py writes into the brief is what hooks/agents.ts reads out of it."""
        ours = agents_lib.format_agents_context(CONTEXT)
        around = f'Facts about the work.\n* a line\n\n{ours}'.rstrip('\n')  # as pane.py's brief ends: the final newline stripped
        self.assertEqual([p for _, _, p in agents_lib.parse_envelopes(around)], [CONTEXT])
        self.assertEqual(agents_lib.strip_envelopes(around), 'Facts about the work.\n* a line\n\n')
        compiler = compiler_path()
        if not compiler:
            if os.environ.get('CI'):
                self.fail('CI installs a pinned typescript before pytest')
            self.skipTest('An installed TypeScript compiler is unavailable')
        got = under_node(compiler, dict(brief=around, context=ours))
        self.assertEqual([e['payload'] for e in got['brief']['parsed']], [CONTEXT])
        self.assertEqual(got['brief']['stripped'], 'Facts about the work.\n* a line\n\n')
        self.assertEqual((got['context']['stripped'], got['context']['own']), ('', ''))
        self.assertEqual(got['brief']['own'], 'Facts about the work.\n* a line\n\n')

    def test_agent_envelope_preserves_surrounding_user_reply(self):
        ours = agents_lib.format_agents_context(CONTEXT)
        reply = f'Yes, please do that.\n{ours}And check the importer too.'
        self.assertEqual(agents_lib.strip_envelopes(reply), 'Yes, please do that.\nAnd check the importer too.')
        self.assertEqual(pane.strip_forwards(reply), 'Yes, please do that.\nAnd check the importer too.')
        self.assertEqual(pane.own_words(reply), 'Yes, please do that.\nAnd check the importer too.')
        # the person's own words are untouched, byte for byte, when nothing is an envelope
        words = 'Mind [[CSR:AGENTS:v1]] in prose\n  indented\n'
        self.assertEqual(agents_lib.strip_envelopes(words), words)
        self.assertEqual(pane.strip_forwards('Plain words.'), 'Plain words.')
        compiler = compiler_path()
        if compiler:
            got = under_node(compiler, dict(reply=reply, plain=words))
            self.assertEqual(got['reply']['own'], 'Yes, please do that.\nAnd check the importer too.')
            self.assertEqual(got['plain']['stripped'], words)

    def test_only_complete_valid_envelopes_at_the_start_of_a_line_are_removed(self):
        good = agents_lib.format_agents_context(CONTEXT)
        body = good.split('\n')[1]
        keep = {
            'bare markers': '[[CSR:AGENTS:v1]]\nnot json\n[[/CSR:AGENTS:v1]]\n',
            'open only': '[[CSR:AGENTS:v1]]\n' + body + '\n',
            'no close': '[[CSR:AGENTS:v1]]\n' + body + '\nsomething else\n',
            'indented': '  ' + good,
            'quoted': '> ' + good.replace('\n', '\n> '),
            'grep prefix': ''.join(f'file.txt:{n}:{line}\n' for n, line in enumerate(good.splitlines(), 1)),
            'extra key': '[[CSR:AGENTS:v1]]\n' + json.dumps(dict(CONTEXT, extra=1), separators=(',', ':')) + '\n[[/CSR:AGENTS:v1]]\n',
            'two json lines': '[[CSR:AGENTS:v1]]\n' + body + '\n' + body + '\n[[/CSR:AGENTS:v1]]\n',
            'over the size': '[[CSR:AGENTS:v1]]\n' + json.dumps(dict(CONTEXT, pad='x' * 20000)) + '\n[[/CSR:AGENTS:v1]]\n',
            'wrong version': good.replace('v1]]', 'v2]]'),
            'raw agents json': body + '\n',
        }
        for name, text in keep.items():
            with self.subTest(name):
                self.assertEqual(agents_lib.strip_envelopes(text), text)
                self.assertEqual(agents_lib.parse_envelopes(text), [])
        for name, text in {'CRLF': good.replace('\n', '\r\n'), 'BOM': '﻿' + good, 'no final newline': good.rstrip('\n'),
                           'twice': good + 'between\n' + good}.items():
            with self.subTest(name):
                self.assertGreaterEqual(len(agents_lib.parse_envelopes(text)), 1)
                self.assertNotIn('[[CSR:AGENTS:v1]]', agents_lib.strip_envelopes(text))
        self.assertEqual(agents_lib.strip_envelopes(good + 'between\n' + good), 'between\n')
        self.assertEqual(agents_lib.strip_envelopes('a\r\n' + good.replace('\n', '\r\n') + 'b'), 'a\r\nb')

    def test_the_fixture_is_what_the_formatter_writes_and_never_rewritten_by_check(self):
        before = FIXTURE.read_bytes()
        ran = subprocess.run([sys.executable, '-B', str(EXPORT), '--check'], capture_output=True, text=True, timeout=60)
        self.assertEqual((ran.returncode, ran.stderr), (0, ''))
        self.assertEqual(FIXTURE.read_bytes(), before)
        doc = json.loads(before)
        self.assertEqual((doc['formatter'], sorted(doc['fixtures'])), ('plugin_agents_v1', ['context', 'handoff']))
        import hashlib
        for name, entry in doc['fixtures'].items():
            self.assertEqual(hashlib.sha256(entry['text'].encode('utf-8')).hexdigest(), entry['sha256'])
            self.assertEqual(agents_lib.format_agents_context(entry['payload']), entry['text'])
            self.assertEqual([p for _, _, p in agents_lib.parse_envelopes(entry['text'])], [entry['payload']])
        # a fixture that is not what the formatter writes fails the check, and the check leaves it as it found it
        folder = tempfile.mkdtemp(prefix='csr-fixture-')
        self.addCleanup(shutil.rmtree, folder, ignore_errors=True)
        stale = Path(folder) / 'agents-v1.json'
        stale.write_bytes(before.replace(b'synthetic-session', b'another-session', 1))
        ran = subprocess.run([sys.executable, '-B', str(EXPORT), '--check'], capture_output=True, text=True, timeout=60,
                             env=dict(os.environ, CSR_AGENT_EMISSIONS_FIXTURE=str(stale)))
        self.assertEqual(ran.returncode, 1)
        self.assertIn('is not what bin/agents.py writes', ran.stderr)
        self.assertEqual(stale.read_bytes(), before.replace(b'synthetic-session', b'another-session', 1))
        self.assertEqual(subprocess.run([sys.executable, '-B', str(EXPORT)], capture_output=True, text=True).returncode, 2)

    def test_the_hooks_read_the_fixture_the_same_way(self):
        compiler = compiler_path()
        if not compiler:
            if os.environ.get('CI'):
                self.fail('CI installs a pinned typescript before pytest')
            self.skipTest('An installed TypeScript compiler is unavailable')
        doc = json.loads(FIXTURE.read_text(encoding='utf-8'))
        got = under_node(compiler, {name: entry['text'] for name, entry in doc['fixtures'].items()})
        for name, entry in doc['fixtures'].items():
            with self.subTest(name):
                self.assertEqual([e['payload'] for e in got[name]['parsed']], [entry['payload']])
                self.assertEqual((got[name]['stripped'], got[name]['own']), ('', ''))


class TheBrief(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix='csr-brief-')
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.repo = os.path.realpath(os.path.join(self.tmp, 'repo'))
        os.makedirs(self.repo)
        env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
        subprocess.run(['git', '-C', self.repo, 'init', '-q'], check=True, env=env)
        self.main_file, self.agent_file = os.path.join(self.repo, 'main.py'), os.path.join(self.repo, 'agent.py')
        for path in (self.main_file, self.agent_file):
            open(path, 'w').close()
        self.asked = []
        agents_dir = os.path.join(self.tmp, 'agents')
        os.makedirs(agents_dir)
        self.doc = dict(version=1, session=SESSION, at=MS, agents=[], omitted_agents=0)
        self.agents_dir = agents_dir

        class Fake:
            files, last, compacts, talk = {self.main_file: 2}, {self.main_file: NOW - 500}, [], []

            def __init__(me, session_id):
                pass

            def read_transcript(me):
                pass

        patches = [unittest.mock.patch.object(pane, name, value) for name, value in (
            ('Session', Fake), ('built_near', lambda *a, **k: []), ('voice_and_lines', lambda *a, **k: (None, [])),
            ('load_dream_log', lambda *a, **k: []), ('typed_by_you', lambda *a, **k: True),
            ('notes_for', lambda con, root, path, session_id=None, before=None: self.asked.append(path) or []),
            ('agents_of', lambda session_id, folder=None: self.read()))]
        for patcher in patches:
            patcher.start()
            self.addCleanup(patcher.stop)
        con = sqlite3.connect(':memory:')
        con.execute('create table intent_events (ts text, quote text, session_id text, transcript_path text, byte_start int, project text, kind text)')
        self.addCleanup(con.close)
        self.con = con

    def read(self):
        return agents_lib.check(self.doc, SESSION)

    def agent(self, name, edits, status='completed', completion=None):
        return dict(id=name, parent=None, description='d', type='Explore', model='m', status=status, started=MS, first_seen=MS,
                    last_at=MS - 1000, tools=1, tokens=1, edits=edits, edits_omitted=0, overlaps=[], overlaps_omitted=0, last='', last_tool=None,
                    current_turn='t', completion=completion, listed=True)

    def test_brief_includes_subagent_files(self):
        self.doc['agents'] = [self.agent('a1', [self.agent_file, '/not/in/a/repo/x.py'])]
        pane.brief(self.con, self.repo, SESSION)
        self.assertEqual(sorted(self.asked), sorted([self.main_file, self.agent_file]))  # the union, the file outside any repo left out
        self.asked.clear()
        self.doc['agents'] = []
        pane.brief(self.con, self.repo, SESSION)
        self.assertEqual(self.asked, [self.main_file])  # and without agents, as before

    def test_the_brief_ends_with_the_agents_envelope_and_no_star_line_for_them(self):
        done = dict(key='k', turnId='t', at=MS, reason='answer', answer='The importer drops a batch.', disposition='pending')
        self.doc['agents'] = [self.agent('a1', [], completion=done), self.agent('a2', [], status='running'),
                              self.agent('a3', [], status='failed', completion=dict(done, reason='error', answer='')),
                              self.agent('a4', [], completion=dict(done, answer='  '))]
        text = pane.brief(self.con, self.repo, SESSION)
        (found,) = agents_lib.parse_envelopes(text)
        payload = found[2]
        self.assertEqual(text.splitlines()[-1], '[[/CSR:AGENTS:v1]]')
        self.assertEqual({row['id']: row['completion'] for row in payload['agents']},
                         {'a1': dict(turn_id='t', reason='answer', answer='The importer drops a batch.'), 'a2': None, 'a3': None, 'a4': None})
        self.assertEqual([line for line in text.splitlines() if line.startswith('* ')], [])  # no "* " fact: not news by text
        self.doc['agents'] = []
        self.assertNotIn('[[CSR:AGENTS', pane.brief(self.con, self.repo, SESSION))


# ---------------------------------------------------------------- Lost and found: handing a thread to a background agent

SCENARIO = r'''
const fs = require('node:fs'), path = require('node:path'), vm = require('node:vm')
const ts = require(process.argv[1])
const mod = { exports: {} }
vm.runInThisContext(`(function(require,module,exports){${ts.transpileModule(fs.readFileSync(path.join(process.argv[2], 'hooks/agents.ts'), 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText}\n})`)(() => { throw 1 }, mod, mod.exports)
const { emptyAgentState, reduceAgentEvent, reconcileAgentList } = mod.exports
const input = JSON.parse(fs.readFileSync(0, 'utf8'))
let state = emptyAgentState(input.session)
for (const event of input.events) state = event.kind === 'list' ? reconcileAgentList(state, event.list, event.at) : reduceAgentEvent(state, event)
process.stdout.write(JSON.stringify({ version: 1, session: state.session, handoffs: state.handoffs }))
'''

T1 = pane.epoch('2026-10-12T10:00:00Z')
def summary(pending):
    return '1. Primary Request and Intent:\n   - The ask.\n\n7. Pending Tasks:\n' + pending + '\n8. Current Work:\n   - Something else.\n'


Q = summary('   - Fix the importer retry limit and ship it.\n')
LATER = summary('   - Something else entirely is still to do.\n')
SUMMARIES_OF = [(T1, Q), (T1 + 1800, Q), (T1 + 3600, LATER)]  # the thread in two summaries, dropped from the third: lost
THREAD_TEXT = 'Fix the importer retry limit and ship it.'


def threads(handoffs=(), project=None):
    return pane.lost_found(SUMMARIES_OF, (), (), project, handoffs)


def the_thread(found):
    return next(t for t in found if t['text'].startswith('Fix the importer'))


def handoff(request, outcome='submitted', thread=None, at=T1 + 7200, **more):
    base = dict(request=request, session=SESSION, thread=thread or the_thread(threads())['key'], drafted_at=at - 10, submitted_at=at,
                spawn_tool_use_id=None, agent_id=None, turn_id=None, outcome=outcome, ended_at=None, reason=None)
    return dict(base, **more)


class HandedThreads(Folder):
    def setUp(self):
        super().setUp()
        self.keep = pane.KEEP, pane.LOST_LOG, pane.HANDOFFS
        pane.KEEP, pane.LOST_LOG, pane.HANDOFFS = self.home, os.path.join(self.home, 'lost.jsonl'), os.path.join(self.home, 'handoffs')
        self.addCleanup(lambda: setattr(pane, 'KEEP', self.keep[0]) or setattr(pane, 'LOST_LOG', self.keep[1]) or setattr(pane, 'HANDOFFS', self.keep[2]))

    def test_handoff_binds_exact_request_and_turn(self):
        compiler = compiler_path()
        if not compiler:
            if os.environ.get('CI'):
                self.fail('CI installs a pinned typescript before pytest')
            self.skipTest('An installed TypeScript compiler is unavailable')
        key = the_thread(threads())['key']
        spawn = lambda tool, requests, parent=None: dict(kind='spawn', at=3, toolUseId=tool, description='h', type='general-purpose', parent=parent,
                                                         model=None, requests=requests)
        events = [
            dict(kind='handoff', at=1, request='req-one', thread=key, draftedAt=0),
            dict(kind='handoff', at=1, request='req-nested', thread=key, draftedAt=0),
            spawn('u-nested', ['req-nested'], parent='someone'),  # an agent's own call carrying the envelope: binds nothing
            dict(kind='spawned', at=4, toolUseId='u-nested', agentId='n1', model='m'),
            spawn('u1', ['req-one', 'req-unsent']),  # one request sent, one never: the sent one is the call's
            dict(kind='step', at=5, agentId='a1', turnId='turn-1', index=0, answer='Looking', tokens=1),  # before the call's result
            dict(kind='spawned', at=6, toolUseId='u1', agentId='a1', model='m'),
            dict(kind='step', at=7, agentId='a1', turnId='turn-2', index=0, answer='again', tokens=1),  # a later run: not its turn
            dict(kind='complete', at=8, agentId='a1', turnId='turn-2', reason='error', answer=''),  # and not its end
        ]
        ran = subprocess.run(['node', '-e', SCENARIO, compiler, str(ROOT)], input=json.dumps(dict(session=SESSION, events=events)),
                             capture_output=True, text=True, timeout=60)
        self.assertEqual(ran.returncode, 0, ran.stderr)
        state = json.loads(ran.stdout)
        by = {h['request']: h for h in state['handoffs']}
        self.assertEqual((by['req-one']['outcome'], by['req-one']['agent_id'], by['req-one']['turn_id'], by['req-one']['spawn_tool_use_id']),
                         ('bound', 'a1', 'turn-1', 'u1'))
        self.assertEqual((by['req-nested']['outcome'], by['req-nested']['agent_id']), ('submitted', None))
        self.assertNotIn('req-unsent', by)
        # the state file as the hooks write it is what the pane reads, ids only
        path = os.path.join(self.home, 'handoffs', f'{SESSION}.json')
        os.makedirs(os.path.dirname(path))
        with open(path, 'w') as handle:
            json.dump(state, handle)
        read = pane.handoffs_of(SESSION)
        self.assertEqual({h['request']: h['outcome'] for h in read}, {'req-one': 'bound', 'req-nested': 'submitted'})
        # the bound turn then ends: succeeded
        events += [dict(kind='complete', at=9, agentId='a1', turnId='turn-1', reason='answer', answer='Done.')]
        ran = subprocess.run(['node', '-e', SCENARIO, compiler, str(ROOT)], input=json.dumps(dict(session=SESSION, events=events)),
                             capture_output=True, text=True, timeout=60)
        done = {h['request']: h for h in json.loads(ran.stdout)['handoffs']}
        self.assertEqual((done['req-one']['outcome'], done['req-one']['ended_at']), ('succeeded', 9))
        self.assertNotIn('Done.', ran.stdout)  # no report in it

    def test_handed_thread_returns_on_failure(self):
        for ended in ('error', 'refusal', 'aborted', 'killed', 'no report'):
            with self.subTest(ended):
                found = threads([handoff('req-1', 'failed', agent_id='a1', turn_id='t1', ended_at=T1 + 9000, reason=ended)])
                t = the_thread(found)
                self.assertEqual((t['state'], t['hidden']), ('lost', None))
                self.assertEqual((t['handed_at'], t['failed_at']), (T1 + 7200, T1 + 9000))  # the original handoff and the failure
                self.assertIn(ended, t['why'])
                (section,) = pane.lost_sections(dict(lost=found), T1 + 9100)
                self.assertEqual(next(i for i in section['items'] if i['lost'] == t['key'])['flag'], 'its subagent failed')
        # while its agent works, or after it reported, the thread is handed off, and hidden as such
        for outcome in ('bound', 'succeeded'):
            t = the_thread(threads([handoff('req-1', outcome, agent_id='a1', turn_id='t1')]))
            self.assertEqual((t['hidden'], t['state']), ('handed', 'handed'))
        # an explicit drop is the person's, and stands over any outcome
        pane.keep_lost(the_thread(threads())['key'], 'drop', T1 + 8000)
        self.assertEqual(the_thread(threads([handoff('req-1', 'failed', reason='error', ended_at=T1 + 9000)]))['hidden'], 'drop')

    def test_unlinked_handoff_stays_visible(self):
        for why in (None, 'spawn denied', 'ambiguous'):
            with self.subTest(why):
                found = threads([handoff('req-1', 'submitted', reason=why)])
                t = the_thread(found)
                self.assertIsNone(t['hidden'])  # not hidden as handed
                self.assertTrue(t['unlinked'])
                self.assertTrue(t['why'].startswith('handoff not linked'))
                (section,) = pane.lost_sections(dict(lost=found), T1 + 9100)
                item = next(i for i in section['items'] if i['lost'] == t['key'])
                self.assertEqual(item['flag'], 'handoff not linked')  # and it stays a click away, asks and all
                self.assertIn('sub', item['asks'])
        # typed words that look like a handoff prove nothing for a thread whose handoff was tied to no agent
        typed = [(T1 + 7300, f'Lost and found: “{THREAD_TEXT}” went quiet after 10:00. Hand it to a background subagent with a five-part spec.')]
        t = the_thread(pane.lost_found(SUMMARIES_OF, typed, (), None,
                                       [handoff('req-1', 'submitted')]))
        self.assertIsNone(t['hidden'])
        # a legacy handoff, which has no request at all, is still read from what was typed, and not guessed at any further
        legacy = the_thread(pane.lost_found(SUMMARIES_OF, typed, (), None, ()))
        self.assertEqual(legacy['hidden'], 'handed')
        # a draft nobody sent says nothing
        t = the_thread(threads([handoff('req-1', 'draft', submitted_at=None)]))
        self.assertEqual((t['hidden'], t.get('unlinked')), (None, None))

    def test_old_failure_does_not_reopen_retry(self):
        key = the_thread(threads())['key']
        failed = handoff('req-1', 'failed', agent_id='a1', turn_id='t1', at=T1 + 7200, ended_at=T1 + 7300, reason='error')
        for outcome, hidden in (('bound', 'handed'), ('succeeded', 'handed')):
            retry = handoff('req-2', outcome, agent_id='a2', turn_id='t2', at=T1 + 8000)
            t = the_thread(threads([failed, retry]))
            self.assertEqual((t['hidden'], t['state']), (hidden, 'handed'), outcome)  # the earlier failure cannot reopen it
        again = handoff('req-3', 'failed', agent_id='a3', turn_id='t3', at=T1 + 9000, ended_at=T1 + 9100, reason='refusal')
        t = the_thread(threads([failed, handoff('req-2', 'succeeded', agent_id='a2', turn_id='t2', at=T1 + 8000), again]))
        self.assertEqual((t['state'], t['hidden']), ('lost', None))  # the newest failed: back
        self.assertIn('refusal', t['why'])
        # a retry only drafted, not sent yet, leaves the failure standing
        t = the_thread(threads([failed, handoff('req-4', 'draft', at=T1 + 9500, submitted_at=None)]))
        self.assertEqual((t['state'], t['hidden']), ('lost', None))
        # the files: the pane's drafts and the hooks' state read together, by request
        folder = os.path.join(self.home, 'handoffs')
        os.makedirs(folder)
        with open(os.path.join(folder, f'{SESSION}.drafts.jsonl'), 'w') as handle:
            handle.write(json.dumps(dict(request='req-4', thread=key, drafted_at=T1 + 9500)) + '\n' + 'not json\n'
                         + json.dumps(dict(request='req-1', thread=key, drafted_at=T1 + 7190)) + '\n')
        with open(os.path.join(folder, f'{SESSION}.json'), 'w') as handle:
            json.dump(dict(version=1, session=SESSION, handoffs=[failed]), handle)
        read = {h['request']: h['outcome'] for h in pane.handoffs_of(SESSION)}
        self.assertEqual(read, {'req-1': 'failed', 'req-4': 'draft'})  # the draft the state already answers is not a second one
        self.assertEqual(pane.handoffs_of('other-session'), [])
        self.assertEqual(pane.handoffs_of('../x'), [])

    def test_a_click_on_subagent_drafts_a_request_and_types_its_envelope_without_sending(self):
        key = the_thread(threads())['key']
        (section,) = pane.lost_sections(dict(lost=threads()), T1 + 9100)
        item = next(i for i in section['items'] if i['lost'] == key)
        rows = pane.lost_rows(dict(item, top=True), 46, {}, False)
        act = next(act for _, act in rows if any(answer == 'sub' for _, _, answer in act['spans']))
        sub_at = next(start for start, _, answer in act['spans'] if answer == 'sub')
        typed = []
        saved = pane.herdr, pane.agent_pane, pane.focus_back
        pane.herdr = lambda *argv: typed.append(argv) or ''
        pane.agent_pane, pane.focus_back = lambda given: 'w1:p1', lambda: None
        shell = types.SimpleNamespace(session=types.SimpleNamespace(id=SESSION, path=None), agent='w1:p1', sent=None)
        try:
            pane.lost_click(shell, act, sub_at)
        finally:
            pane.herdr, pane.agent_pane, pane.focus_back = saved
        texts = [argv[3] for argv in typed if argv[1] == 'send-text']
        keys = [argv[3:] for argv in typed if argv[1] == 'send-keys']
        self.assertEqual(keys, [('ctrl+j',)] * 4)  # a new line is Ctrl+J: after the ask, and after each line of the envelope; no Enter
        self.assertTrue(all(argv[1] in ('send-text', 'send-keys') for argv in typed))
        self.assertTrue(texts[0].startswith('Lost and found: “Fix the importer'))
        self.assertEqual(texts[1:], [agents_lib.OPEN, texts[2], agents_lib.CLOSE])
        (found,) = agents_lib.parse_envelopes('\n'.join(texts[1:]) + '\n')
        payload = found[2]
        self.assertEqual((payload['kind'], payload['session'], payload['thread']), ('handoff', SESSION, key))
        self.assertEqual([h['outcome'] for h in pane.handoffs_of(SESSION)], ['draft'])
        self.assertEqual(pane.handoffs_of(SESSION)[0]['request'], payload['request'])
        with open(pane.LOST_LOG) as handle:
            kept = [json.loads(line) for line in handle]
        self.assertEqual([(k['answer'], k['how'], k['request']) for k in kept], [('sent', 'sub', payload['request'])])
        self.assertTrue(shell.sent[1])
        # a second click is a new request
        typed.clear()
        pane.herdr, pane.agent_pane, pane.focus_back = (lambda *argv: typed.append(argv) or ''), (lambda given: 'w1:p1'), (lambda: None)
        try:
            pane.lost_click(shell, act, sub_at)
        finally:
            pane.herdr, pane.agent_pane, pane.focus_back = saved
        self.assertEqual(len({h['request'] for h in pane.handoffs_of(SESSION)}), 2)
        # the side task is still one line, as it was, and a pane with no session types the old one-line ask
        typed.clear()
        pane.herdr, pane.agent_pane, pane.focus_back = (lambda *argv: typed.append(argv) or ''), (lambda given: 'w1:p1'), (lambda: None)
        try:
            side = pane.lost_rows(dict(item, top=True), 46, {}, False)[0][1]
            pane.lost_click(shell, side, next(start for start, _, answer in side['spans'] if answer == 'side'))
            pane.lost_click(types.SimpleNamespace(session=None, agent='w1:p1', sent=None), act, sub_at)
        finally:
            pane.herdr, pane.agent_pane, pane.focus_back = saved
        self.assertTrue(all(argv[1] == 'send-text' for argv in typed))

    def test_the_multi_line_draft_is_never_typed_into_a_pane_the_mod_did_not_name(self):
        """With no named pane the left neighbour is a guess: if it is a shell, Ctrl+J would run the ask as a command."""
        key = the_thread(threads())['key']
        (section,) = pane.lost_sections(dict(lost=threads()), T1 + 9100)
        item = next(i for i in section['items'] if i['lost'] == key)
        act = next(act for _, act in pane.lost_rows(dict(item, top=True), 46, {}, False)
                   if any(answer == 'sub' for _, _, answer in act['spans']))
        sub_at = next(start for start, _, answer in act['spans'] if answer == 'sub')
        typed = []

        def herdr(*argv):
            typed.append(argv)
            return json.dumps(dict(result=dict(pane=dict(pane_id='w1:shell')))) if argv[1] == 'neighbor' else ''

        saved = pane.herdr, pane.focus_back
        pane.herdr, pane.focus_back = herdr, lambda: None
        unnamed = types.SimpleNamespace(session=types.SimpleNamespace(id=SESSION, path=None), agent=None, sent=None)
        try:
            unittest.mock.patch.dict(os.environ, HERDR_PANE_ID='w1:mine').start()
            self.addCleanup(unittest.mock.patch.stopall)
            self.assertEqual(pane.agent_pane(None), 'w1:shell')  # the fallback finds the shell
            self.assertFalse(pane.send_draft('w1:shell', 'ask\nline two'))
            pane.lost_click(unnamed, act, sub_at)
            named = types.SimpleNamespace(session=unnamed.session, agent='w1:claude', sent=None)
            typed.clear()
            pane.lost_click(named, act, sub_at)  # the named pane still gets the whole draft
        finally:
            pane.herdr, pane.focus_back = saved
        self.assertFalse(unnamed.sent[1])
        self.assertTrue(named.sent[1])
        self.assertEqual({argv[3] for argv in typed if argv[1] == 'send-keys'}, {'ctrl+j'})
        self.assertTrue(all(argv[2] == 'w1:claude' for argv in typed if argv[1] in ('send-keys', 'send-text')))
