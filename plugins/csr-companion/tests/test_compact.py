"""Compact at the right moment, the pane's side: the payback the mod asks for, the plan row it draws, what CSR's
compactions did, and the dream log taking kind "compact". python3 -I -m unittest discover -s tests"""
import contextlib
import importlib.util
import io
import json
import os
import re
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('pane', os.path.join(HERE, '..', 'bin', 'pane.py'))
pane = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pane)

from test_pane import flat, page, plain  # noqa: E402  the pane tests' own stand-in page

COMPACT_TS = os.path.join(HERE, '..', 'hooks', 'compact.ts')
PROVENANCE = os.path.join(HERE, '..', '..', '..', 'csr-engine', 'src', 'extraction', 'provenance.rs')
SID = 'sess-compact'


def stamp(seconds):
    return pane.datetime.fromtimestamp(seconds, pane.timezone.utc).isoformat().replace('+00:00', 'Z')


def reply(at, context, mid):
    return dict(type='assistant', timestamp=stamp(at), message=dict(
        id=mid, model='claude-opus-5-5', stop_reason='end_turn', content=[dict(type='text', text='ok')],
        usage=dict(input_tokens=10, cache_read_input_tokens=context - 10, cache_creation_input_tokens=0, output_tokens=5)))


def prompt(at, text='go on'):
    return dict(type='user', timestamp=stamp(at), message=dict(content=text))


def transcript(folder, rows):
    os.makedirs(os.path.join(folder, '.claude', 'projects', 'proj'), exist_ok=True)
    path = os.path.join(folder, '.claude', 'projects', 'proj', f'{SID}.jsonl')
    with open(path, 'w', encoding='utf-8') as handle:
        handle.write(''.join(json.dumps(row) + '\n' for row in rows))
    return path


# A session: three turns to 214k, CSR compacts at 1300 (the first request after carries 66k), three more turns.
ROWS = [prompt(1000), reply(1010, 150_000, 'm1'), prompt(1100), reply(1110, 190_000, 'm2'), prompt(1200), reply(1210, 214_000, 'm3'),
        dict(type='system', subtype='compact_boundary', timestamp=stamp(1300), compactMetadata=dict(trigger='plugin', preTokens=214_000, postTokens=20_000, durationMs=40_000)),
        prompt(1400), reply(1410, 66_000, 'm4'), prompt(1500), reply(1510, 80_000, 'm5'), prompt(1600), reply(1610, 95_000, 'm6')]
DONE = dict(kind='compact', event='compact', outcome='compacted', session=SID, at=1301.0, ctx_before=214_000, by='countdown')


# 10-12 20:55, CSR's compaction of this session: 278,129 before, a 21,730 summary; the plan's floor was 69,579
# and the first request after carried 70,300; 110 requests followed before the card was drawn.
REAL = dict(before=278_129, plan_floor=69_579, summary=21_730, first=70_300)


