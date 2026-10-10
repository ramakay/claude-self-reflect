"""The cockpit masthead: context tape, compaction payback, cache, plan quotas and STS, and the wrapper that keeps their fields.
python3 -I -m unittest discover -s tests -p 'test_*.py'"""
import importlib.util
import json
import os
import re
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('pane', os.path.join(HERE, '..', 'bin', 'pane.py'))
pane = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pane)

NOW = 1_800_000_000.0
PLAIN = re.compile(r'\x1b\[[0-9;]*m|\x1b\[K')


def plain(lines):
    return [PLAIN.sub('', line) for line in lines]


# (tokens before, context of the first request after, ms): three of this session's own compactions
POSTS = [(159_635, 64_971, 70_542), (266_661, 64_115, 78_206), (402_895, 88_915, 102_046)]


class Payback(unittest.TestCase):
    def test_turns_to_break_even_from_this_sessions_own_median_post_size(self):
        turns, floor, seconds = pane.compaction_payback(POSTS, 200_000)
        self.assertEqual((turns, floor, seconds), (13, 64_971, 78.206))
        # (2 * 64971 + 5 * 4500 + 0.1 * 200000) / (0.1 * (200000 - 64971)) = 12.77
        self.assertAlmostEqual((2 * 64_971 + 5 * 4500 + 20_000) / (0.1 * (200_000 - 64_971)), 12.77, places=2)

    def test_a_bigger_context_pays_back_sooner(self):
        self.assertLess(pane.compaction_payback(POSTS, 300_000)[0], pane.compaction_payback(POSTS, 220_000)[0])

    def test_nothing_when_there_is_no_history_or_it_would_not_pay_back(self):
        self.assertIsNone(pane.compaction_payback([], 200_000))
        self.assertIsNone(pane.compaction_payback([(None, None, None)], 200_000))
        self.assertIsNone(pane.compaction_payback(POSTS, 0))
        self.assertIsNone(pane.compaction_payback(POSTS, 60_000))  # below the floor: compacting loses
        self.assertIsNone(pane.compaction_payback(POSTS, 150_000))  # 20 turns to repay: too slow to say
        self.assertIsNotNone(pane.compaction_payback(POSTS, 200_000))

    def test_an_even_number_of_sizes_takes_the_middle_pair(self):
        self.assertEqual(pane.compaction_payback(POSTS[:2], 200_000)[1], (64_971 + 64_115) / 2)

    def test_the_payback_reads_the_status_lines_floor_and_rounding(self):
        def js(context, posts):  # the status line's own arithmetic: the median of the last 5 posts, Math.round
            last = sorted(posts[-5:])
            floor = last[len(last) >> 1]
            saved = (context - floor) * 0.1
            turns = (floor * 2 + 4500 * 5 + context * 0.1) / saved if saved > 0 else 99
            return turns, int(turns + 0.5)

        def row2(context, sizes):
            fields = dict(context_window=dict(total_input_tokens=context, context_window_size=1_000_000))
            g = pane.gauges(fields, NOW, dict(compact_sizes=sizes), NOW)
            return plain(pane.cockpit(g, 49))[1].strip()

        sizes = [(None, p, None) for p in REAL_POSTS]
        self.assertAlmostEqual(js(300_000, REAL_POSTS)[0], 8.76, places=2)
        self.assertTrue(row2(300_000, sizes).startswith('♻ 9t'))  # the old all-posts floor (66,832) said 8
        self.assertTrue(row2(250_000, sizes).startswith('♻ 11t'))
        self.assertTrue(row2(250_000, []).startswith('♻~10t'))  # no history: the 65k guess, 9.59, marked ~
        self.assertAlmostEqual((65_000 * 2 + 22_500 + 25_000) / ((250_000 - 65_000) * 0.1), 9.59, places=2)
        for context in range(120_000, 600_001, 20_000):
            turns, shown = js(context, REAL_POSTS)
            want = f'♻ {shown}t' if turns <= 15 else '♻ >15t'
            self.assertTrue(row2(context, sizes).startswith(want), (context, turns, row2(context, sizes)))

    def test_a_reading_at_or_under_the_floor_never_pays_back_and_says_so(self):
        sizes = [(None, p, None) for p in REAL_POSTS]
        fields = dict(context_window=dict(total_input_tokens=60_000, context_window_size=1_000_000))
        self.assertEqual(pane.gauges(fields, NOW, dict(compact_sizes=sizes), NOW)['payback']['text'], '♻ >15t')
        self.assertEqual(pane.gauges({}, None, dict(compact_sizes=sizes), NOW)['payback']['text'], '♻ --')

    def test_the_payback_ink_follows_the_turns_to_repay(self):
        sizes = [(None, 65_000, None)]
        sev = lambda context: pane.gauges(dict(context_window=dict(total_input_tokens=context)), NOW,
                                          dict(compact_sizes=sizes), NOW)['payback']['sev']
        self.assertEqual((sev(1_000_000), sev(300_000), sev(250_000)), ('red', 'amber', 'advisory'))  # 2.7, 7.8, 9.6 turns

    def test_a_transcript_gives_its_compaction_rows(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, 's.jsonl')
            with open(path, 'w') as handle:
                for pre, post, ms in POSTS[:2]:
                    handle.write(json.dumps(dict(type='system', subtype='compact_boundary',
                                                 timestamp='2026-10-10T10:00:00Z',
                                                 compactMetadata=dict(trigger='manual', preTokens=pre, postTokens=19_000,
                                                                      durationMs=ms))) + '\n')
                    handle.write(json.dumps(dict(type='assistant', timestamp='2026-10-10T10:01:00Z', message=dict(
                        id=f'm{pre}', usage=dict(input_tokens=1, cache_read_input_tokens=post - 1)))) + '\n')
                handle.write(json.dumps(dict(type='system', subtype='compact_boundary',
                                             timestamp='2026-10-10T11:00:00Z')) + '\n')  # no metadata at all
                handle.write(json.dumps(dict(type='system', subtype='compact_boundary', compactMetadata='odd',
                                             timestamp='2026-10-10T12:00:00Z')) + '\n')  # metadata not a dict
                handle.write('{"type": "system", "subtype": "compact_bou\n')  # a torn line
            session = pane.Session('sess-x')
            session.path = path
            session.read_transcript()
            self.assertEqual(session.compact_sizes, POSTS[:2] + [(None, None, None)] * 2)
            self.assertEqual(len(session.compacts), 4)


