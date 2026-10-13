"""The cockpit masthead: context tape, compaction payback, cache, plan quotas and STS, and the wrapper that keeps their fields.
python3 -I -m unittest discover -s tests -p 'test_*.py'"""
import math
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


def turns_of(per_turn, count=3):
    """A stand-in session's prompts and main-chain requests: `count` whole turns of `per_turn` requests each, and a
    running one."""
    prompts = [NOW - 1000 + 100 * k for k in range(count + 1)]
    main = [(p + 1 + i, 100_000) for p in prompts[:-1] for i in range(per_turn)]
    return dict(prompts=prompts, main=main)


class Payback(unittest.TestCase):
    def test_requests_to_break_even_from_this_sessions_own_median_post_size(self):
        floor, measured = pane.floor_of(POSTS)
        self.assertEqual((floor, measured), (64_971, True))
        # (2 * 64971 + 5 * 21700 + 0.1 * 200000) / (0.1 * (200000 - 64971)) = 19.14 requests
        self.assertAlmostEqual(pane.payback_requests(floor, 200_000), 19.14, places=2)
        self.assertAlmostEqual(pane.payback_turns(pane.payback_requests(floor, 200_000), 17), 1.13, places=2)

    def test_a_bigger_context_pays_back_sooner(self):
        self.assertLess(pane.payback_requests(64_971, 300_000), pane.payback_requests(64_971, 220_000))

    def test_nothing_at_or_under_the_floor(self):
        self.assertIsNone(pane.payback_requests(64_971, 60_000))  # below the floor: compacting loses
        self.assertIsNone(pane.payback_requests(64_971, 64_971))
        self.assertEqual(pane.floor_of([]), (pane.FLOOR_GUESS, False))
        self.assertEqual(pane.floor_of([(None, None, None)]), (pane.FLOOR_GUESS, False))
        self.assertEqual(pane.summary_of([None, 19_000, 0, 24_000]), (21_500, True))
        self.assertEqual(pane.summary_of([None]), (pane.SUMMARY_GUESS, False))

    def test_an_even_number_of_sizes_takes_the_middle_pair(self):
        self.assertEqual(pane.floor_of(POSTS[:2])[0], 64_543)  # (64,971 + 64,115) / 2, rounded as JavaScript does

    def test_the_payback_reads_the_status_lines_floor_and_rounding(self):
        def js(context, posts, per_turn, summary=21_700):  # the status line's own arithmetic
            last = sorted(posts[-5:])
            floor = last[len(last) >> 1]
            saved = (context - floor) * 0.1
            requests = (floor * 2 + summary * 5 + context * 0.1) / saved if saved > 0 else 9999
            turns = requests / per_turn if per_turn else requests
            # the status line's math, said in words (10-12: "14r", "2t", no clue): your prompts, rounded up
            prompts = max(1, math.ceil(requests / per_turn)) if per_turn else None
            n = int(requests + 0.5)
            return turns, (f"♻ ~{prompts} {'prompt' if prompts == 1 else 'prompts'}" if prompts
                           else f"♻ {n} {'call' if n == 1 else 'calls'}")

        def row2(context, sizes, more):  # the payback's slot: row 1, after the context
            fields = dict(context_window=dict(total_input_tokens=context, context_window_size=1_000_000))
            g = pane.gauges(fields, NOW, dict(compact_sizes=sizes, **more), NOW)
            return plain(pane.cockpit(g, 49))[0].rsplit('  ', 1)[1].strip()  # after the sparkline, when there is one

        sizes = [(None, p, None) for p in REAL_POSTS]
        self.assertAlmostEqual(js(300_000, REAL_POSTS, 17)[0], 0.74, places=2)  # the old 4,500 summary said 8.76 turns
        self.assertEqual(row2(300_000, sizes, turns_of(17)), '♻ ~1 prompt')  # 12.6 calls, within this prompt
        self.assertEqual(row2(300_000, sizes, turns_of(4)), '♻ ~4 prompts')  # 3.15 prompts: a prompt under way counts whole
        self.assertEqual(row2(300_000, sizes, {}), '♻ 13 calls')  # no whole turns yet: in API calls
        self.assertEqual(row2(250_000, [], {}), '♻ ~14 calls')  # no history: the 65k guess, marked ~
        for per_turn in (1, 4, 17):
            for context in range(120_000, 600_001, 20_000):
                turns, shown = js(context, REAL_POSTS, per_turn)
                want = '' if context < pane.PAYBACK_FROM else shown if turns <= 15 else '♻ >15 prompts'
                got = row2(context, sizes, turns_of(per_turn))
                self.assertTrue(got.startswith(want) and ('♻' in got) == bool(want), (per_turn, context, turns, got))

    def test_a_reading_at_or_under_the_floor_never_pays_back_and_says_so(self):
        sizes = [(None, p, None) for p in REAL_POSTS]
        fields = dict(context_window=dict(total_input_tokens=60_000, context_window_size=1_000_000))
        self.assertEqual(pane.gauges(fields, NOW, dict(compact_sizes=sizes), NOW)['payback']['text'], '')
        self.assertEqual(pane.gauges({}, None, dict(compact_sizes=sizes), NOW)['payback']['text'], '♻ --')

    def test_no_payback_under_the_band_csr_offers_in(self):  # 10-12: ♻ 9t at 95k read as advice to compact
        sizes = [(None, p, None) for p in REAL_POSTS]
        text = lambda context: pane.gauges(dict(context_window=dict(total_input_tokens=context)), NOW,
                                           dict(compact_sizes=sizes, **turns_of(12)), NOW)['payback']['text']
        self.assertEqual((text(95_000), text(180_588)), ('', ''))
        self.assertTrue(text(pane.PAYBACK_FROM).startswith('♻'))
        fields = dict(context_window=dict(total_input_tokens=95_000, context_window_size=1_000_000))
        g = pane.gauges(fields, NOW, dict(compact_sizes=sizes, **turns_of(12)), NOW)
        self.assertNotIn('♻', plain(pane.cockpit(g, 49))[0])

    def test_the_payback_ink_follows_the_turns_to_repay(self):
        sizes = [(None, 65_000, None)]
        sev = lambda context: pane.gauges(dict(context_window=dict(total_input_tokens=context)), NOW,
                                          dict(compact_sizes=sizes, **turns_of(17)), NOW)['payback']['sev']
        # 0.7 and 1.1 turns: amber at a turn or less, advisory to three; never red, the context's own size says that;
        # and nothing under the band
        self.assertEqual((sev(300_000), sev(200_000), sev(150_000)), ('amber', 'advisory', 'normal'))

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
            self.assertEqual(session.summary_sizes, [19_000, 19_000, None, None])  # postTokens: the summary each wrote
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

    def test_the_context_row_reads_the_context_against_its_window_and_the_payback(self):  # A1
        fields = fields_of(87_724)
        rows = self.rows(fields, session(compact_sizes=POSTS))
        self.assertEqual(plain(rows)[0], ' CTX 88k/1M')  # no turns yet: no sparkline, never a zero; under the band, no ♻
        self.assertEqual(plain(self.rows(fields_of(260_000), session(compact_sizes=POSTS)))[0], ' CTX 260k/1M  ♻ 14 calls')  # in the band: no whole turns yet, in API calls
        self.assertEqual(plain(self.rows({}))[0], ' CTX --  ♻ --')  # no data: '--'
        self.assertIn(sgr('data') + '88k/1M' + pane.OFF, rows[0])
        self.assertIn(sgr('normal') + 'CTX ' + pane.OFF, rows[0])
        self.assertNotIn('▕', rows[0])  # the bar and its floor bug are the strip's now

    def test_the_context_lights_amber_at_100k_and_red_at_400k_and_its_tape_pegs(self):
        self.assertIn(sgr('amber') + '150k/1M', self.rows(fields_of(150_000))[0])
        red = self.rows(fields_of(432_000))[0]
        self.assertIn(sgr('red') + '432k/1M', red)  # past the full scale: the value exact
        self.assertEqual(''.join(c[0] for c in pane.ctx_tape(432_000, 900_000, 8)[1:]), '█' * 7)  # the tape pegged

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
        row3 = self.rows(fields_of(limits=limits_of(95, 75)), width=60)[1]
        reset = datetime.fromtimestamp(NOW + 3 * 86400).strftime('%a %H:%M')
        soon = datetime.fromtimestamp(NOW + 7200)  # the time alone only when that is today, else the weekday too
        soon = soon.strftime('%H:%M' if soon.date() == datetime.fromtimestamp(NOW).date() else '%a %H:%M')
        self.assertIn(sgr('amber') + f'7D 75%» ↻{reset}' + pane.OFF, row3)  # both ahead of pace: »
        self.assertIn(sgr('red') + '5H 95%» ↻' + soon + pane.OFF, row3)
        low = self.rows(fields_of(limits=limits_of(8, 12)))[1]
        self.assertIn(sgr('normal') + '5H 8%' + pane.OFF, low)
        self.assertNotIn('↻', low)

    def test_the_riskier_window_comes_first(self):  # A6
        first = lambda five, seven: plain(self.rows(fields_of(limits=limits_of(five, seven))))[1].split()[0]
        self.assertEqual(first(8, 75), '7D')
        self.assertEqual(first(92, 75), '5H')
        self.assertEqual(first(30, 30), '5H')  # a tie keeps the 5-hour window first

    def test_a_value_that_went_invalid_is_dashed_in_amber_and_one_never_seen_in_the_normal_ink(self):  # A7
        gone = limits_of(95, 12, five_at=NOW - 60)
        row3 = self.rows(fields_of(limits=gone))[1]
        self.assertIn(sgr('amber') + '5H --' + pane.OFF, row3)
        self.assertNotIn('95', row3)  # never a stale number
        self.assertTrue(plain([row3])[0].strip().startswith('7D 12%'))  # a '--' goes last
        none = self.rows({})[1]
        self.assertIn(sgr('normal') + '5H --' + pane.OFF, none)
        self.assertIn(sgr('normal') + '7D --' + pane.OFF, none)
        only = self.rows(fields_of(limits=dict(seven_day=dict(used_percentage=41, resets_at=NOW + 99))))[1]
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
        row3 = self.rows(fields_of(), limits=limits_of(33, 44))[1]
        self.assertTrue(plain([row3])[0].strip().startswith('7D 44%'))
        own = self.rows(fields_of(limits=limits_of(10, 11)), limits=limits_of(33, 44))[1]
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
            # ratification is off on purpose and never said (10-12: "dont care about ratification")
            self.assertEqual(got['inop'], [('AI_NARRATIVES', 'INHIBITED · CSR_NO_AI_NARRATIVES=true (hooks)', 'advisory')])
            self.assertTrue(got['daemon']['up'])
            for secret in ('PATH', 'SECRET', 'shh', 'CSR_STORY'):
                self.assertNotIn(secret, json.dumps(got))
            only_plist = pane.csr_status(NOW, agents, lock, os.path.join(tmp, 'none.json'))
            self.assertEqual((only_plist['inop'], only_plist['sev']), ([], None))  # its one switch is a quiet one
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

    def test_sts_lights_row_two_in_its_severity_ink(self):
        advisory = dict(sev='advisory', inop=[('RATIFICATION', 'x', 'advisory')])
        self.assertIn(sgr('advisory') + 'STS' + pane.OFF, self.rows({}, sts=advisory)[1])
        amber = dict(sev='amber', inop=[('DAEMON', 'x', 'amber')])
        self.assertIn(sgr('amber') + 'STS' + pane.OFF, self.rows({}, sts=amber)[1])
        self.assertNotIn('STS', plain(self.rows({}))[1])
        self.assertTrue(plain(self.rows({}, sts=advisory))[1].endswith('STS'))  # right-aligned

    def test_while_a_compaction_runs_its_timer_takes_the_paybacks_slot(self):  # A10
        usual = dict(low=50.0, median=75.0, high=100.0, n=9)
        info = dict(elapsed=72, trigger='manual', usual=usual, facts=['CSR keeps 12 chunks of this session'])
        rows = self.rows(fields_of(300_000, cache=dict(warm=True, expires_at=NOW + 3000)), compacting=info)
        self.assertEqual(len(rows), 2)
        self.assertEqual(plain(rows)[0], ' CTX 300k/1M  ♻ 1:12 █████▊░░ 0:50–1:40')
        self.assertEqual(plain(rows)[0].count('♻'), 1)  # the timer's icon, no payback beside it
        self.assertNotIn('COMPACTING', plain(rows)[0])
        self.assertTrue(plain(rows)[1].endswith('CACHE 50m'))  # the cache keeps row 2
        self.assertIn(sgr('advisory') + '♻ 1:12', rows[0])
        late = self.rows({}, compacting=dict(info, elapsed=130, trigger='auto'))[0]
        self.assertIn(sgr('amber') + '♻A 2:10', late)  # past the usual longest; an auto compaction is ♻A
        self.assertNotIn('AUTO', plain([late])[0])
        bare = plain(self.rows({}, compacting=dict(elapsed=5, trigger='manual', usual=None, facts=[])))[0]
        self.assertEqual(bare, ' CTX --  ♻ 0:05')
        row1 = lambda width: plain(pane.cockpit(pane.gauges(fields_of(), NOW, session(), NOW, compacting=info), width))[0]
        self.assertEqual(row1(34), ' CTX --  ♻ 1:12 █████▊░░ 0:50–1:40')
        self.assertEqual(row1(33), ' CTX --  ♻ 1:12 █████▊░░')  # the range goes
        self.assertEqual(row1(23), ' CTX --  ♻ 1:12')  # then the bar
        stack = pane.stack_rows(pane.gauges({}, None, session(), NOW, compacting=dict(info, elapsed=130, trigger='auto')))
        self.assertIn([('♻A 2:10', 'amber'), ('♻A2:10', 'amber')], stack)  # column mode says it the same way
        for width in range(10, 60):
            for row in plain(pane.cockpit(pane.gauges(fields_of(), NOW, session(), NOW, compacting=info), width)):
                self.assertLessEqual(len(row), width, width)

    def test_two_rows_at_every_width_and_sts_always_survives(self):  # A11
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
                self.assertEqual(len(rows), 2, width)
                for row in plain(rows):
                    self.assertLessEqual(len(row), width, (width, row))
                    self.assertTrue(row.startswith(' '))
                self.assertTrue(plain(rows)[1].endswith('STS'), (width, plain(rows)[1]))

    def test_the_logo_takes_the_ctx_word_cells_and_nothing_else_moves(self):
        fields = fields_of(87_724, size=1_000_000, limits=limits_of(95, 75))
        main = [(100.0 + 100 * k + 5, 60_000 + 12_000 * k) for k in range(1, 5)]
        s = session(main=main, prompts=[100.0 + 100 * k for k in range(1, 5)])
        compacting = dict(elapsed=72, trigger='manual', usual=dict(low=50.0, median=75.0, high=100.0, n=8), facts=[])
        for g in (pane.gauges(fields, NOW, s, NOW), pane.gauges({}, None, session(), NOW),
                  pane.gauges(fields, NOW, s, NOW, compacting=compacting)):
            for width in (60, 46, 29, 25, 18, 12):
                with self.subTest(width=width):
                    word, mark = pane.cockpit(g, width), pane.cockpit(g, width, logo=True)
                    self.assertEqual(plain(word)[0][:5], ' CTX ')
                    self.assertEqual(plain(mark)[0], ' ' + ' ' * pane.LOGO[0] + plain(word)[0][5:])  # the same row, the word blank
                    self.assertEqual(mark[1], word[1])  # row 2 untouched

    def test_rows_that_cannot_hold_everything_shrink_in_a_fixed_order(self):
        fields = fields_of(87_724, size=1_000_000, limits=limits_of(95, 75))
        main = [(100.0 + 100 * k + 5, 60_000 + 12_000 * k) for k in range(1, 5)]
        s = session(main=main, prompts=[100.0 + 100 * k for k in range(1, 5)])
        row1 = lambda width: plain(pane.cockpit(pane.gauges(fields, NOW, s, NOW), width))[0]
        sparks = lambda row: sum(row.count(c) for c in pane.EIGHTHS[1:])
        # four turns at 72k to 108k, an eighth a 50k over 0..CTX_RED, newest at the right
        # 88k is under PAYBACK_FROM: no ♻ on the row
        self.assertEqual(row1(60), ' CTX ' + ' ' * 20 + '▁▂▂▂ 88k/1M ▲12k/t')
        self.assertEqual(row1(21), ' CTX ' + ' ' * 5 + '▁▂▂▂ 88k/1M')  # the trend goes first
        self.assertEqual(sparks(row1(21)), 4)
        self.assertEqual(row1(17), ' CTX 88k/1M')  # then the sparkline, under SPARK_MIN cells
        big = pane.gauges(fields_of(1_200_000, size=10_000_000), None, session(), NOW)
        self.assertEqual(plain(pane.cockpit(big, 40))[0], ' CTX 1.2M/10M  ♻ ~3 calls')  # no whole turns: in API calls
        self.assertEqual(plain(pane.cockpit(big, 21))[0], ' CTX 1.2M  ♻ ~3 calls')  # then the window
        self.assertEqual(plain(pane.cockpit(big, 18))[0], ' CTX 1.2M')  # no room for words: none, never a code
        g = pane.gauges(fields, NOW, s, NOW, sts=dict(sev='amber', inop=[]))
        row2 = lambda width: plain(pane.cockpit(g, width))[1]
        first, second = (pane.paced(q) for q in g['quotas'])
        self.assertTrue(first.startswith('5H 95%» ↻') and second.startswith('7D 75%» ↻'))
        full = 1 + len(first) + 2 + len(second) + 2 + len('CACHE --') + len(' STS')
        self.assertEqual(row2(full), f' {first}  {second}  CACHE -- STS')
        self.assertEqual(row2(full - 1), f' {first}  {second}  ◷--' + ' ' * 5 + 'STS')  # the cache's word goes first
        glyph = 1 + len(first) + 2 + len('◷--') + len(' STS')
        self.assertEqual(row2(glyph), f' {first}  ◷-- STS')  # then the second window
        self.assertEqual(row2(glyph - 1), ' 5H 95%»  ◷--' + ' ' * (glyph - 1 - 16) + 'STS')  # then the first's reset time
        self.assertEqual(row2(16), ' 5H 95%»' + ' ' * 5 + 'STS')  # then the cache
        self.assertEqual(row2(11), ' 95%»' + ' ' * 3 + 'STS')  # then the window's code, the » last
        self.assertTrue(all(row2(width).endswith('STS') for width in range(12, 70)))  # STS never goes

    def test_a_window_ahead_of_pace_carries_a_mark_after_its_percentage(self):
        # 5H resets in 2h of 5h (pace 0.6), 7D in 3 days of 7 (pace 0.57): ahead past pace + AHEAD
        at = lambda five, seven, **more: plain(self.rows(fields_of(limits=limits_of(five, seven, **more)), width=60))[1]
        self.assertEqual(pane.AHEAD, 0.05)
        self.assertIn(' 5H 66%»  7D 50%  ', at(66, 50))  # 0.66 > 0.65: ahead
        self.assertNotIn('»', at(64, 50))  # 0.64: within five points of pace, nothing
        self.assertNotIn('»', at(8, 12))  # well under pace
        self.assertIn(' 7D 63%»  5H 8%  ', at(8, 63))  # 0.63 > 0.62; the riskier window comes first
        self.assertNotIn('7D 61%»', at(8, 61))
        self.assertNotIn('»', at(95, 75, five_at=None, seven_at=None))  # no reset, no pace: nothing
        self.assertIn(' 5H 95% ↻', at(95, 75, five_at=NOW + 8 * 3600))  # a reset further off than the window: no pace
        self.assertNotIn('▲', at(95, 75))  # row 1's trend owns ▲ and ▼
        self.assertNotIn('▼', at(95, 75))
        row = self.rows(fields_of(limits=limits_of(95, 30)), width=60)[1]
        self.assertIn(sgr('red') + '5H 95%» ↻', row)  # the » in its window's own ink
        self.assertIn(sgr('normal') + '7D 30%' + pane.OFF, row)

    def test_the_pace_mark_outlasts_every_other_piece_of_row_two(self):
        g = pane.gauges(fields_of(limits=limits_of(95, 75)), NOW, session(), NOW, sts=dict(sev='amber', inop=[]))
        plain_g = pane.gauges(fields_of(limits=limits_of(95, 75)), NOW, session(), NOW)
        for gauge in (g, plain_g):
            for width in range(9, 70):
                with self.subTest(width=width, sts=gauge is g):
                    row = plain(pane.cockpit(gauge, width))[1]
                    self.assertLessEqual(len(row), width)
                    self.assertIn('95%»', row)  # the first window's percentage and its » never go
        self.assertEqual(plain(pane.cockpit(g, 11))[1], ' 95%»   STS')  # the code goes first

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
        self.assertEqual(text[:2], plain(pane.cockpit(page['head']['gauges'], 50)))
        for line in text[:2]:
            self.assertNotRegex(line, r'prompts|Opus|opus|\d+h\d\dm|compacts')
        self.assertNotEqual(text[2].strip(), '')  # no lip, no strip: the body starts under the two rows
        self.assertIsNone(lines.lanes)
        self.assertFalse(any('learned' in line or 'handed' in line for line in text[:8]))
        tall = pane.page_lines(page, 50, 40, {})
        self.assertNotEqual(plain(tall)[2].strip(), '')  # a tall pane spends no row on it either


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
            self.assertNotIn('♻', plain(pane.cockpit(g, 49))[0])  # its context is under the band
            self.assertTrue(plain(pane.cockpit(g, 49))[1].endswith('  CACHE 50m'))


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
                self.assertEqual(len(loaded.cockpit(g, 49)), 2)
                self.assertEqual(len(loaded.ctx_tape(150_000, 65_000, 8)), 8)  # the tape divides by CTX_RED
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


