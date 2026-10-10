"""The masthead's clocks: cache, plan quotas and compaction payback, and the wrapper that keeps their fields.
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


class CacheClock(unittest.TestCase):
    def test_a_warm_cache_says_how_long_it_stays_warm(self):
        got = pane.cache_clock(dict(warm=True, ttl='1h', expires_at=NOW + 41 * 60 + 20), NOW)
        self.assertEqual(got, ('cache warm · 41m left', None))

    def test_a_cache_in_its_last_five_minutes_is_inked_as_a_warning(self):
        self.assertEqual(pane.cache_clock(dict(warm=True, expires_at=NOW + 150), NOW), ('cache warm · 2m left', 'dream'))
        self.assertEqual(pane.cache_clock(dict(warm=True, expires_at=NOW + 20), NOW)[0], 'cache warm · under 1m left')

    def test_a_cold_cache_says_what_the_next_turn_re_reads(self):
        cold = dict(warm=False, ttl='1h', expires_at=NOW - 10, recache_tokens_if_cold=152_300)
        self.assertEqual(pane.cache_clock(cold, NOW), ('cache cold · re-reads ~152k', 'turn'))
        self.assertEqual(pane.cache_clock(dict(warm=False), NOW), ('cache cold', 'turn'))

    def test_an_expiry_that_has_passed_beats_a_warm_flag_the_status_line_wrote_earlier(self):
        got = pane.cache_clock(dict(warm=True, expires_at=NOW - 5, recache_tokens_if_cold=90_000), NOW)
        self.assertEqual(got[0], 'cache cold · re-reads ~90k')

    def test_a_warm_cache_without_an_expiry_shows_its_ttl_only(self):
        self.assertEqual(pane.cache_clock(dict(warm=True, ttl='5m'), NOW), ('cache warm · 5m', None))

    def test_missing_or_odd_fields_show_nothing_and_never_raise(self):
        for odd in (None, {}, [], 'warm', dict(ttl='1h'), dict(warm=None), dict(warm=True, expires_at=[1])):
            self.assertIn(pane.cache_clock(odd, NOW), (None, ('cache warm', None)), odd)
        self.assertIsNone(pane.cache_clock({}, NOW))

    def test_numbers_no_clock_can_show_never_raise(self):
        nan, inf = float('nan'), float('inf')
        self.assertEqual(pane.cache_clock(dict(warm=True, expires_at=nan), NOW), ('cache warm', None))
        self.assertEqual(pane.cache_clock(dict(warm=True, expires_at=1e20), NOW), ('cache warm', None))
        self.assertEqual(pane.cache_clock(dict(warm=True, expires_at=NOW + 9 * 86400), NOW), ('cache warm', None))
        for cost in (nan, inf, -5, 0):
            self.assertEqual(pane.cache_clock(dict(warm=False, recache_tokens_if_cold=cost), NOW), ('cache cold', 'turn'))
        for used, at in ((nan, NOW + 60), (inf, NOW + 60), (95, 1e20), (95, inf), (95, -5)):
            got = pane.quota_pieces(dict(five_hour=dict(used_percentage=used, resets_at=at)), NOW)
            self.assertIn(got, ([], [('5h 95%', 'turn')]), (used, at))


class Quotas(unittest.TestCase):
    def limits(self, five, seven, five_at=NOW + 7200, seven_at=NOW + 3 * 86400):
        return dict(five_hour=dict(used_percentage=five, resets_at=five_at),
                    seven_day=dict(used_percentage=seven, resets_at=seven_at))

    def test_two_windows_in_percent(self):
        self.assertEqual(pane.quota_pieces(self.limits(34.2, 12), NOW), [('5h 34%', None), ('7d 12%', None)])

    def test_above_seventy_percent_the_reset_time_shows(self):
        got = pane.quota_pieces(self.limits(82, 12), NOW)
        reset = datetime.fromtimestamp(NOW + 7200).strftime('%H:%M')
        self.assertEqual(got[0], (f'5h 82% resets {reset}', 'dream'))
        self.assertEqual(got[1], ('7d 12%', None))
        week = pane.quota_pieces(self.limits(10, 91), NOW)[1]
        self.assertEqual((week[1], week[0].startswith('7d 91% resets ')), ('turn', True))
        self.assertRegex(week[0], r'resets \w{3} \d\d:\d\d$')  # a day or more away names the weekday

    def test_exactly_seventy_percent_shows_no_reset(self):
        self.assertEqual(pane.quota_pieces(self.limits(70, 70), NOW), [('5h 70%', None), ('7d 70%', None)])

    def test_a_window_that_has_rolled_over_since_is_left_out(self):
        got = pane.quota_pieces(self.limits(95, 12, five_at=NOW - 60), NOW)
        self.assertEqual([text for text, _ in got], ['7d 12%'])

    def test_one_window_alone_and_absent_fields(self):
        only = dict(seven_day=dict(used_percentage=41))
        self.assertEqual(pane.quota_pieces(only, NOW), [('7d 41%', None)])
        for odd in (None, {}, 'x', dict(five_hour=None), dict(five_hour=dict(resets_at=1)),
                    dict(five_hour=dict(used_percentage='lots'))):
            self.assertEqual(pane.quota_pieces(odd, NOW), [], odd)

    def test_resets_at_may_be_a_date_string(self):
        got = pane.quota_pieces(dict(five_hour=dict(used_percentage=80, resets_at='2027-01-15T08:00:00Z')),
                                pane.epoch('2027-01-15T06:00:00Z'))
        self.assertTrue(got[0][0].startswith('5h 80% resets '))


class MastheadRows(unittest.TestCase):
    FIELDS = dict(prompt_cache=dict(warm=True, ttl='1h', expires_at=NOW + 41 * 60 + 5),
                  rate_limits=dict(five_hour=dict(used_percentage=34, resets_at=NOW + 3600),
                                   seven_day=dict(used_percentage=12, resets_at=NOW + 86400 * 3)))

    def test_the_rows_in_order_and_nothing_when_fields_are_missing(self):
        rows = pane.masthead(self.FIELDS, NOW)
        self.assertEqual([[t for t, _ in row] for row in rows], [['cache warm · 41m left'], ['5h 34%', '7d 12%']])
        self.assertEqual(pane.masthead({}, NOW), [])
        self.assertEqual(pane.masthead(None, NOW), [])
        self.assertEqual(pane.masthead(dict(rate_limits='x', prompt_cache=3), NOW), [])
        only_limits = pane.masthead(dict(rate_limits=self.FIELDS['rate_limits']), NOW)
        self.assertEqual(len(only_limits), 1)

    def test_rows_fit_the_pane_dropping_pieces_right_to_left(self):
        rows = pane.masthead(self.FIELDS, NOW)
        self.assertEqual(plain(pane.mast_rows(rows, 45)), [' cache warm · 41m left', ' 5h 34% · 7d 12%'])
        self.assertEqual(plain(pane.mast_rows(rows, 8)), [' cache w…', ' 5h 34%'])
        high = pane.masthead(dict(rate_limits=dict(five_hour=dict(used_percentage=90, resets_at=NOW + 600),
                                                   seven_day=dict(used_percentage=20))), NOW)
        wide, narrow = plain(pane.mast_rows(high, 40))[0], plain(pane.mast_rows(high, 12))[0]
        self.assertTrue(wide.startswith(' 5h 90% resets ') and wide.endswith('· 7d 20%'))
        self.assertEqual(narrow, ' 5h 90% rese…')
        for width in (24, 30, 40, 46):
            for line in plain(pane.mast_rows(rows + high, width)):
                self.assertLessEqual(len(line), width + 1)

    def test_no_rows_draw_nothing(self):
        self.assertEqual(pane.mast_rows([], 40), [])
        self.assertEqual(pane.mast_rows(None, 40), [])

    def test_facts_drop_their_rightmost_pieces_to_fit(self):
        facts = '12 prompts · 3h20m · compact repays in ~6t · 13 compacts'
        self.assertEqual(pane.fit_pieces(facts, 60), facts)
        self.assertEqual(pane.fit_pieces(facts, 42), '12 prompts · 3h20m · compact repays in ~6t')
        self.assertEqual(pane.fit_pieces(facts, 20), '12 prompts · 3h20m')
        self.assertEqual(pane.fit_pieces(facts, 6), '12 pr…')


POSTS = [(159_635, 16_807, 70_542), (266_661, 18_603, 78_206), (402_895, 24_947, 102_046)]


class Payback(unittest.TestCase):
    def test_turns_to_break_even_from_this_sessions_own_median_post_size(self):
        turns, floor, seconds = pane.compaction_payback(POSTS, 200_000)
        self.assertEqual((turns, floor, seconds), (4, 18_603, 78.206))
        # (2 * 18603 + 5 * 4500 + 0.1 * 200000) / (0.1 * (200000 - 18603)) = 4.39
        self.assertAlmostEqual((2 * 18_603 + 5 * 4500 + 20_000) / (0.1 * (200_000 - 18_603)), 4.39, places=2)

    def test_a_bigger_context_pays_back_sooner(self):
        self.assertLess(pane.compaction_payback(POSTS, 300_000)[0], pane.compaction_payback(POSTS, 120_000)[0])

    def test_nothing_when_there_is_no_history_or_it_would_not_pay_back(self):
        self.assertIsNone(pane.compaction_payback([], 200_000))
        self.assertIsNone(pane.compaction_payback([(None, None, None)], 200_000))
        self.assertIsNone(pane.compaction_payback(POSTS, 0))
        self.assertIsNone(pane.compaction_payback(POSTS, 18_000))  # below the floor: compacting loses
        self.assertIsNone(pane.compaction_payback(POSTS, 60_000))  # 16 turns to repay: too slow to say
        self.assertIsNotNone(pane.compaction_payback(POSTS, 90_000))

    def test_an_even_number_of_sizes_takes_the_middle_pair(self):
        self.assertEqual(pane.compaction_payback(POSTS[:2], 200_000)[1], (16_807 + 18_603) / 2)

    def test_the_phrase_reads_the_context_since_the_last_compaction(self):
        replies = [(100.0, 380_000, 5, 0), (400.0, 150_000, 5, 0), (500.0, 200_000, 5, 0)]
        s = dict(compact_sizes=POSTS, compacts=[300.0], replies=replies)
        self.assertEqual(pane.payback_phrase(s), 'compact repays in ~4t')  # the 200k reply, not the 380k before the cut
        self.assertEqual(pane.payback_phrase(dict(s, replies=replies[:1])), '')  # nothing since the cut
        self.assertEqual(pane.payback_phrase(dict(s, compact_sizes=[])), '')
        self.assertEqual(pane.payback_phrase(dict(id='x')), '')

    def test_a_transcript_gives_its_compaction_rows(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, 's.jsonl')
            with open(path, 'w') as handle:
                for pre, post, ms in POSTS[:2]:
                    handle.write(json.dumps(dict(type='system', subtype='compact_boundary',
                                                 timestamp='2026-10-10T10:00:00Z',
                                                 compactMetadata=dict(trigger='manual', preTokens=pre, postTokens=post,
                                                                      durationMs=ms))) + '\n')
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


def stand_in(fields, s=None):
    """page_main over a stand-in CSR with the status line's fields as given."""
    con = sqlite3.connect(':memory:')
    con.execute('create table intent_events (ts text, quote text, session_id text, transcript_path text, '
                'byte_start int, project text, kind text)')
    connect, pane.connect = pane.connect, lambda: con
    try:
        s = s or dict(id='sess-now', prompts=[NOW - 100] * 12, replies=[], compacts=[], remembered=[],
                      gave=dict(recalls=[], carried=[], dreams=[]), project='csr-engine')
        data = dict(repo='/tmp/csr-engine', dreams=[], chapter=dict(at=1000.0, of=5), dreams_on=True, look=None,
                    story=None, claude_fields=fields, claude_line='Opus 5 │ 57k')
        return pane.page_main(data, s, NOW)
    finally:
        pane.connect = connect