# the last 16 post-compaction contexts of a real session (receipt R6): floor of the last 5 = 73,120
REAL_POSTS = [64971, 64115, 62570, 63631, 88915, 67855, 65809, 64168, 68675, 69778, 64700, 69746, 61437, 77136,
              80254, 73120]


def sgr(name):
    return '\x1b[38;2;%d;%d;%dm' % pane.SEV[name][:3]


def session(**more):
    return dict(dict(id='sess-now', prompts=[], replies=[], compacts=[], compact_sizes=[], main=[], remembered=[],
                     gave=dict(recalls=[], carried=[], dreams=[]), project='csr-engine'), **more)


def fields_of(tokens=None, size=1_000_000, cache=None, limits=None):
    out = {}
    if tokens is not None:
        out['context_window'] = dict(total_input_tokens=tokens, context_window_size=size)
    if cache is not None:
        out['prompt_cache'] = cache
    if limits is not None:
        out['rate_limits'] = limits
    return out


def limits_of(five, seven, five_at=NOW + 7200, seven_at=NOW + 3 * 86400):
    return dict(five_hour=dict(used_percentage=five, resets_at=five_at),
                seven_day=dict(used_percentage=seven, resets_at=seven_at))


class Cockpit(unittest.TestCase):
    def rows(self, fields=None, s=None, width=49, **more):
        g = pane.gauges(fields, None, s or session(), NOW, **more)
        return pane.cockpit(g, width)

    def test_the_tape_reads_the_context_against_the_window_with_the_floor_bug(self):  # A1
        fields = fields_of(87_724)
        rows = self.rows(fields, session(compact_sizes=POSTS))
        self.assertEqual(plain(rows)[0], ' CTX ▕███│██' + '░' * 18 + '▏ 88k/1M')
        bare = plain(self.rows({}))[0]
        self.assertEqual(bare, ' CTX ▕░░░│' + '░' * 20 + '▏ --')  # no data: '--', the 65k guess for the bug
        # the bug is in the data ink, the rest of the bar in the severity's ink
        self.assertIn(sgr('data') + '│' + pane.OFF, rows[0])
        self.assertIn(sgr('normal') + '▕███' + pane.OFF, rows[0])

    def test_the_tape_lights_amber_at_100k_and_red_at_400k_and_pegs_the_bar(self):
        amber = self.rows(fields_of(150_000))[0]
        self.assertIn(sgr('amber') + '▕', amber)
        red = self.rows(fields_of(432_000))[0]
        self.assertIn(sgr('red') + '▕', red)
        self.assertEqual(plain([red])[0].count('█'), 23)  # past the full scale: the bar is full, the value exact
        self.assertTrue(plain([red])[0].endswith('432k/1M'))

    def test_the_floor_is_the_median_of_the_last_five_posts(self):  # A2
        self.assertEqual(pane.floor_of([(None, p, None) for p in REAL_POSTS]), (73_120, True))
        self.assertEqual(pane.floor_of(REAL_POSTS), (73_120, True))
        self.assertEqual(pane.floor_of([]), (65_000, False))
        self.assertEqual(pane.floor_of([(None, None, None), (1, 0, 2)]), (65_000, False))
        self.assertEqual(pane.floor_of([(None, 100, None), (None, 201, None)]), (151, True))  # Math.round(150.5)

    def test_the_trend_is_the_median_of_the_turn_to_turn_steps_since_the_compaction(self):  # A4
        main = [(100.0 + 100 * k + dt, ctx - extra) for k, ctx in enumerate((100_000, 112_000, 120_000, 135_000), 1)
                for dt, extra in ((5, 2_000), (10, 0))]
        s = session(main=main, prompts=[100.0 + 100 * k for k in range(1, 5)], compacts=[50.0])
        self.assertIn(' ▲12k/t', plain(self.rows(fields_of(), s, width=60))[0])
        two = session(main=main[:4], prompts=[200.0, 300.0], compacts=[50.0])
        self.assertNotIn('▲', plain(self.rows(fields_of(), two, width=60))[0])
        down = session(main=[(105.0, 130_000), (205.0, 120_000), (305.0, 108_000)], prompts=[100.0, 200.0, 300.0])
        self.assertIn('▼11k/t', plain(self.rows(fields_of(), down, width=60))[0])
        before = session(main=main, prompts=[100.0 + 100 * k for k in range(1, 5)], compacts=[450.0])
        self.assertIsNone(pane.gauges(fields_of(), NOW, before, NOW)['trend'])  # the compaction cut the turns

    def test_the_newest_reply_beats_a_kept_status_line_older_than_it(self):
        fields = fields_of(80_000)
        main = [(NOW - 10, 91_000)]
        self.assertEqual(pane.ctx_now(fields, NOW - 60, dict(main=main)), (91_000, 1_000_000))
        self.assertEqual(pane.ctx_now(fields, NOW - 12, dict(main=main)), (80_000, 1_000_000))  # not 5 s newer
        usage = dict(context_window=dict(current_usage=dict(input_tokens=2, cache_read_input_tokens=30, cache_creation_input_tokens=8)))
        self.assertEqual(pane.ctx_now(usage, None, {}), (40, None))
        self.assertEqual(pane.ctx_now({}, None, {}), (None, None))

    def test_a_dark_cockpit_lights_only_what_deviates(self):  # A5
        warm = lambda left: dict(warm=True, ttl='1h', expires_at=NOW + left)
        row2 = lambda left: self.rows(fields_of(cache=warm(left)))[1]
        self.assertIn(sgr('normal') + 'CACHE 41m' + pane.OFF, row2(41 * 60 + 20))
        self.assertIn(sgr('amber') + 'CACHE 4m' + pane.OFF, row2(4 * 60 + 10))
        self.assertIn(sgr('amber') + 'CACHE <1m' + pane.OFF, row2(30))
        cold = dict(warm=False, expires_at=NOW - 10, recache_tokens_if_cold=152_300)
        self.assertIn(sgr('amber') + 'CACHE COLD ~152k' + pane.OFF, self.rows(fields_of(cache=cold))[1])
        self.assertIn(sgr('normal') + 'CACHE --' + pane.OFF, self.rows({})[1])
        self.assertIn(sgr('normal') + 'CACHE 5m' + pane.OFF, self.rows(fields_of(cache=dict(warm=True, ttl='5m')))[1])
        row3 = self.rows(fields_of(limits=limits_of(95, 75)))[2]
        reset = datetime.fromtimestamp(NOW + 3 * 86400).strftime('%a %H:%M')
        soon = datetime.fromtimestamp(NOW + 7200)  # the time alone only when that is today, else the weekday too
        soon = soon.strftime('%H:%M' if soon.date() == datetime.fromtimestamp(NOW).date() else '%a %H:%M')
        self.assertIn(sgr('amber') + f'7D 75% ↻{reset}' + pane.OFF, row3)
        self.assertIn(sgr('red') + '5H 95% ↻' + soon + pane.OFF, row3)
        low = self.rows(fields_of(limits=limits_of(8, 12)))[2]
        self.assertIn(sgr('normal') + '5H 8%' + pane.OFF, low)
        self.assertNotIn('↻', low)

    def test_the_riskier_window_comes_first(self):  # A6
        first = lambda five, seven: plain(self.rows(fields_of(limits=limits_of(five, seven))))[2].split()[0]
        self.assertEqual(first(8, 75), '7D')
        self.assertEqual(first(92, 75), '5H')
        self.assertEqual(first(30, 30), '5H')  # a tie keeps the 5-hour window first

    def test_a_value_that_went_invalid_is_dashed_in_amber_and_one_never_seen_in_the_normal_ink(self):  # A7
        gone = limits_of(95, 12, five_at=NOW - 60)
        row3 = self.rows(fields_of(limits=gone))[2]
        self.assertIn(sgr('amber') + '5H --' + pane.OFF, row3)
        self.assertNotIn('95', row3)  # never a stale number
        self.assertTrue(plain([row3])[0].strip().startswith('7D 12%'))  # a '--' goes last
        none = self.rows({})[2]
        self.assertIn(sgr('normal') + '5H --' + pane.OFF, none)
        self.assertIn(sgr('normal') + '7D --' + pane.OFF, none)
        only = self.rows(fields_of(limits=dict(seven_day=dict(used_percentage=41, resets_at=NOW + 99))))[2]
        self.assertTrue(plain([only])[0].strip().startswith('7D 41%'))
        self.assertIn(sgr('normal') + '5H --' + pane.OFF, only)

    def test_the_account_limits_come_from_another_session_within_a_day(self):  # A8
        def keep(folder, name, at, limits):
            with open(os.path.join(folder, name + '.json'), 'w') as handle:
                json.dump(dict(at=at, line='x', fields=dict(rate_limits=limits)), handle)

        with tempfile.TemporaryDirectory() as tmp:
            keep(tmp, 'old', NOW - 90_000, limits_of(99, 99))
            self.assertEqual(pane.account_limits(NOW, tmp), {})
            keep(tmp, 'other', NOW - 3_600, limits_of(33, 44))
            self.assertEqual(pane.account_limits(NOW, tmp), limits_of(33, 44))
            keep(tmp, 'newer', NOW - 60, limits_of(35, 45))
            with open(os.path.join(tmp, 'broken.json'), 'w') as handle:
                handle.write('{not json')
            self.assertEqual(pane.account_limits(NOW, tmp), limits_of(35, 45))
            self.assertEqual(pane.account_limits(NOW + 86_400, tmp), {})  # a day on: both are old
        self.assertEqual(pane.account_limits(NOW, '/nonexistent/folder'), {})
        # a session with no status line of its own still shows the account's windows
        row3 = self.rows(fields_of(), limits=limits_of(33, 44))[2]
        self.assertTrue(plain([row3])[0].strip().startswith('7D 44%'))
        own = self.rows(fields_of(limits=limits_of(10, 11)), limits=limits_of(33, 44))[2]
        self.assertTrue(plain([own])[0].strip().startswith('7D 11%'))  # its own fields win

    def test_sts_names_what_csr_runs_without_and_never_reads_out_another_variable(self):  # A9
        import plistlib
        with tempfile.TemporaryDirectory() as tmp:
            agents = os.path.join(tmp, 'agents')
            os.makedirs(agents)
            with open(os.path.join(agents, 'com.x.csr-engine.daemon.plist'), 'wb') as handle:
                plistlib.dump({'Label': 'x', 'EnvironmentVariables': {'PATH': 'x', 'CSR_NO_RATIFICATION': '1',
                                                                       'CSR_NO_RECAP': '0', 'CSR_STORY': '1'}}, handle)
            with open(os.path.join(agents, 'com.x.other.plist'), 'wb') as handle:
                plistlib.dump({'EnvironmentVariables': {'CSR_NO_OTHER': '1'}}, handle)  # not a csr-engine agent
            lock = os.path.join(tmp, 'daemon.lock')
            with open(lock, 'w') as handle:
                handle.write(str(os.getpid()))
            settings = os.path.join(tmp, 'settings.json')
            with open(settings, 'w') as handle:
                json.dump(dict(env=dict(CSR_NO_AI_NARRATIVES='true', CSR_NO_DREAMING='false', SECRET='shh')), handle)
            got = pane.csr_status(NOW, agents, lock, settings)
            self.assertEqual(got['sev'], 'advisory')
            self.assertEqual(got['inop'], [('RATIFICATION', 'INHIBITED · CSR_NO_RATIFICATION=1 (daemon)', 'advisory'),
                                           ('AI_NARRATIVES', 'INHIBITED · CSR_NO_AI_NARRATIVES=true (hooks)', 'advisory')])
            self.assertTrue(got['daemon']['up'])
            for secret in ('PATH', 'SECRET', 'shh', 'CSR_STORY'):
                self.assertNotIn(secret, json.dumps(got))
            only_plist = pane.csr_status(NOW, agents, lock, os.path.join(tmp, 'none.json'))
            self.assertEqual([row[0] for row in only_plist['inop']], ['RATIFICATION'])
            done = subprocess.Popen(['true'])
            done.wait()
            dead = os.path.join(tmp, 'dead.lock')
            with open(dead, 'w') as handle:
                handle.write(str(done.pid))
            down = pane.csr_status(NOW, agents, dead, settings)
            self.assertEqual((down['sev'], down['inop'][0]), ('amber', ('DAEMON', 'DOWN · no live pid in daemon.lock', 'amber')))
            nothing = pane.csr_status(NOW, os.path.join(tmp, 'x'), os.path.join(tmp, 'no.lock'), os.path.join(tmp, 'no.json'))
            self.assertEqual([row[0] for row in nothing['inop']], ['DAEMON'])
            with open(os.path.join(tmp, 'garbled.lock'), 'w') as handle:
                handle.write('not a pid')
            self.assertEqual(pane.csr_status(NOW, os.path.join(tmp, 'x'), os.path.join(tmp, 'garbled.lock'),
                                             os.path.join(tmp, 'no.json'))['sev'], 'amber')

    def test_sts_is_read_once_a_minute(self):
        with tempfile.TemporaryDirectory() as tmp:
            lock = os.path.join(tmp, 'daemon.lock')
            args = (os.path.join(tmp, 'agents'), lock, os.path.join(tmp, 'settings.json'))
            with open(lock, 'w') as handle:
                handle.write(str(os.getpid()))
            self.assertIsNone(pane.csr_status(NOW, *args)['sev'])
            os.remove(lock)
            self.assertIsNone(pane.csr_status(NOW + 30, *args)['sev'])  # kept
            self.assertEqual(pane.csr_status(NOW + 61, *args)['sev'], 'amber')  # read again

    def test_sts_lights_row_three_in_its_severity_ink(self):
        advisory = dict(sev='advisory', inop=[('RATIFICATION', 'x', 'advisory')])
        self.assertIn(sgr('advisory') + 'STS' + pane.OFF, self.rows({}, sts=advisory)[2])
        amber = dict(sev='amber', inop=[('DAEMON', 'x', 'amber')])
        self.assertIn(sgr('amber') + 'STS' + pane.OFF, self.rows({}, sts=amber)[2])
        self.assertNotIn('STS', plain(self.rows({}))[2])
        self.assertTrue(plain(self.rows({}, sts=advisory))[2].endswith('STS'))  # right-aligned

    def test_while_a_compaction_runs_its_timer_takes_row_two(self):  # A10
        usual = dict(low=50.0, median=75.0, high=100.0, n=9)
        info = dict(elapsed=72, trigger='manual', usual=usual, facts=['CSR keeps 12 chunks of this session'])
        rows = self.rows(fields_of(300_000, cache=dict(warm=True, expires_at=NOW + 3000)), compacting=info)
        self.assertEqual(len(rows), 3)
        row2 = plain(rows)[1]
        self.assertTrue(row2.strip().startswith('COMPACTING 1:12 █████▊░░ 0:50–1:40'), row2)
        self.assertNotIn('♻', row2)
        self.assertTrue(row2.endswith('CACHE 50m'))
        self.assertIn(sgr('advisory') + 'COMPACTING', rows[1])
        late = self.rows({}, compacting=dict(info, elapsed=130, trigger='auto'))[1]
        self.assertIn(sgr('amber') + 'COMPACTING AUTO 2:10', late)  # past the usual longest
        bare = plain(self.rows({}, compacting=dict(elapsed=5, trigger='manual', usual=None, facts=[])))[1]
        self.assertTrue(bare.strip().startswith('COMPACTING 0:05 '))
        for width in (24, 30, 40):  # the range goes, then the bar, then the cache
            for row in plain(pane.cockpit(pane.gauges(fields_of(), NOW, session(), NOW, compacting=info), width)):
                self.assertLessEqual(len(row), width)
        row2 = lambda width: plain(pane.cockpit(pane.gauges(fields_of(), NOW, session(), NOW, compacting=info), width))[1]
        self.assertTrue(row2(40).startswith(' COMPACTING 1:12 █████▊░░ ') and '0:50' not in row2(40))  # the range goes
        self.assertTrue(row2(40).endswith('CACHE --'))
        self.assertTrue('█' not in row2(30) and row2(30).strip().startswith('COMPACTING 1:12') and row2(30).endswith('CACHE --'))  # then the bar
        self.assertEqual(row2(24), ' COMPACTING 1:12')  # then the right slot

    def test_three_rows_at_every_width_and_sts_always_survives(self):  # A11
        fields = fields_of(432_000, cache=dict(warm=False, recache_tokens_if_cold=255_000),
                           limits=limits_of(92, 75))
        main = [(100.0 + 100 * k + 5, 100_000 + 60_000 * k) for k in range(1, 5)]
        s = session(main=main, prompts=[100.0 + 100 * k for k in range(1, 5)], compact_sizes=POSTS)
        sts = dict(sev='amber', inop=[('DAEMON', 'x', 'amber')])
        info = dict(elapsed=72, trigger='auto', usual=dict(low=50.0, median=75.0, high=100.0, n=9), facts=[])
        for compacting in (None, info):
            for width in range(24, 81):
                g = pane.gauges(fields, NOW, s, NOW, sts=sts, compacting=compacting)
                rows = pane.cockpit(g, width)
                self.assertEqual(len(rows), 3, width)
                for row in plain(rows):
                    self.assertLessEqual(len(row), width, (width, row))
                    self.assertTrue(row.startswith(' '))
                self.assertTrue(plain(rows)[2].endswith('STS'), (width, plain(rows)[2]))

    def test_rows_that_cannot_hold_everything_shrink_in_a_fixed_order(self):
        fields = fields_of(87_724, size=1_000_000, limits=limits_of(95, 75))
        main = [(100.0 + 100 * k + 5, 60_000 + 12_000 * k) for k in range(1, 5)]
        s = session(main=main, prompts=[100.0 + 100 * k for k in range(1, 5)])
        row1 = lambda width: plain(pane.cockpit(pane.gauges(fields, NOW, s, NOW), width))[0]
        self.assertTrue(row1(60).endswith('/1M ▲12k/t'))
        self.assertTrue(row1(29).endswith('/1M') and '▲' not in row1(29))  # the trend goes first
        self.assertEqual(row1(24).count('█') + row1(24).count('░'), 8)  # the bar shrinks to 8 cells
        big = pane.gauges(fields_of(1_200_000, size=10_000_000), None, session(), NOW)
        self.assertTrue(plain(pane.cockpit(big, 40))[0].endswith('1.2M/10M'))
        self.assertTrue(plain(pane.cockpit(big, 24))[0].endswith('1.2M'))  # then the window
        row3 = lambda width: plain(pane.cockpit(pane.gauges(fields, NOW, s, NOW, sts=dict(sev='amber', inop=[])), width))[2]
        self.assertEqual((row3(40).count('↻'), row3(30).count('↻'), row3(24).count('↻')), (2, 1, 1))  # the second window goes
        self.assertTrue(all(row3(width).endswith('STS') for width in (24, 30, 40)))  # STS never does
        self.assertTrue(row3(24).strip().startswith('5H 95%'))

    def test_facts_drop_their_rightmost_pieces_to_fit(self):
        facts = '12 prompts · 3h20m · compact repays in ~6t · 13 compacts'
        self.assertEqual(pane.fit_pieces(facts, 60), facts)
        self.assertEqual(pane.fit_pieces(facts, 42), '12 prompts · 3h20m · compact repays in ~6t')
        self.assertEqual(pane.fit_pieces(facts, 20), '12 prompts · 3h20m')
        self.assertEqual(pane.fit_pieces(facts, 6), '12 pr…')

    def test_numbers_no_clock_can_show_never_raise(self):
        nan, inf = float('nan'), float('inf')
        cache = lambda **c: pane.cache_info(c, NOW)
        self.assertEqual(cache(warm=True, expires_at=nan)['text'], 'CACHE WARM')
        self.assertEqual(cache(warm=True, expires_at=1e20)['text'], 'CACHE WARM')
        self.assertEqual(cache(warm=True, expires_at=NOW + 9 * 86400)['text'], 'CACHE WARM')
        for cost in (nan, inf, -5, 0):
            self.assertEqual(cache(warm=False, recache_tokens_if_cold=cost)['text'], 'CACHE COLD')
        for odd in (None, {}, [], 'warm', dict(ttl='1h'), dict(warm=None), dict(warm=True, expires_at=[1])):
            self.assertIn(pane.cache_info(odd, NOW)['text'], ('CACHE --', 'CACHE WARM'), odd)
        for used, at in ((nan, NOW + 60), (inf, NOW + 60), (95, 1e20), (95, inf), (95, -5)):
            got = pane.quota_slot('5H', dict(used_percentage=used, resets_at=at), NOW)
            self.assertNotIn('nan', got['text'].lower())
        self.assertEqual(pane.quota_slot('5H', dict(used_percentage='lots'), NOW)['text'], '5H --')
        self.assertEqual(pane.quota_slot('5H', 'x', NOW)['text'], '5H --')
        resets = pane.epoch('2027-01-15T08:00:00Z')
        self.assertIn('↻', pane.quota_slot('5H', dict(used_percentage=80, resets_at='2027-01-15T08:00:00Z'), resets - 7200)['text'])

    def test_the_page_head_is_the_cockpit_and_nothing_else(self):  # A12
        s = session(prompts=[NOW - 100] * 12, replies=[(NOW - 50, 90_000, 5, 0)], model='claude-opus-5-5',
                    compacts=[NOW - 9000], compact_sizes=POSTS)
        page = stand_in(fields_of(90_000, cache=dict(warm=True, expires_at=NOW + 3000), limits=limits_of(8, 75)), s)
        lines = pane.page_lines(page, 50, 24, {})
        text = plain(lines)
        self.assertEqual(text[:3], plain(pane.cockpit(page['head']['gauges'], 50)))
        for line in text[:3]:
            self.assertNotRegex(line, r'prompts|Opus|opus|\d+h\d\dm|compacts')
        self.assertEqual(text[3].strip(), '')  # the lip, or the first strip of paper
        self.assertIsNone(lines.lanes)
        self.assertFalse(any('learned' in line or 'handed' in line for line in text[:8]))
        tall = pane.page_lines(page, 50, pane.LIP_ROWS, {})
        lip = '\x1b[48;2;%d;%d;%dm' % pane.blend(pane.DARK[1], pane.PAPER, 0.6)
        self.assertTrue(tall[3].startswith(lip))