class GaugeStrip(unittest.TestCase):
    """The gauge strip down the pane's right edge: CTX, 7D, 5H and the cache, a column a tape."""

    def glyphs(self, cells):
        return ''.join(cell[0] for cell in cells)

    def test_the_context_tape_fills_bottom_up_in_eighths_over_the_full_scale(self):
        self.assertEqual(self.glyphs(pane.ctx_tape(pane.CTX_RED // 2, None, 10)), '│││' + '│─█████')  # 40 of 80 eighths
        self.assertEqual(self.glyphs(pane.ctx_tape(135_000, None, 10)), '││││─│▃███')  # 27 eighths
        self.assertEqual(self.glyphs(pane.ctx_tape(10 * pane.CTX_RED, None, 4)), '████')  # pegged, never more
        self.assertEqual(self.glyphs(pane.ctx_tape(1, None, 4)), '│──│')  # under half an eighth: the rail, two ticks

    def test_each_cell_takes_the_heat_of_its_own_height(self):
        cells = pane.ctx_tape(pane.CTX_RED, None, 8)
        for row, cell in enumerate(cells):
            self.assertEqual(cell[1], pane.heat((8 - 1 - row + 0.5) / 8 * pane.CTX_RED))
        near = lambda a, b: all(abs(x - y) <= 2 for x, y in zip(a, b))
        self.assertTrue(near(pane.heat(0), pane.PIGMENT['indigo_lit']))  # blue at the bottom
        self.assertTrue(near(pane.heat(pane.CTX_AMBER), pane.TEAL))
        self.assertTrue(near(pane.heat(pane.CTX_HOT), pane.PIGMENT['ochre_lit']))
        self.assertTrue(near(pane.heat(pane.CTX_RED), pane.PIGMENT['vermilion_lit']))  # red at the top
        self.assertEqual(pane.heat(10 * pane.CTX_RED), pane.heat(pane.CTX_RED))
        for odd in (None, float('nan'), 'x', True):
            self.assertEqual(pane.heat(odd), pane.heat(0))

    def test_the_floor_bug_and_the_ticks_sit_in_the_tape_itself(self):
        cells = pane.ctx_tape(150_000, 73_120, 8)  # 50k a row, top first: filled rows 5 to 7
        self.assertEqual(cells[6], ('━', pane.BUG, pane.heat(1.5 / 8 * pane.CTX_RED)))  # on the fill, in its heat
        self.assertEqual(cells[3], ('─', pane.SEV['red'], None))  # CTX_HOT, above the fill
        self.assertEqual(cells[5][0], '█')  # CTX_AMBER's row is filled: the colour says it
        self.assertEqual(pane.ctx_tape(20_000, 73_120, 8)[6], ('━', pane.BUG, None))  # under the floor: on the ground
        self.assertEqual(pane.ctx_tape(150_000, 10 ** 9, 8)[0][0], '━')  # past the scale: the top row
        for floor in (float('nan'), float('inf'), None, 'x'):
            self.assertEqual(len(pane.ctx_tape(150_000, floor, 4)), 4)  # never raises

    def test_unknown_tokens_leave_the_tape_unfilled(self):
        for odd in (None, 0, -5, float('nan'), 'lots', True):
            cells = pane.ctx_tape(odd, 73_120, 8)
            self.assertFalse(any(cell[0] in pane.EIGHTHS[1:] for cell in cells), odd)  # never a stale fill
            self.assertEqual(cells[6][0], '━', odd)

    def test_a_window_fills_to_its_use_with_the_pace_bug_at_the_share_gone(self):
        q = dict(code='7D', used=50, pace=0.75, sev='normal')
        cells = pane.quota_tape(q, 8)
        self.assertEqual(self.glyphs(cells), '│━││████')
        self.assertEqual(cells[1], ('━', pane.BUG, None))  # ahead of the use: on the ground
        self.assertEqual(cells[4][1], pane.QUOTA_INK['normal'])
        behind = pane.quota_tape(dict(q, pace=0.25), 8)
        self.assertEqual(behind[5], ('━', pane.BUG, pane.QUOTA_INK['normal']))  # inside the fill: on it
        self.assertEqual(pane.quota_tape(dict(q, sev='amber', used=83), 8)[7][1], pane.PIGMENT['ochre_lit'])
        self.assertEqual(self.glyphs(pane.quota_tape(dict(q, pace=None), 4)), '││██')  # no reset known: no bug
        for gone in (None, dict(code='5H', used=None, pace=None, sev='amber', stale=True)):
            self.assertEqual(self.glyphs(pane.quota_tape(gone, 4)), '││││')  # never reported, or rolled over

    def test_the_cache_drains_downward_and_turns_amber_under_ten_minutes(self):
        warm = lambda left: pane.cache_tape(dict(left=left, ttl=3600, cold=False), 8)
        self.assertEqual(self.glyphs(warm(1800)), '││││████')  # half its life left
        self.assertEqual(self.glyphs(warm(900)), '││││││██')  # draining
        self.assertEqual(warm(1800)[7][1], pane.WARM)
        self.assertEqual(warm(300)[7][1], pane.PIGMENT['ochre_lit'])  # under CACHE_LOW
        self.assertEqual(self.glyphs(pane.cache_tape(dict(left=None, ttl=None, cold=True), 4)), '││││')  # cold
        self.assertEqual(self.glyphs(pane.cache_tape(dict(left=1800, ttl=None, cold=False), 4)), '││││')  # no clock
        self.assertEqual(self.glyphs(pane.cache_tape(None, 4)), '││││')

    def test_a_narrow_pane_drops_tapes_from_the_right(self):
        self.assertEqual([pane.strip_tapes(cols, 20) for cols in (122, 46, 30, 27, 24)], [0] * 5)  # none beside a page
        self.assertEqual(pane.strip_tapes(46, 5), 0)  # too short for a tape
        self.assertEqual([pane.strip_tapes(cols, 20, column=True) for cols in (23, 12, 10, 9, 7, 6)], [4, 4, 4, 3, 1, 0])
        g = pane.gauges(fields_of(150_000), None, session(), NOW)
        self.assertEqual([kind for kind, _ in pane.gauge_strip(g, 2, 6)], ['ctx', '7d'])  # CACHE first, then 5H
        self.assertEqual([kind for kind, _ in pane.gauge_strip(g, 4, 6)], list(pane.STRIP))
        self.assertEqual((pane.strip_width(0), pane.strip_width(4)), (0, 5))

    def test_the_strip_writes_each_row_by_position_on_its_ground(self):
        g = pane.gauges(fields_of(150_000, limits=limits_of(10, 50)), None, session(), NOW)
        columns = pane.gauge_strip(g, 4, 5)
        steps = pane.dark_steps(2)
        strips = pane.strip_rows(columns, 12, steps)
        self.assertEqual(len(strips), 6)  # the headers, then five cells
        for row, strip in enumerate(strips):
            self.assertTrue(strip.startswith(f'\x1b[{row + 1};8H'), strip)  # the margin, then four tapes
            self.assertEqual(len(PLAIN.sub('', strip.split('H', 1)[1])), 5)
        self.assertEqual(PLAIN.sub('', strips[0].split('H', 1)[1]), ' C75◷')
        self.assertIn('\x1b[48;2;%d;%d;%dm' % steps[0], strips[0])  # beside the masthead: its row's ground
        self.assertIn('\x1b[48;2;%d;%d;%dm' % steps[1], strips[1])
        self.assertIn('\x1b[48;2;%d;%d;%dm' % pane.DARK[1], strips[4])  # below it: the masthead's last step
        self.assertEqual(pane.strip_rows([], 46), [])

    def test_the_masthead_steps_down_from_payne_to_night(self):
        page = stand_in(fields_of(90_000), session())
        grounds = pane.dark_steps(2)
        self.assertEqual((grounds[0], grounds[-1]), (pane.PIGMENT['payne'], pane.PIGMENT['night']))
        lines = pane.page_lines(page, 50, 24, {})
        for line, ground in zip(lines[:2], grounds):
            self.assertTrue(line.startswith('\x1b[48;2;%d;%d;%dm' % ground), repr(line[:40]))
        self.assertEqual(lines.grounds, grounds)  # what the strip beside them takes
        phone = pane.page_lines(page, 40, 24, dict(phone=True))
        for line, ground in zip(phone[:2], grounds):  # a phone's masthead: the same two steps
            self.assertTrue(line.startswith('\x1b[48;2;%d;%d;%dm' % ground))


class MicroCharts(unittest.TestCase):
    """The masthead's sparkline and the readings behind the tapes."""

    def test_the_sparkline_is_the_last_turns_in_eighths_newest_at_the_right(self):
        cells = plain([pane.spark([50_000, 200_000, 400_000, 900_000], 6)])[0]
        self.assertEqual(cells, '  ▁▄██')
        self.assertEqual(plain([pane.spark([0, None, float('nan'), 'x', 100_000], 3)])[0], '  ▂')  # never a zero
        self.assertIn(pane.fg_of(pane.heat(400_000)) + '█', pane.spark([400_000], 1))  # each in its heat
        self.assertEqual(pane.spark([1, 2, 3], 0), '')

    def test_the_turns_are_each_turns_last_context(self):
        main = [(105.0, 10), (106.0, 20), (205.0, 30), (405.0, 40)]
        self.assertEqual(pane.turn_ends(main, [100.0, 200.0, 300.0, 400.0]), [(100.0, 20), (200.0, 30), (400.0, 40)])
        self.assertEqual(pane.turn_ends([], [1.0]), [])
        g = pane.gauges(fields_of(), None, session(main=main, prompts=[100.0, 200.0, 300.0, 400.0]), NOW)
        self.assertEqual(g['turns'], [20, 30, 40])

    def test_the_pace_is_the_share_of_the_window_gone(self):
        self.assertAlmostEqual(pane.pace_of('5H', NOW + 3600, NOW), 0.8)
        self.assertAlmostEqual(pane.pace_of('7D', NOW + 7 * 86400 / 4, NOW), 0.75)
        for code, resets in (('5H', NOW - 1), ('5H', None), ('5H', NOW + 6 * 3600), ('XX', NOW + 60), ('5H', float('nan'))):
            self.assertIsNone(pane.pace_of(code, resets, NOW), (code, resets))
        self.assertAlmostEqual(pane.quota_slot('5H', dict(used_percentage=10, resets_at=NOW + 3600), NOW)['pace'], 0.8)

    def test_the_cache_keeps_its_clock_for_the_tape(self):
        got = pane.cache_info(dict(warm=True, ttl='1h', expires_at=NOW + 1800), NOW)
        self.assertEqual((got['left'], got['ttl'], got['cold']), (1800, 3600, False))
        self.assertIsNone(pane.cache_info(dict(warm=True, expires_at=NOW + 1800), NOW)['ttl'])  # no ttl said
        self.assertTrue(pane.cache_info(dict(warm=False), NOW)['cold'])
        self.assertEqual([pane.ttl_seconds(t) for t in ('1h', '5m', '90s', ' 2h ', '1d', 'x', None, '0m', '2d')],
                         [3600, 300, 90, 7200, 86400, None, None, None, None])
        stretched = pane.cache_info(dict(warm=True, ttl='5m', expires_at=NOW + 900), NOW)
        self.assertEqual(stretched['ttl'], 900)  # a clock past its ttl: the tape never overflows

    def test_column_mode_stacks_the_values_in_their_inks(self):
        g = pane.gauges(fields_of(177_000, cache=dict(warm=True, ttl='1h', expires_at=NOW + 3540), limits=limits_of(7, 82)),
                        None, session(compact_sizes=POSTS), NOW, sts=dict(sev='advisory', inop=[]))
        rows = pane.stack_rows(g)
        self.assertEqual([forms[0][0] for forms in rows], ['CTX 177k', '7D 82%', '5H 7%', '◷59m', 'STS'])  # under the band: no ♻
        self.assertEqual([forms[-1][0] for forms in rows], ['177k', '82%', '7%', '◷59m', 'STS'])
        self.assertEqual(rows[0][0][1], 'amber')
        self.assertEqual(rows[1][0][1], 'amber')
        g = pane.gauges(fields_of(260_000), None, session(compact_sizes=POSTS), NOW)
        self.assertEqual([forms[0][0] for forms in pane.stack_rows(g)][:2], ['CTX 260k', '♻ 14 calls'])
        self.assertEqual(rows[-1][0][1], 'advisory')


class PhoneCockpit(unittest.TestCase):
    """On a phone the masthead is the desktop's two rows; the strip carries the bars."""

    def test_a_phone_draws_the_same_two_rows(self):
        fields = fields_of(155_000, cache=dict(warm=True, expires_at=NOW + 3000), limits=limits_of(6, 81))
        g = pane.gauges(fields, None, session(compact_sizes=POSTS), NOW)
        for width in range(10, 70):
            self.assertEqual(pane.cockpit(g, width, phone=True), pane.cockpit(g, width))
            for line in plain(pane.cockpit(g, width, phone=True)):
                self.assertLessEqual(len(line), width, (width, line))

    def test_the_phone_legend_starts_with_four_short_keys(self):
        text, spans = pane.legend_parts('main', 39, phone=True)
        self.assertEqual([key for *_, key in spans], ['l', 's', 'i', 'q'])
        shown = plain([text])[0]
        self.assertLessEqual(len(shown), 39)
        for start, end, key in spans:
            self.assertTrue(shown[start:end].startswith(key + ' '), (shown, start, key))
        self.assertIn('f', [key for *_, key in pane.legend_parts('main', 80)[1]])  # a desktop keeps f