class OnThePage(unittest.TestCase):
    def test_the_clock_rows_sit_in_the_masthead_under_the_facts(self):
        lines = plain(pane.page_lines(stand_in(MastheadRows.FIELDS), 45, 30, {}))
        facts = next(i for i, line in enumerate(lines) if '12 prompts' in line)
        self.assertEqual(lines[facts + 1].strip(), 'cache warm · 41m left')
        self.assertEqual(lines[facts + 2].strip(), '5h 34% · 7d 12%')

    def test_without_fields_the_page_is_the_page_it_was(self):
        with_none = plain(pane.page_lines(stand_in(None), 45, 30, {}))
        without = plain(pane.page_lines(stand_in({}), 45, 30, {}))
        self.assertEqual(with_none, without)
        self.assertFalse(any('cache' in line for line in with_none))

    def test_the_payback_phrase_joins_the_facts_row_only_with_compaction_history(self):
        replies = [(NOW - 50, 200_000, 5, 0)]
        busy = dict(id='sess-now', prompts=[NOW - 100] * 12, replies=replies, compacts=[NOW - 900],
                    compact_sizes=POSTS, remembered=[], gave=dict(recalls=[], carried=[], dreams=[]), project='p')
        fact = stand_in({}, busy)['head']['facts']
        self.assertIn('compact repays in ~4t', fact)
        self.assertTrue(fact.endswith('1 compacts'))
        calm = dict(busy, compacts=[], compact_sizes=[])
        self.assertNotIn('repays', stand_in({}, calm)['head']['facts'])


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

    def run_wrapper(self, home, status, pane_open=True):
        keep = os.path.join(home, '.claude-self-reflect', 'csr-companion')
        if pane_open:
            os.makedirs(os.path.join(keep, 'panes'))
            with open(os.path.join(keep, 'panes', 'sess-w.json'), 'w') as handle:
                json.dump(dict(pid=os.getpid()), handle)
        done = subprocess.run(['node', self.WRAPPER, '--sh', 'echo drawn'], input=json.dumps(status), text=True,
                              capture_output=True, env=dict(os.environ, HOME=home), timeout=30)
        return done, os.path.join(keep, 'statusline')

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
            self.assertEqual(pane.masthead(kept['fields'], NOW), [])

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
            rows = pane.masthead(fields, 1_800_000_000.0)
            self.assertEqual([[t for t, _ in row] for row in rows][0], ['cache warm · 50m left'])


if __name__ == '__main__':
    unittest.main()