def stand_in(fields, s=None, **more):
    """page_main over a stand-in CSR with the status line's fields as given."""
    con = sqlite3.connect(':memory:')
    con.execute('create table intent_events (ts text, quote text, session_id text, transcript_path text, '
                'byte_start int, project text, kind text)')
    connect, pane.connect = pane.connect, lambda: con
    try:
        s = s or session(prompts=[NOW - 100] * 12)
        data = dict(repo='/tmp/csr-engine', dreams=[], chapter=dict(at=1000.0, of=5), dreams_on=True, look=None,
                    story=None, claude_fields=fields, claude_line='Opus 5 │ 57k', **more)
        return pane.page_main(data, s, NOW)
    finally:
        pane.connect = connect


class KeptFields(unittest.TestCase):
    def test_fields_come_back_from_the_kept_file_and_not_when_a_day_old(self):
        with tempfile.TemporaryDirectory() as tmp:
            kept, pane.STATUS_LINES = pane.STATUS_LINES, tmp
            try:
                with open(os.path.join(tmp, 'sess-1.json'), 'w') as handle:
                    json.dump(dict(at=NOW - 60, line='x', fields=dict(prompt_cache=dict(warm=True))), handle)
                with open(os.path.join(tmp, 'sess-2.json'), 'w') as handle:
                    json.dump(dict(at=NOW - 90_000, line='x', fields=dict(prompt_cache=dict(warm=True))), handle)
                with open(os.path.join(tmp, 'sess-3.json'), 'w') as handle:
                    json.dump(dict(at=NOW - 60, line='x'), handle)  # kept by the older wrapper: no fields
                self.assertEqual(pane.claude_fields('sess-1', NOW), dict(prompt_cache=dict(warm=True)))
                self.assertEqual(pane.claude_fields('sess-2', NOW), {})
                self.assertEqual(pane.claude_fields('sess-3', NOW), {})
                self.assertEqual(pane.claude_fields('missing', NOW), {})
                self.assertEqual(pane.claude_fields(None, NOW), {})
            finally:
                pane.STATUS_LINES = kept