class Payback(unittest.TestCase):
    def test_the_payback_is_the_status_lines_math_split_in_two(self):
        for floor, context in ((65_000, 214_000), (70_000, 180_589), (60_000, 400_000)):
            self.assertAlmostEqual(pane.payback_requests(floor, context),
                                   pane.compaction_cost(floor, context) / pane.request_saving(floor, context))
        self.assertIsNone(pane.payback_requests(65_000, 60_000))
        # the summary a compaction writes is measured: postTokens, ~21.7k, never the old 4,500
        self.assertEqual(pane.compaction_cost(65_000, 214_000), 65_000 * 2 + 21_700 * 5 + 21_400)
        # at the band's low edge, on the guessed floor, at 17 requests a turn: well within PAYBACK_UNDER turns
        self.assertLessEqual(pane.payback_turns(pane.payback_requests(pane.FLOOR_GUESS, 180_589), 17), pane.PAYBACK_UNDER)

    def test_the_real_compaction_cost_276k_and_paid_back_at_its_14th_request(self):
        self.assertEqual(round(pane.compaction_cost(REAL['plan_floor'], REAL['before'], REAL['summary'])), 275_621)
        main = [(1010.0 + 10 * i, REAL['first'] + 1_500 * i) for i in range(110)] + [(5010.0, 90_000)]
        record = dict(at=1000.0, ctx_before=REAL['before'], ctx_after=REAL['summary'])
        paid = pane.compaction_realized(record, main, [995.0, 5000.0])  # the next compaction at 5000 ends the count
        self.assertEqual((paid['floor'], paid['summary'], paid['requests']), (70_300, 21_730, 110))
        self.assertAlmostEqual(paid['saving'], (278_129 - 70_300) * 0.1)  # 20,782.9 a request
        self.assertEqual(round(paid['cost']), 277_063)  # on the measured floor, 1,442 over the plan's
        self.assertEqual((paid['even_request'], paid['even_at']), (14, 1010.0 + 130))
        self.assertEqual(round(paid['net']), 2_009_056)  # +2.0M vs not compacting (the plan's floor said 2,010,498)
        self.assertIsNone(pane.compaction_realized(record, [(990.0, 278_000)], [995.0]))  # no request after it yet
        self.assertIsNone(pane.compaction_realized(dict(at=1000.0), main, [995.0]))

    def test_requests_a_turn_are_the_mean_of_the_newest_whole_turns(self):
        main = [(t, 100_000) for t in (1, 2, 3, 201, 202, 203, 204, 205, 301, 302)]
        self.assertEqual(pane.requests_per_turn(main, [0, 100, 200, 300]), 4.0)  # 3, (0: not a turn), 5; the running one waits
        self.assertIsNone(pane.requests_per_turn(main, [0, 100]))  # one whole turn
        self.assertIsNone(pane.requests_per_turn([], []))

    def test_the_payback_is_said_in_api_calls_and_in_your_prompts(self):
        # 10-12: "2 of 14 requests to pay back, no clue still": API calls, then the prompts they take, rounded up
        self.assertEqual(pane.payback_text(13.2, 17), 'pays back in ~13 API calls (~1 prompt)')
        self.assertEqual(pane.payback_text(13.6, 8.9), 'pays back in ~14 API calls (~2 prompts)')
        self.assertEqual(pane.payback_text(52, 17), 'pays back in ~52 API calls (~4 prompts)')
        self.assertEqual(pane.payback_text(1, None), 'pays back in ~1 API call')  # calls a prompt unmeasured: calls alone
        self.assertEqual(pane.payback_text(None, 17), '')
        self.assertEqual((pane.payback_prompts(14, 14), pane.payback_prompts(14.1, 14), pane.payback_prompts(14, None)),
                         (1, 2, None))
        with open(COMPACT_TS, encoding='utf-8') as handle:  # the mod says it the same way
            ts = handle.read()
        for said in ("Math.ceil(payback.requests / payback.perTurn)", "`pays back in ~${plural(", "'API call')}`",
                     "plural(prompts, 'prompt')"):
            self.assertIn(said, ts)

    def test_compact_plan_reads_this_sessions_floor_and_says_whether_it_pays_back(self):
        with tempfile.TemporaryDirectory() as tmp:
            transcript(tmp, ROWS)
            log = os.path.join(tmp, 'dreams.jsonl')
            with open(log, 'w') as handle:
                handle.write(json.dumps(DONE) + '\n')
            with mock.patch.object(pane, 'HOME', tmp):
                got = pane.compact_plan_json(SID, 300_000, log)
            self.assertEqual(got['floor'], 66_000)  # the first request after the compaction, not postTokens
            self.assertEqual(got['summary'], 20_000)  # postTokens: what the compaction wrote
            self.assertTrue(got['measured'])
            self.assertEqual(got['payback'], round(pane.payback_requests(66_000, 300_000, 20_000), 1))
            self.assertEqual(got['per_turn'], 1.0)  # a request a turn in this stand-in session
            self.assertTrue(got['worth'])  # 11.4 requests, 11.4 turns: within 15
            self.assertIn('this session: 1 compaction by CSR · 0 by you', got['story']['text'])

    def test_a_session_with_no_compaction_plans_on_the_guessed_floor_and_has_no_story(self):
        with tempfile.TemporaryDirectory() as tmp:
            transcript(tmp, ROWS[:6])
            with mock.patch.object(pane, 'HOME', tmp):
                got = pane.compact_plan_json(SID, 214_000, os.path.join(tmp, 'none.jsonl'))
            self.assertEqual((got['floor'], got['measured'], got['story']), (pane.FLOOR_GUESS, False, None))
            self.assertEqual(got['summary'], pane.SUMMARY_GUESS)
            self.assertIsNotNone(got['payback'])
            self.assertIsNone(pane.compact_plan_json('../../etc', 214_000)['story'])  # a session id is never a path

    def test_the_mode_prints_json_for_the_mod(self):
        out = io.StringIO()
        with mock.patch.object(sys, 'argv', ['pane.py', '--repo', '/tmp', '--compact-plan', '--sid', 'no-such-session', '--ctx', '300000']), \
                contextlib.redirect_stdout(out):
            pane.main()
        got = json.loads(out.getvalue())
        # no session: no requests a turn, so each request is taken as a turn; 11.4 is within 15
        self.assertEqual((got['ctx'], got['measured'], got['per_turn'], got['worth']), (300_000, False, None, True))


class Story(unittest.TestCase):
    def test_csrs_compactions_and_yours_and_the_payback_so_far(self):
        main = [(1010.0, 150_000), (1110.0, 190_000), (1210.0, 214_000), (1410.0, 66_000), (1510.0, 80_000), (1610.0, 95_000)]
        sizes = [(214_000, 66_000, 40_000)]
        got = pane.compact_story([DONE], [1300.0], sizes, main, 95_000)
        net = 3 * pane.request_saving(66_000, 214_000) - pane.compaction_cost(66_000, 214_000)  # 3 requests after it
        self.assertEqual(got['payback'], int(net))
        self.assertEqual((got['by_csr'], got['by_you']), (1, 0))
        self.assertLess(net, 0)  # 3 requests in: in the word list's words, not 'cache payback so far ~-'
        self.assertEqual(got['text'], 'this session: 1 compaction by CSR · 0 by you · context 95k · not yet paid back ~'
                         + pane.short(-net) + ' tokens')
        paid = pane.compact_story([DONE], [1300.0], sizes, main[:3] + [(1410.0 + t, 66_000) for t in range(40)], 66_000)
        self.assertTrue(paid['text'].endswith(f"net saving so far ~{pane.tokens_said(paid['payback'])} tokens"), paid['text'])
        # a compaction of yours later stops the count of CSR's at it, and counts as yours
        later = pane.compact_story([DONE], [1300.0, 1450.0], sizes, main, 95_000)
        self.assertEqual(later['by_you'], 1)
        self.assertEqual(later['payback'], int(1 * pane.request_saving(66_000, 214_000) - pane.compaction_cost(66_000, 214_000)))
        # one just made, no request after it yet: its cost, spent, and nothing back
        fresh = pane.compact_story([DONE], [1300.0], sizes, main[:3], 214_000)
        self.assertEqual(fresh['payback'], int(-pane.compaction_cost(66_000, 214_000)))

    def test_no_compaction_of_csrs_no_story_and_skips_do_not_count(self):
        skip = dict(DONE, event='skip', outcome='skipped')
        self.assertIsNone(pane.compact_story([skip, dict(DONE, event='plan')], [1300.0], [], [], 95_000))

    def test_the_records_are_this_sessions_compact_ones_oldest_first(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, 'dreams.jsonl')
            with open(log, 'w') as handle:
                for record in (dict(DONE, at=5.0), dict(kind='look', session=SID, at=1.0), dict(DONE, session='other'),
                               dict(DONE, at=2.0, event='plan')):
                    handle.write(json.dumps(record) + '\n')
                handle.write('not json\n')
            self.assertEqual([(r['event'], r['at']) for r in pane.compact_records(SID, log)], [('plan', 2.0), ('compact', 5.0)])