@unittest.skipUnless(shutil.which('node'), 'node is not installed')
class Wrapper(unittest.TestCase):
    """bin/statusline.cjs run for real under a throwaway HOME, with a pane "open" (this process's pid)."""
    WRAPPER = os.path.join(HERE, '..', 'bin', 'statusline.cjs')
    STATUS = dict(session_id='sess-w', model=dict(display_name='Opus'),
                  prompt_cache=dict(warm=True, ttl='1h', expires_at=1_800_003_000, hit_ratio=0.91, misses=0,
                                    last_miss_cause=None, recache_tokens_if_cold=152_000),
                  rate_limits=dict(five_hour=dict(used_percentage=34, resets_at=1_800_003_600),
                                   seven_day=dict(used_percentage=12, resets_at=1_800_400_000)),
                  context_window=dict(total_input_tokens=57_000, context_window_size=1_000_000, remaining_percentage=94))

    def run_wrapper(self, home, status, pane_open=True, beat_age=0, raw=None):
        keep = os.path.join(home, '.claude-self-reflect', 'csr-companion')
        if pane_open:
            os.makedirs(os.path.join(keep, 'panes'))
            pid_file = os.path.join(keep, 'panes', 'sess-w.json')
            with open(pid_file, 'w') as handle:
                json.dump(dict(pid=os.getpid()), handle)
            if beat_age:
                then = __import__('time').time() - beat_age
                os.utime(pid_file, (then, then))
        done = subprocess.run(['node', self.WRAPPER, '--sh', 'cat >/dev/null; echo drawn' if raw is None else 'cat'],
                              input=(raw if raw is not None else json.dumps(status).encode()),
                              capture_output=True, env=dict(os.environ, HOME=home), timeout=30)
        done.stdout = done.stdout.decode('utf-8', 'replace')
        return done, os.path.join(keep, 'statusline')

    def test_a_pid_file_the_pane_stopped_touching_is_a_pane_that_is_gone(self):
        with tempfile.TemporaryDirectory() as home:
            done, folder = self.run_wrapper(home, self.STATUS, beat_age=120)  # a live pid, but no heartbeat for 2 min
            self.assertEqual(done.stdout.strip(), 'drawn')
            self.assertFalse(os.path.exists(folder))

    def test_a_character_split_across_input_chunks_reaches_the_command_whole(self):
        for pad in ('', 'a'):  # one of the two puts a 64 KB chunk edge inside an é (the old code broke 10 of them)
            where = '/repo/' + pad + 'é' * 40000
            status = json.dumps(dict(session_id='sess-nopane', workspace=dict(current_dir=where)), ensure_ascii=False).encode()
            with tempfile.TemporaryDirectory() as home:
                done, _ = self.run_wrapper(home, None, pane_open=False, raw=status)
            self.assertNotIn('\ufffd', done.stdout)
            self.assertEqual(json.loads(done.stdout)['workspace']['current_dir'], where)

    def test_the_raw_fields_are_kept_beside_the_line_atomically(self):
        with tempfile.TemporaryDirectory() as home:
            done, folder = self.run_wrapper(home, self.STATUS)
            self.assertEqual((done.returncode, done.stdout), (0, ''))  # drawn in the pane, not under the prompt
            self.assertEqual(os.listdir(folder), ['sess-w.json'])  # no .tmp left behind
            kept = json.load(open(os.path.join(folder, 'sess-w.json')))
            self.assertEqual(kept['line'].strip(), 'drawn')
            self.assertEqual(kept['fields'], {k: self.STATUS[k] for k in ('prompt_cache', 'rate_limits', 'context_window')})
            self.assertNotIn('model', kept['fields'])

    def test_absent_fields_stay_absent_and_the_pane_reads_it_as_nothing(self):
        with tempfile.TemporaryDirectory() as home:
            bare = dict(session_id='sess-w', rate_limits=None, prompt_cache='odd')
            done, folder = self.run_wrapper(home, bare)
            kept = json.load(open(os.path.join(folder, 'sess-w.json')))
            self.assertEqual(kept['fields'], {})
            g = pane.gauges(kept['fields'], None, {}, NOW)
            self.assertEqual((g['cache']['text'], [q['text'] for q in g['quotas']]), ('CACHE --', ['5H --', '7D --']))

    def test_without_a_pane_the_line_passes_through_and_nothing_is_kept(self):
        with tempfile.TemporaryDirectory() as home:
            done, folder = self.run_wrapper(home, self.STATUS, pane_open=False)
            self.assertEqual(done.stdout.strip(), 'drawn')
            self.assertFalse(os.path.exists(folder))

    def test_what_the_wrapper_kept_is_what_the_masthead_draws(self):
        with tempfile.TemporaryDirectory() as home:
            _, folder = self.run_wrapper(home, self.STATUS)
            kept, pane.STATUS_LINES = pane.STATUS_LINES, folder
            try:
                fields = pane.claude_fields('sess-w', __import__('time').time())
            finally:
                pane.STATUS_LINES = kept
            g = pane.gauges(fields, None, {}, 1_800_000_000.0)
            self.assertEqual(g['cache']['text'], 'CACHE 50m')
            self.assertEqual(plain(pane.cockpit(g, 49))[1].strip(), '♻ >15t' + ' ' * 33 + 'CACHE 50m')