class Plan(unittest.TestCase):
    def test_a_countdowns_seconds_come_from_its_end_and_a_taken_down_or_old_plan_is_none(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, f'{SID}.json')
            with open(path, 'w') as handle:
                json.dump(dict(at=1000.0, state='countdown', ends_at=1020.0, text='Compacting in 20s · 214k · commit 6369e6b landed'), handle)
            self.assertEqual(pane.compact_plan_of(SID, 1007.2, tmp),
                             dict(state='countdown', text='Compacting in 13s · 214k · commit 6369e6b landed'))
            self.assertEqual(pane.compact_plan_of(SID, 1030.0, tmp)['text'].split(' · ')[0], 'Compacting in 0s')
            self.assertIsNone(pane.compact_plan_of(SID, 1000.0 + pane.PLAN_LIFE + 1, tmp))
            with open(path, 'w') as handle:
                json.dump(dict(at=1000.0, state='none', text=None), handle)
            self.assertIsNone(pane.compact_plan_of(SID, 1001.0, tmp))
            self.assertIsNone(pane.compact_plan_of('../x', 1001.0, tmp))

    def test_the_plan_and_the_story_sit_under_the_gauges_and_on_the_info_page(self):
        info = dict(plan='Compact planned · 214k of your 181k–269k band · pays back in ~8 API calls (~1 prompt) · waiting: 1 agent running',
                    state='planned', story='this session: 2 compactions by CSR · 0 by you · context 142k · net saving so far ~120k tokens')
        lines = [plain(line) for line in pane.page_lines(pane.with_compacting(page(), dict(infinite=info)), 140, 30, {})]
        at = next(i for i, line in enumerate(lines) if 'Compact planned' in line)
        self.assertTrue(lines[0].startswith(' CTX '))  # the gauges stay first
        self.assertIn('this session: 2 compactions by CSR', lines[at + 1])
        bare = [plain(line) for line in pane.page_lines(pane.with_compacting(page(), {}), 140, 30, {})]
        self.assertFalse(any('Compact planned' in line for line in bare))
        rows = {f.label: f for f in pane.info_rows(dict(infinite=info), dict(main=[], prompts=[]), 2000.0, 80)}
        self.assertEqual(rows['Compact plan'].value, '214k of your 181k–269k band · pays back in ~8 API calls (~1 prompt) · waiting: 1 agent running')
        self.assertEqual(rows['Compactions'].value, '2 compactions by CSR · 0 by you · context 142k · net saving so far ~120k tokens')
        tallied = dict(info, tally=dict(by_csr=2, by_you=26, payback=3_100_000))  # the counts, said plainly on the i page
        rows = {f.label: f for f in pane.info_rows(dict(infinite=tallied), dict(main=[], prompts=[]), 2000.0, 80)}
        self.assertEqual((rows['Compactions'].value, rows['Compactions'].note, rows['Compactions'].section),
                         ('2 by CSR, ~3.1M saved · 26 by you', 'net saving, tokens at full input price, against not compacting',
                          pane.SESSION))
        owed = dict(info, tally=dict(by_csr=1, by_you=0, payback=-65_000))
        self.assertEqual(pane.info_rows(dict(infinite=owed), {}, 2000.0)[0].value, '1 by CSR, ~65k not yet paid back · 0 by you')

    def test_the_plan_row_wraps_by_its_pieces_and_is_never_cut(self):
        """10-12: renders cut it at 61 columns: "Compact planned · 196k of your 181k–269k band · pays back i…"."""
        plan = 'Compact planned · 196k of your 181k–269k band · pays back in ~14 API calls (~2 prompts) · waiting for a seam'
        info = dict(plan=plan, state='planned', story='this session: 1 compaction by CSR · 26 by you')
        for cols, want in ((62, ['Compact planned · 196k of your 181k–269k band',
                                 'pays back in ~14 API calls (~2 prompts) · waiting for a seam']),
                           (46, ['Compact planned · 196k of your 181k–269k band', 'pays back in ~14 API calls (~2 prompts)',
                                 'waiting for a seam'])):
            lines = [flat(line).strip() for line in pane.page_lines(pane.with_compacting(page(), dict(infinite=info)), cols, 40, {})]
            at = lines.index(want[0])
            self.assertEqual(lines[at:at + len(want)], want, (cols, lines))
            self.assertTrue(lines[at + len(want)].startswith('this session: 1 compaction by CSR'))
            self.assertFalse(any(line.endswith('…') for line in lines[at:at + len(want)]))
        self.assertEqual(pane.piece_rows('a · ' + 'b' * 30, 20, 3), ['a', 'b' * 19 + '…'])  # a piece wider than the row: cut alone
        self.assertEqual(len(pane.piece_rows(' · '.join(['word'] * 40), 20, 3)), 3)  # three rows at most

    def test_infinite_info_is_none_with_neither_plan_nor_story(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(pane.infinite_info(SID, 2000.0, None, tmp, os.path.join(tmp, 'log')))


SUMMARY = """1. Primary Request and Intent:
- Build the compact coach.
4. Errors and fixes:
- **Keep list brought back stale threads:** fixed in 5202d4c, only the open question goes.
- **User feedback:** "you should screenshot it", so every UI change ships with a screenshot.
5. Problem Solving:
- Solved."""
KEEP = dict(running=['subagent a95ea904 · Fable: blind pigment concept'], decisions=[],
            work=['branch main at fdaba23', '7 uncommitted files (src/engine.rs, mcp/mod.rs, …)'], rules=[])


def record(event, at, **more):
    return dict(kind='compact', event=event, at=at, session=SID, ctx_before=278_129, why='over 269k · 1 agent kept', **more)


class Card(unittest.TestCase):
    """10-12: "if you trigger the compact, make it a card, note what you learnt from this convo in the pane and
    stream the info as needed"."""

    def test_the_card_streams_each_stage_as_the_mod_writes_it(self):
        records = [record('plan', 1000.0), record('countdown', 1005.0, ends_at=1025.0)]
        soon = pane.compaction_card(records, [], 1011.2)
        self.assertEqual((soon['stage'], soon['left'], soon['why']), ('soon', 14, 'over 269k · 1 agent kept'))
        records.append(record('start', 1025.5, keep=KEEP))
        running = pane.compaction_card(records, [], 1067.0)
        self.assertEqual((running['stage'], running['elapsed']), ('running', 41))
        self.assertEqual(running['keeping'][0], 'subagent a95ea904 · Fable: blind pigment concept')
        records.append(record('compact', 1172.0, outcome='compacted', floor=69_579, ctx_after=21_730, took_ms=146_489,
                              payback=9.1, keep=KEEP))
        done = pane.compaction_card(records, [(1100.0, 'older'), (1171.5, SUMMARY)], 1180.0)
        self.assertEqual((done['stage'], done['after'], done['measured'], done['payback'], done['paid']), ('done', 69_579, False, 9.1, None))
        main = [(1175.0 + i, 70_300 + 100 * i) for i in range(20)]  # the requests after it: what it has paid
        paid = pane.compaction_card(records, [(1171.5, SUMMARY)], 1200.0, main, [1171.0], [21_730])
        self.assertEqual((paid['after'], paid['measured'], paid['paid']['requests'], paid['paid']['even_request']), (70_300, True, 20, 14))
        self.assertEqual(done['learnt'], ['Keep list brought back stale threads: fixed in 5202d4c, only the open question goes.',
                                          'User feedback: "you should screenshot it", so every UI change ships with a screenshot.'])

    def test_a_cancel_takes_it_down_a_skip_says_why_and_a_done_one_goes_after_half_an_hour(self):
        self.assertIsNone(pane.compaction_card([record('countdown', 1005.0, ends_at=1025.0), record('cancel', 1010.0)], [], 1011.0))
        self.assertIsNone(pane.compaction_card([record('countdown', 1005.0, ends_at=1025.0)], [], 1040.0))  # nothing followed
        skipped = pane.compaction_card([record('skip', 1010.0, reason='a hook vetoed it')], [], 1020.0)
        self.assertEqual((skipped['stage'], skipped['reason']), ('skipped', 'a hook vetoed it'))
        done = [record('compact', 1010.0, outcome='compacted', took_ms=1000)]
        self.assertEqual(pane.compaction_card(done, [], 1010.0 + pane.CARD_LIFE - 1)['stage'], 'done')
        self.assertIsNone(pane.compaction_card(done, [], 1010.0 + pane.CARD_LIFE + 1))
        self.assertIsNone(pane.compaction_card([record('plan', 1000.0)], [], 1001.0))  # a plan is the masthead's row

    def test_on_the_page_it_sits_under_the_pinned_story_while_it_runs_and_once_done(self):
        story = dict(rev=3, now='Building the card.', text='Building the card.', at=1990.0)
        running = dict(card=pane.compaction_card([record('start', 1958.0, keep=KEEP)], [], 2000.0))
        lines = [plain(line) for line in pane.page_lines(pane.with_compacting(page(story=story, infinite=running), {}), 100, 60, {})]
        heads = [line.strip() for line in lines if line.strip().startswith(('Compacting', 'Story'))]
        self.assertTrue(heads[0].startswith('Story') and heads[1].startswith('Compacting'), heads)  # the story is pinned
        self.assertTrue(any('Summarizing 278k · over 269k · 1 agent kept' in line for line in lines))
        self.assertTrue(any('Keeping: subagent a95ea904' in line for line in lines))
        main = [(1990.0 + 6 * i, 70_300 + 1_500 * i) for i in range(110)]
        done = dict(card=pane.compaction_card([record('compact', 1990.0, outcome='compacted', floor=69_579, ctx_after=21_730,
                                                       took_ms=146_489, payback=13.2, per_turn=17.2, keep=KEEP)],
                                              [(1990.0, SUMMARY)], 2000.0, main, [1989.0], [21_730]))
        page_done = page(story=story, infinite=done)
        lines = [plain(line) for line in pane.page_lines(pane.with_compacting(page_done, {}), 100, 60, {})]
        names = [section['name'] for section in page_done['sections']]
        self.assertEqual(names.index('compacted'), names.index('story') + 1)
        # 10-12: "this all is jargon": a fact a sentence, in the order the card reads
        said = ['Context: 278k → 70k tokens.', 'Each API call now sends 208k fewer tokens.',
                'Paid back at call 14. Now at call 110.', 'Net saving: 2.0M tokens at full input price.',
                'Took 2:26.', 'Kept 3 items, 2 lessons. Click to read.']
        at = [next((i for i, line in enumerate(lines) if text in line), None) for text in said]
        self.assertTrue(None not in at and at == sorted(at), (at, lines))
        self.assertFalse(any('Learnt:' in line or 'Kept:' in line for line in lines))  # counted, not cut
        item = next(section for section in page_done['sections'] if section['name'] == 'compacted')['items'][0]
        self.assertIn('Kept: subagent a95ea904 · Fable: blind pigment concept', item['send'])  # a click hands Claude all of it
        self.assertIn('Learnt: User feedback: "you should screenshot it", so every UI change ships with a screenshot.', item['send'])
        opened = [plain(line) for line in pane.page_lines(pane.with_compacting(page_done, {}), 100, 80, dict(open=item['key']))]
        text = ' '.join(' '.join(opened).split())
        for whole in ('Kept: 7 uncommitted files (src/engine.rs, mcp/mod.rs, …)',
                      'Learnt: Keep list brought back stale threads: fixed in 5202d4c, only the open question goes.',
                      'Learnt: User feedback: "you should screenshot it", so every UI change ships with a screenshot.',
                      'Cost: cache 141k + summary 109k + read 28k.'):
            self.assertIn(whole, text)  # opened, every item whole, past the four rows other cards show


    def test_the_done_card_says_how_many_threads_wait_on_you_in_lost_and_found(self):
        """10-12 21:17: the notice the phone shows says it, and the card says the same count, live."""
        thread = lambda text, waiting, hidden, rank, state: dict(
            text=text, waiting=waiting, hidden=hidden, rank=rank, state=state, key=text, first_seen=1500.0,
            last_seen=1600.0, times=1, detail=[], why='')
        lost = [thread('a', True, None, 0, 'lost'), thread('b', True, None, 2, 'open'), thread('c', True, 'later', 0, 'lost'),
                thread('d', True, None, 4, 'answered'), thread('e', False, None, 1, 'lost')]
        self.assertEqual(pane.lost_waiting(lost), 2)  # set aside, answered or quiet: not waiting
        self.assertEqual((pane.lost_waiting(None), pane.lost_row(0), pane.lost_row(1)), (0, '', '1 waiting in Lost and found.'))
        story = dict(rev=3, now='Building the card.', text='Building the card.', at=1990.0)
        done = dict(card=pane.compaction_card([record('compact', 1990.0, outcome='compacted', floor=69_579, ctx_after=21_730,
                                                       took_ms=146_489, keep=KEEP)], [], 2000.0))
        built = page(story=story, infinite=done, lost=lost)
        item = next(section for section in built['sections'] if section['name'] == 'compacted')['items'][0]
        self.assertEqual(item['body'][-1], '2 waiting in Lost and found.')
        self.assertIn('2 waiting in Lost and found.', item['send'])
        quiet = page(story=story, infinite=done, lost=lost[2:])
        item = next(section for section in quiet['sections'] if section['name'] == 'compacted')['items'][0]
        self.assertFalse(any('Lost and found' in row for row in item['body']))
        self.assertLessEqual(len(pane.lost_row(999)), pane.CARD_COLS)
        running = pane.compact_section(pane.compaction_card([record('start', 1958.0, keep=KEEP)], [], 2000.0), waiting=2)
        self.assertFalse(any('Lost and found' in row for row in running['items'][0]['body']))  # said once it has run


class Figure(unittest.TestCase):
    """10-12: "i really dont like these want something more higher fidelity": the payback in pixels, the masthead's
    kind, never in block characters."""

    def facts(self, n=110, even=True):
        series = [(2000.0 + 6 * i, 70_300 + 1_500 * i) for i in range(n)]
        return dict(at=1995.0, before=278_129, floor=70_300, series=series, even_at=series[13][0] if even and n > 13 else None,
                    pre=[(1900.0 + 6 * i, 262_000 + 1_300 * i) for i in range(12)])

    def card(self, main):
        return pane.compaction_card([record('compact', 1995.0, outcome='compacted', ctx_after=21_730, took_ms=146_489,
                                            payback=13.2, per_turn=17.2, keep=KEEP)], [(1995.0, SUMMARY)], 2700.0,
                                    main, [1994.0], [21_730])

    def test_the_done_card_carries_its_picture_and_the_stages_before_it_none(self):
        main = [(1900.0 + 6 * i, 262_000 + 1_300 * i) for i in range(12)] + self.facts()['series']
        item = pane.compact_section(self.card(main))['items'][0]
        self.assertEqual((item['picture']['kind'], item['picture']['rows']), ('payback', pane.PAYBACK_ROWS))
        facts = item['picture']['facts']  # the card's own numbers: its rows and its line agree
        self.assertEqual((round(facts['cost']), facts['need'], facts['done'], round(facts['net']), facts['per']),
                         (277_063, 14, 110, 2_009_056, 17.2))
        one = pane.compact_section(self.card(main[:13]))['items'][0]['picture']['facts']  # one call after: a line already
        self.assertEqual((one['done'], round(one['net'])), (1, round(20_782.9 - 277_063)))
        running = pane.compaction_card([record('start', 1958.0, keep=KEEP)], [], 2000.0)
        self.assertIsNone(pane.compact_section(running)['items'][0]['picture'])

    def test_without_pixels_the_payback_is_its_text_rows_never_block_characters(self):
        main = self.facts()['series']
        saved = pane.PIXELS
        try:
            pane.PIXELS = False
            done = dict(card=self.card(main))
            lines = [plain(line) for line in pane.page_lines(pane.with_compacting(page(infinite=done), {}), 100, 60, {})]
        finally:
            pane.PIXELS = saved
        self.assertTrue(any('Net saving: 2.0M tokens at full input price.' in line for line in lines), lines)
        self.assertFalse(any(set(line) & set('▁▂▃▄▅▆▇█') for line in lines))

    def test_each_state_says_its_payback_in_two_plain_rows(self):
        """10-12: "2 of 14 requests to pay back, no clue still. 231k still to recoup etc. doesnt make sense"."""
        rows = lambda card: pane.payback_rows(pane.card_payback(card))
        just = pane.compaction_card([record('compact', 1995.0, outcome='compacted', floor=69_579, ctx_after=21_730,
                                            took_ms=146_489, payback=13.2, per_turn=17.2)], [], 2000.0)
        self.assertEqual(rows(just), ['One-time cost: 276k tokens at full input price.',  # the plan's floor, a guess
                                      'Each call saves 21k. Paid back at call 14.'])
        self.assertEqual(rows(self.card(self.facts()['series'][:6])), ['Paid back: 6 of 14 calls. 8 calls to go.',
                                                                       'Earned back: 125k of 277k (full input price).'])
        self.assertEqual(rows(self.card(self.facts()['series'][:13]))[0], 'Paid back: 13 of 14 calls. 1 call to go.')
        self.assertEqual(rows(self.card(self.facts()['series'])), ['Paid back at call 14. Now at call 110.',
                                                                   'Net saving: 2.0M tokens at full input price.'])
        # earned back is the real series' net plus the cost, never calls times a rounded saving: 10-12 15:46
        # the card said 149k not yet paid back at 6 calls (271,864 less 6 x 20,405), where 6 x 20.4k off 276k is 154k
        real = dict(paid=dict(before=274_073, floor=70_026, summary=20_881, cost=271_864, saving=20_405, requests=6,
                              net=6 * 20_405 - 271_864))
        self.assertEqual(rows(real)[1], 'Earned back: 122k of 272k (full input price).')
        self.assertEqual(pane.short(-real['paid']['net']), '149k')
        never = pane.compaction_card([record('compact', 1995.0, outcome='compacted', floor=300_000, ctx_after=21_730)], [], 2000.0)
        section = pane.compact_section(never)['items'][0]
        self.assertEqual(section['body'][:2], ['This compaction does not pay back.', 'The context did not get smaller.'])
        self.assertEqual((section['picture'], section['lede']), (None, []))
        self.assertIsNone(pane.card_payback(dict(before=None, after=70_000)))

    def test_no_row_passes_48_columns_at_its_longest_numbers(self):
        worst = [pane.payback_rows(dict(cost=c, saving=s, need=n, done=d, net=d * s - c, earned=d * s, before=0, floor=0))
                 for c, s, n, d in ((999_999, 999_000, 999, 0), (999_999, 1_001, 999, 998), (9_990_000, 99_000, 101, 9_999),
                                    (275_000, 20_400, 14, 13))]
        rows = [row for pair in worst for row in pair] + [
            'Context: 1.2M → ~999k tokens.', 'Each API call now sends 999k fewer tokens.',
            pane.took_row(599, dict(median=599, n=999)), pane.took_row(600, dict(median=650, n=1440)),
            pane.kept_row(99, 99), pane.kept_row(1, 0), pane.kept_row(0, 1)]
        for row in rows:
            self.assertLessEqual(len(row), pane.CARD_COLS, row)
            self.assertFalse(set(row) & set('…—'), row)
        self.assertEqual(pane.took_row(600, dict(median=650, n=1440)), 'Took 10:00. Your /compact: 10:50 (median).')
        self.assertEqual(pane.took_row(89, None), 'Took 1:29.')
        self.assertEqual((pane.kept_row(1, 0), pane.kept_row(0, 0)), ('Kept 1 item. Click to read.', ''))

    @unittest.skipUnless(pane.HAS_PIL, 'Pillow is not installed')
    def test_with_pixels_the_picture_lies_over_its_rows_and_draws_as_a_payback(self):
        main = self.facts()['series']
        saved = pane.PIXELS, pane.cell_size
        try:
            pane.PIXELS, pane.cell_size = True, lambda: (16, 34)
            lines, pictures = pane.draw_page(pane.with_compacting(page(infinite=dict(card=self.card(main))), {}), 100, 60, {})
        finally:
            pane.PIXELS, pane.cell_size = saved
        wanted = [w for w in lines.wanted if w[2].get('kind') == 'payback']
        self.assertEqual(len(wanted), 1)
        row, run = wanted[0][:2]
        self.assertEqual(run, pane.PAYBACK_ROWS)
        shot = [p for p in pictures if p[0] == row]
        self.assertEqual(len(shot), 1)
        self.assertTrue(shot[0][4].startswith(b'\x89PNG'))

    def money(self, done):
        cost, saving = 277_063, 20_782.9
        return dict(at=1995.0, cost=cost, saving=saving, need=14, done=done, net=done * saving - cost, per=17.2)

    @unittest.skipUnless(pane.HAS_PIL, 'Pillow is not installed')
    def test_the_figure_is_one_break_even_line_in_named_zones(self):
        from PIL import Image
        ground, cell = pane.tint('compacted', pane.ROW), (16, 34)
        png = pane.payback_png(self.money(110), 53, pane.PAYBACK_ROWS, cell, ground)
        self.assertIs(pane.payback_png(self.money(110), 53, pane.PAYBACK_ROWS, cell, ground), png)  # drawn once
        image = Image.open(io.BytesIO(png))
        self.assertEqual((image.mode, image.size), ('RGB', (53 * 16, pane.PAYBACK_ROWS * 34)))  # RGB: nothing shows through
        colours = set(image.getdata())
        near = lambda pixels, ink, within: any(all(abs(a - b) <= within for a, b in zip(p, ink[:3])) for p in pixels)
        blend = lambda ink, alpha: tuple(round(g + (i - g) * alpha / 255) for g, i in zip(ground, ink))
        self.assertTrue(near(colours, blend(pane.INK['learn'], 40), 3))  # over zero: saving
        self.assertTrue(near(colours, blend(pane.INK['quiet'], 30), 3))  # under it: not yet paid back
        for ink in ('ctx', 'turn', 'learn'):  # the calls so far, the compaction's cost, where it paid back
            self.assertTrue(near(colours, pane.INK[ink], 30), ink)
        green = lambda png: sum(1 for p in Image.open(io.BytesIO(png)).getdata() if near([p], pane.INK['learn'], 20))
        owed = pane.draw_payback(self.money(6), 53, 5, cell, ground)
        self.assertGreater(green(png), green(owed))  # the paid-back dot and its words turn green only once passed
        just = pane.draw_payback(self.money(0), 53, 5, cell, ground)  # just compacted: the cost, nothing earned
        self.assertTrue(just.startswith(b'\x89PNG'))
        self.assertIsNone(pane.payback_png(dict(self.money(3), need=None, saving=0), 53, 5, cell, ground))  # never pays back
        for odd in (None, {}, dict(series='abc')):
            self.assertIsNone(pane.payback_png(odd, 53, 5, cell, ground), odd)
        saved, pane.HAS_PIL = pane.HAS_PIL, False
        try:
            self.assertIsNone(pane.payback_png(self.money(50), 53, 5, cell, ground))
        finally:
            pane.HAS_PIL = saved

    @unittest.skipUnless(pane.HAS_PIL, 'Pillow is not installed')
    def test_the_axis_names_what_it_counts_with_the_calls_a_prompt_when_known(self):
        from PIL import Image, ImageDraw
        cell, ground = (16, 34), pane.tint('compacted', pane.ROW)
        said = []
        real = ImageDraw.ImageDraw.text
        def keep(self_, xy, text, *a, **k):
            said.append(text)
            return real(self_, xy, text, *a, **k)
        with mock.patch.object(ImageDraw.ImageDraw, 'text', keep):
            pane.draw_payback(self.money(6), 53, 5, cell, ground)
            self.assertIn('API calls after compacting (~17 a prompt)', said)
            said.clear()
            pane.draw_payback(dict(self.money(6), per=None), 53, 5, cell, ground)  # unmeasured: no per prompt
            self.assertIn('API calls after compacting', said)
            self.assertFalse(any('a prompt' in t for t in said))
            said.clear()
            pane.draw_payback(self.money(6), 22, 5, cell, ground)  # no room: the shorter words, never over the ticks
            self.assertFalse(any('a prompt' in t for t in said), said)


class YourCompact(unittest.TestCase):
    """10-12: "did we add time to compact? what was our time vs theirs": CSR's beside your own /compact."""
    NOW = 1_800_000_000.0

    def boundary(self, ago, pre, seconds, trigger='manual'):
        return dict(type='system', subtype='compact_boundary', timestamp=stamp(self.NOW - ago),
                    compactMetadata=dict(trigger=trigger, preTokens=pre, postTokens=21_000, durationMs=seconds * 1000))

    def projects(self, tmp):
        folder = os.path.join(tmp, 'projects', 'p')
        os.makedirs(folder)
        with open(os.path.join(folder, 's.jsonl'), 'w') as handle:
            for row in (self.boundary(3600, 200_000, 80), self.boundary(7200, 150_000, 60),  # under the band
                        self.boundary(9000, 250_000, 70, 'auto'), self.boundary(5000, 250_000, 140),  # CSR's: at 5000 ago
                        self.boundary(86400, 300_000, 100), self.boundary(40 * 86400, 250_000, 50),  # 40 days ago
                        self.boundary(2 * 86400, 400_000, 200), self.boundary(3 * 86400, 190_000, 95)):  # past the band
                handle.write(json.dumps(row) + '\n')
            handle.write('{"type": "system", "subtype": "compact_boundary", "torn\n')
        os.utime(os.path.join(folder, 's.jsonl'), (self.NOW, self.NOW))  # written lately, as NOW reads it
        old = os.path.join(folder, 'old.jsonl')  # a transcript untouched for 40 days is not read at all
        with open(old, 'w') as handle:
            handle.write(json.dumps(self.boundary(40 * 86400, 250_000, 30)) + '\n')
        os.utime(old, (self.NOW - 40 * 86400, self.NOW - 40 * 86400))
        return os.path.join(tmp, 'projects')

    def test_your_own_are_manual_in_the_band_in_30_days_and_never_csrs(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, 'dreams.jsonl')
            with open(log, 'w') as handle:
                handle.write(json.dumps(dict(kind='compact', event='compact', at=self.NOW - 5000 + 6)) + '\n')
                handle.write(json.dumps(dict(kind='compact', event='plan', at=self.NOW - 3600)) + '\n')  # a plan is no compaction
            csr = pane.csr_compaction_times([log])
            self.assertEqual(csr, [self.NOW - 4994])
            self.assertEqual(pane.own_compactions(self.projects(tmp), self.NOW, csr), [80.0, 95.0, 100.0])
            kept = pane.keep_compact_usual(self.NOW, os.path.join(tmp, 'projects'), os.path.join(tmp, 'usual.json'), [log])
            self.assertEqual((kept['median'], kept['n'], kept['excluded']), (95.0, 3, 1))
            self.assertEqual(pane.compact_usual(self.NOW, os.path.join(tmp, 'usual.json'), refresh=False), dict(median=95.0, n=3))

    def test_the_card_never_scans_a_day_old_answer_starts_one_process(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, 'usual.json')
            with open(path, 'w') as handle:
                json.dump(dict(at=self.NOW - 2 * 86400, median=89.5, n=144), handle)
            with mock.patch.object(pane.subprocess, 'Popen') as popen, mock.patch.object(pane, '_USUAL_ASKED', [0.0]):
                self.assertEqual(pane.compact_usual(self.NOW, path), dict(median=89.5, n=144))  # the old answer, now
                self.assertEqual(pane.compact_usual(self.NOW + 60, path), dict(median=89.5, n=144))
                self.assertEqual(popen.call_count, 1)  # one scan, in a process of its own
                self.assertIn('--compact-usual', popen.call_args[0][0])
                self.assertTrue(popen.call_args[1]['start_new_session'])
            with mock.patch.object(pane.subprocess, 'Popen') as popen, mock.patch.object(pane, '_USUAL_ASKED', [0.0]):
                self.assertIsNone(pane.compact_usual(self.NOW, os.path.join(tmp, 'none.json')))
                self.assertEqual(popen.call_count, 1)
                with open(path, 'w') as handle:
                    json.dump(dict(at=self.NOW, median=None, n=2), handle)
                self.assertIsNone(pane.compact_usual(self.NOW + 900, path))  # fresh: no scan; under three: no median
                self.assertEqual(popen.call_count, 1)

    def test_the_card_says_how_long_it_took_beside_yours(self):
        card = pane.compaction_card([record('compact', 1990.0, outcome='compacted', took_ms=146_489, keep=KEEP)], [], 2000.0)
        self.assertIsNone(card['usual'])  # the card itself never reads it: infinite_info does, live
        card['usual'] = dict(median=89.5, n=144)
        item = pane.compact_section(card)['items'][0]
        self.assertIn('Took 2:26. Your /compact: 1:29 (median of 144).', item['body'])
        self.assertNotIn(' in 2:26', item['title'])  # said once, beside yours


class OptIn(unittest.TestCase):
    """10-12: auto-compact shipped on for anyone who added the plugin. Compacting on its own is opt-in."""

    def test_the_shipped_plugin_offers_a_compaction_and_never_makes_one_unasked(self):
        with open(os.path.join(HERE, '..', '.claude-plugin', 'plugin.json'), encoding='utf-8') as handle:
            manifest = json.load(handle)
        row = manifest['userConfig']['autoCompact']
        self.assertEqual(row['options'], ['off', 'suggest', 'auto'])
        self.assertTrue(row['description'].startswith('Compacting on its own is opt-in'))
        if row['default'] != 'suggest':  # only the owner's own copy, under another name, may ship auto
            self.assertNotEqual(manifest['name'], 'csr-' + 'companion')  # spelled apart: the dev mod sync renames the whole word
        with open(COMPACT_TS, encoding='utf-8') as handle:
            self.assertIn("value === 'off' || value === 'auto' ? value : 'suggest'", handle.read())  # a missing value too


class Log(unittest.TestCase):
    def test_the_dream_log_takes_kind_compact_with_its_repo(self):
        kept = []
        with mock.patch.object(pane, 'keep', kept.append), \
                mock.patch.object(sys, 'argv', ['pane.py', '--repo', HERE, '--keep', json.dumps(dict(DONE))]), \
                contextlib.redirect_stdout(io.StringIO()):
            pane.main()
        self.assertEqual(kept[0]['kind'], 'compact')
        self.assertIn('repo', kept[0])  # stamped like a dream's, so the pane can keep projects apart


class Headers(unittest.TestCase):
    def test_csr_engines_guard_holds_both_headers_the_mod_writes(self):
        """What CSR writes around a compaction must open with a header csr-engine's provenance guard knows, or
        the engine would import the keep list or the notice as the owner's words."""
        with open(COMPACT_TS, encoding='utf-8') as handle:
            source = handle.read()
        headers = dict(re.findall(r"export const (KEEP_HEADER|NOTICE_HEADER) = '([^']+)'", source))
        self.assertEqual(set(headers), {'KEEP_HEADER', 'NOTICE_HEADER'})
        if not os.path.exists(PROVENANCE):
            self.skipTest('csr-engine is not beside this plugin')
        with open(PROVENANCE, encoding='utf-8') as handle:
            rust = handle.read()
        listed = rust[rust.index('const EMISSION_HEADERS'):]
        listed = listed[:listed.index('];')]
        for name, header in headers.items():
            self.assertIn(f'"{header}"', listed, name)


if __name__ == '__main__':
    unittest.main()