class ReviewFindings(unittest.TestCase):
    """Defects the review of the cockpit found, each reproduced first."""

    def test_a_plist_that_is_not_xml_is_skipped_and_the_others_still_read(self):  # finding 1
        import plistlib
        with tempfile.TemporaryDirectory() as tmp:
            agents = os.path.join(tmp, 'agents')
            os.makedirs(agents)
            with open(os.path.join(agents, 'com.x.csr-engine.broken.plist'), 'w') as handle:
                handle.write('<plist><dict><key>EnvironmentVariables')  # an ExpatError, which is no ValueError
            with open(os.path.join(agents, 'com.x.csr-engine.binary.plist'), 'wb') as handle:
                handle.write(b'bplist00garbage')
            with open(os.path.join(agents, 'com.x.csr-engine.daemon.plist'), 'wb') as handle:
                plistlib.dump({'EnvironmentVariables': {'CSR_NO_RECAP': '1'}}, handle)
            lock = os.path.join(tmp, 'daemon.lock')
            with open(lock, 'w') as handle:
                handle.write(str(os.getpid()))
            got = pane.csr_status(NOW, agents, lock, os.path.join(tmp, 'none.json'))
            self.assertEqual([row[0] for row in got['inop']], ['RECAP'])

    def test_a_daemon_lock_with_an_absurd_number_is_a_daemon_that_is_down(self):  # finding 2
        with tempfile.TemporaryDirectory() as tmp:
            lock = os.path.join(tmp, 'daemon.lock')
            for text in ('9' * 30, '-' + '9' * 30, '9' * 5000):
                with open(lock, 'w') as handle:
                    handle.write(text)
                got = pane.csr_status(NOW + len(text), os.path.join(tmp, 'x'), lock, os.path.join(tmp, 'none.json'))
                self.assertEqual((got['sev'], got['inop'][0][0]), ('amber', 'DAEMON'), text[:10])

    def test_a_threshold_set_to_zero_never_divides_by_zero(self):  # finding 6
        saved = {k: os.environ.get(k) for k in ('CC_CTX_RED_TOKENS', 'CC_CTX_AMBER_TOKENS')}
        try:
            for red, amber in (('0', '0'), ('-5', '-1'), ('abc', 'x')):
                os.environ.update(CC_CTX_RED_TOKENS=red, CC_CTX_AMBER_TOKENS=amber)
                loaded = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(loaded)
                self.assertGreaterEqual(loaded.CTX_RED, 1)
                self.assertGreaterEqual(loaded.CTX_AMBER, 1)
                g = loaded.gauges(fields_of(150_000), None, session(), NOW)
                self.assertEqual(len(loaded.cockpit(g, 49)), 3)  # tape_row divides by CTX_RED
        finally:
            for key, value in saved.items():
                os.environ.pop(key, None) if value is None else os.environ.__setitem__(key, value)

    def test_a_reset_tomorrow_names_its_day_even_under_a_day_away(self):  # finding 7
        window = lambda at: dict(used_percentage=80, resets_at=at)
        late = datetime(2027, 1, 15, 23, 0).timestamp()
        tomorrow = late + 8 * 3600  # 07:00 the next morning: 8 hours away, but not today
        self.assertEqual(pane.quota_slot('7D', window(tomorrow), late)['text'],
                         '7D 80% ↻' + datetime.fromtimestamp(tomorrow).strftime('%a 07:00'))
        morning = datetime(2027, 1, 15, 9, 0).timestamp()
        self.assertEqual(pane.quota_slot('5H', window(morning + 8 * 3600), morning)['text'], '5H 80% ↻17:00')  # today: time only
        self.assertEqual(pane.reset_label(tomorrow, late), datetime.fromtimestamp(tomorrow).strftime('%a 07:00'))
        self.assertEqual(pane.reset_label(morning + 3600, morning), '10:00')


if __name__ == '__main__':
    unittest.main()
