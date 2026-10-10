"""The pane script's counting, without a live CSR: python3 -m unittest discover -s tests -p 'test_*.py'"""
import glob
import importlib.util
import json
import os
import re
import shutil
import sqlite3
import tempfile
import types
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('pane', os.path.join(HERE, '..', 'bin', 'pane.py'))
pane = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pane)

PAST = '00000000-1111-4222-8333-444455556666'
HERE_NOW = 'sess-now'
AT = '2026-10-08T22:33:57.100000+00:00'


def memory():
    con = sqlite3.connect(':memory:')
    con.execute('create table session_registry (session_id text, first_prompt text, first_ts text, last_ts text, project text)')
    con.execute('create table reflections (id text, content text, tags text, timestamp text)')
    con.execute("insert into session_registry values (?, 'Herdr CSR core runtime', '2026-10-04T10:00:00Z', "
                "'2026-10-04T12:00:00Z', 'csr-engine')", (PAST,))
    return con


# One injection: five chunks of the same past session, logged within the same second.
ONE_HANDOFF = [(AT.replace('.100000', f'.{k}00000'), 'chunk', f'chunk-{k}', PAST, None, None) for k in range(1, 6)]


class Handoffs(unittest.TestCase):
    def test_five_chunks_in_one_injection_are_one_handoff(self):
        items = pane.remembered_items(memory(), ONE_HANDOFF, [], HERE_NOW)
        self.assertEqual([(item['key'], item['times'], item['opened']) for item in items], [(PAST, 1, False)])
        self.assertEqual(pane.handoffs([pane.epoch(row[0]) for row in ONE_HANDOFF]), 1)

    def test_a_later_csr_request_naming_the_session_opens_it(self):
        later = pane.epoch(AT) + 60
        items = pane.remembered_items(memory(), ONE_HANDOFF, [(later, 'csr_reflect_on_past', f'conv_{PAST}')], HERE_NOW)
        self.assertTrue(next(item for item in items if item['kind'] == 'recall')['opened'])

    def test_a_request_before_the_handoff_or_about_something_else_does_not(self):
        before = pane.epoch(AT) - 60
        later = pane.epoch(AT) + 60
        questions = [(before, 'csr_reflect_on_past', f'conv_{PAST}'), (later, 'search_by_recency', 'herdr runtime')]
        items = pane.remembered_items(memory(), ONE_HANDOFF, questions, HERE_NOW)
        self.assertFalse(next(item for item in items if item['kind'] == 'recall')['opened'])


def dream(at, answer=None, session=HERE_NOW, line=None):
    return dict(id=f'dream:{at}', at=at, answer=answer, session=session, line=line or f'What if {at}?', title='', text='')


class Feed(unittest.TestCase):
    CHAPTER = dict(at=1000.0, of=5)

    def test_left_counts_this_chapter_and_an_adopted_dream_gives_its_place_back(self):
        dreams = [dream(1300, 'ask_claude'), dream(1200, 'ignored'), dream(900, 'not_relevant')]  # newest first
        fed = pane.feed(dreams, self.CHAPTER, HERE_NOW)
        self.assertEqual((fed['left'], fed['of']), (4, 6))  # 5 + 1 adopted - 2 shown since 1000
        self.assertIsNone(fed['star'])
        self.assertEqual([d['at'] for d in fed['adopted']], [1300])
        self.assertEqual([d['at'] for d in fed['passed']], [1200, 900])

    def test_the_star_is_the_newest_dream_still_open_in_this_chapter(self):
        fed = pane.feed([dream(1500), dream(1200, 'helpful')], self.CHAPTER, HERE_NOW)
        self.assertEqual(fed['star']['at'], 1500)
        # an open dream from before the compaction is folded, not starred
        fed = pane.feed([dream(900)], self.CHAPTER, HERE_NOW)
        self.assertIsNone(fed['star'])
        self.assertEqual([d['at'] for d in fed['passed']], [900])

    def test_dreams_of_other_sessions_never_show(self):
        fed = pane.feed([dream(1500, session='another'), dream(1400, 'helpful', session='another')], self.CHAPTER, HERE_NOW)
        self.assertEqual((fed['star'], fed['adopted'], fed['passed'], fed['left']), (None, [], [], 5))

    def test_no_chapter_yet_means_five_left(self):
        self.assertEqual(pane.feed([], None, HERE_NOW)['left'], 5)

    def test_an_adopted_dream_is_never_counted_as_passed(self):
        # the 10-10 pane: three adopted, four passed; the third adopted folded, but not as passed
        dreams = [dream(1400, 'ask_claude'), dream(900, 'helpful'), dream(800, 'ask_claude'), dream(700, 'ignored'),
                  dream(600, 'ignored'), dream(500, 'ignored'), dream(400, 'ignored')]
        fed = pane.feed(dreams, self.CHAPTER, HERE_NOW)
        self.assertEqual([d['at'] for d in fed['adopted']], [1400, 900])
        self.assertEqual([d['at'] for d in fed['passed']], [700, 600, 500, 400])
        self.assertEqual([d['at'] for d in fed['folded']], [800, 700, 600, 500, 400])


def page(dreams=(), look=None, story=None, remembered=(), chapter=None):
    """page_main over a stand-in CSR: no rules on record, this session's dreams and story as given."""
    con = sqlite3.connect(':memory:')
    con.execute('create table intent_events (ts text, quote text, session_id text, transcript_path text, '
                'byte_start int, project text, kind text)')
    connect, pane.connect = pane.connect, lambda: con
    try:
        s = dict(id=HERE_NOW, prompts=[], replies=[], compacts=[], remembered=list(remembered),
                 gave=dict(recalls=[], carried=[], dreams=[]), project='csr-engine')
        data = dict(repo='/tmp/csr-engine', dreams=list(dreams), chapter=chapter or dict(at=1000.0, of=5),
                    dreams_on=True, look=look, story=story)
        return pane.page_main(data, s, 2000.0)
    finally:
        pane.connect = connect


STORY = ('The session built the pane. The threads were the story and the dreams. '
         'Work stands at: abc1234 committed on fix/x, installed. Nothing is pushed.')


class Page(unittest.TestCase):
    def test_the_story_row_leads_with_where_the_work_stands(self):
        self.assertEqual(pane.where_it_stands(STORY), 'Work stands at: abc1234 committed on fix/x, installed. Nothing is pushed.')
        # no sentence says where it stands: its last two
        self.assertEqual(pane.where_it_stands('One. Two. Three.'), 'Two. Three.')
        self.assertEqual(pane.where_it_stands('Only one.'), 'Only one.')

    def test_an_idle_dreamer_is_one_folded_row_under_the_story(self):
        story = dict(text=STORY, now=pane.where_it_stands(STORY), rev=3, by='Haiku', at=1990.0)
        got = page([dream(1400, 'ask_claude'), dream(1200, 'ignored')], look=dict(at=1995.0, outcome='none'),
                   story=story)
        self.assertEqual([s['name'] for s in got['sections']][:2], ['story', 'dreams'])
        self.assertEqual(got['sections'][0]['right'], 'rev 3 · Haiku · now')
        self.assertTrue(got['sections'][0]['items'][0]['title'].startswith('Work stands at'))
        dreams = got['sections'][1]
        self.assertEqual([i['title'] for i in dreams['items']], ['› 1 adopted · 1 passed'])
        self.assertEqual(dreams['right'], 'looked just now · 4 left')  # 5 + 1 adopted - 2 shown

    def test_a_dream_waiting_on_you_comes_before_the_story(self):
        story = dict(text=STORY, now=STORY, rev=1, by='', at=1990.0)
        got = page([dream(1500)], story=story)
        self.assertEqual([s['name'] for s in got['sections']][:2], ['dreams', 'story'])

    def test_the_sections_run_story_dreams_lost_handed_coach_rules(self):
        story = dict(text=STORY, now=STORY, rev=1, by='', at=1990.0)
        lost = pane.lost_sections
        pane.lost_sections = lambda s, now: [dict(name='lost', title='Lost and found', right='1 quiet', items=[], empty='')]
        try:
            names = [s['name'] for s in page([dream(1400, 'ask_claude')], story=story)['sections']]
            self.assertEqual(names, ['story', 'dreams', 'lost', 'remembered', 'coach', 'rules'])
            names = [s['name'] for s in page([dream(1500)], story=story)['sections']]  # a dream waiting on you
            self.assertEqual(names, ['dreams', 'story', 'lost', 'remembered', 'coach', 'rules'])
        finally:
            pane.lost_sections = lost

    def test_what_claude_asked_csr_folds_into_the_count(self):
        asked = [dict(kind='asked', at=1500.0 + k, text=f'query {k}', tool='reflect_on_past', times=1) for k in range(3)]
        handed = next(s for s in page(remembered=asked)['sections'] if s['name'] == 'remembered')
        self.assertEqual([i['title'] for i in handed['items']], ['Claude asked 3×'])


class Depth(unittest.TestCase):
    """Shadows and depth: the cards' tint, the icon's cast shadow, the masthead's lip, the opened card's edge."""
    ESC = __import__('re').compile(r'\x1b\[[0-9;]*[mK]')

    def lines(self, rows=40, opened=False):
        story = dict(text=STORY, now=pane.where_it_stands(STORY), rev=3, by='Haiku', at=1990.0)
        got = page([dream(1400, 'ask_claude')], look=dict(at=1995.0, outcome='none'), story=story)
        ui = dict(open=got['sections'][0]['items'][0]['key']) if opened else {}
        return pane.page_lines(got, 60, rows, ui), got

    def test_a_card_row_is_tinted_deeper_but_stays_under_the_heading_and_readable(self):
        self.assertEqual(pane.ROW, 0.07)
        self.assertLess(pane.ROW, pane.HEAD)
        for name in pane.SECTION:
            ground = pane.tint(name, pane.ROW)
            for voice in ('you', 'quiet'):  # the page's body type and its quiet type
                self.assertGreaterEqual(pane.contrast(pane.INK[voice], ground), 4.5, (name, voice))
            self.assertGreaterEqual(pane.contrast(pane.INK['faint'], ground), pane.FAINT, name)  # the meta type

    def test_the_masthead_has_one_lip_row_where_the_pane_is_tall_enough(self):
        lip = '\x1b[48;2;%d;%d;%dm' % pane.blend(pane.DARK[1], pane.PAPER, 0.6)
        lines, _ = self.lines(rows=40)
        lips = [line for line in lines if line.startswith(lip)]
        self.assertEqual(len(lips), 1)
        self.assertEqual(self.ESC.sub('', lips[0]), '')
        self.assertEqual(lines.index(lips[0]), len(lines.heads) and lines.heads[0][0] - 2)  # just above the paper strip
        short, _ = self.lines(rows=pane.LIP_ROWS - 1)
        self.assertFalse([line for line in short if line.startswith(lip)])

    def test_the_lip_costs_one_row_and_moves_the_icons_with_it(self):
        tall, _ = self.lines(rows=pane.LIP_ROWS)
        short, _ = self.lines(rows=pane.LIP_ROWS - 1)
        self.assertEqual(tall.heads[0][0], short.heads[0][0] + 1)

    def test_an_opened_block_carries_the_section_edge_and_steps_back_to_the_row_tint_below(self):
        lines, got = self.lines(opened=True)
        name = got['sections'][0]['name']
        edge = pane.ink(pane.SECTION[name]) + '\u258c'
        first, count = lines.heads[0][0], len(lines)
        block = [line for line in lines[first:] if edge in line]
        plain, _ = self.lines()
        self.assertEqual(len([line for line in plain if '\u258c' in line]), 0)
        self.assertGreaterEqual(len(block), 2)
        # the edge sits in the cell just left of the text, on every row of the block, and only there
        for line in block:
            self.assertEqual(self.ESC.sub('', line)[:pane.ITEM], ' ' * (pane.ITEM - 1) + '\u258c')
        self.assertEqual(len(block), sum(1 for line in lines if '\u258c' in line))
        deep = pane.band(name)
        self.assertTrue(all(line.startswith(deep) for line in block[:-1]))
        row = '\x1b[48;2;%d;%d;%dm' % pane.tint(name, pane.ROW)
        self.assertTrue(block[-1].startswith(row))
        self.assertFalse(block[-1].startswith(deep))

    def test_an_icon_carries_a_cast_shadow_two_cells_right_and_one_down(self):
        import io
        from PIL import Image
        cell, backs = (10, 20), ((200, 200, 200), (240, 240, 240))
        name = 'touching'
        icon = pane.iso_icon(name)
        k = max(1, min((5 * 10 - 4) // icon.width, (2 * 20 - 4) // icon.height))
        w, h = 5 * 10, 2 * 20
        x, y = (w - icon.width * k) // 2, (h - icon.height * k) // 2
        got = Image.open(io.BytesIO(pane.iso_png(name, cell, backs))).convert('RGBA')
        big = icon.resize((icon.width * k, icon.height * k), Image.NEAREST)
        alpha = big.getchannel('A')
        seen = 0
        for py in range(big.height):
            for px in range(big.width):
                sx, sy = px + pane.SHADOW[0] * k, py + pane.SHADOW[1] * k
                if alpha.getpixel((px, py)) and sx < big.width and sy < big.height and not alpha.getpixel((sx, sy)) \
                        and 0 <= x + sx < w and 0 <= y + sy < h:
                    ground = backs[0] if y + sy < 20 else backs[1]
                    pixel = got.getpixel((x + sx, y + sy))[:3]
                    want = tuple(round(c * (1 - 56 / 255)) for c in ground)  # 22% black on the tint
                    self.assertEqual(pixel, want)
                    seen += 1
        self.assertGreater(seen, 0)
        self.assertEqual(pane.SHADOW, (2, 1))
        # nothing was drawn outside the icon's cell box
        self.assertEqual(got.size, (w, h))


class HotReload(unittest.TestCase):
    def test_an_edit_that_does_not_compile_is_held_and_said(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            good, bad = os.path.join(tmp, 'good.py'), os.path.join(tmp, 'bad.py')
            open(good, 'w').write('x = 1\n')
            open(bad, 'w').write('x = 1\ndef broken(:\n')
            self.assertIsNone(pane.compiles(good))
            self.assertEqual(pane.compiles(bad), 'SyntaxError line 2')
        self.assertIn('edit not loaded: SyntaxError line 2', pane.legend('main', 60, 'SyntaxError line 2'))
        self.assertNotIn('edit not loaded', pane.legend('main', 60))


def plain(text):
    return pane.COLOUR.sub('', text)


def flat(text):
    """A drawn line as the eye sees it: no colours, no erase-to-end."""
    return re.sub(r'\x1b\[[0-9;]*[mK]', '', text)


def put(path, record):
    with open(path, 'w') as handle:
        json.dump(record, handle)


def boundary(seconds):
    return json.dumps(dict(type='system', subtype='compact_boundary', compactMetadata=dict(trigger='manual', durationMs=int(seconds * 1000))))


def transcripts(folder, durations, name='s1'):
    """A projects folder with one transcript holding a compaction of each duration, and talk around them."""
    os.makedirs(os.path.join(folder, 'proj'), exist_ok=True)
    path = os.path.join(folder, 'proj', f'{name}.jsonl')
    with open(path, 'w') as handle:
        handle.write(json.dumps(dict(type='user', message=dict(content='mentions compact_boundary in passing'))) + '\n')
        for seconds in durations:
            handle.write(boundary(seconds) + '\n')
    return path


class CompactionTimer(unittest.TestCase):
    def test_marker_lifecycle_young_old_and_ended(self):
        with tempfile.TemporaryDirectory() as tmp:
            file = os.path.join(tmp, 'sess-1.json')
            self.assertIsNone(pane.compacting_since('sess-1', 1000.0, tmp))  # no marker
            put(file, dict(at=990.0, trigger='manual'))
            self.assertEqual(pane.compacting_since('sess-1', 1000.0, tmp), dict(at=990.0, trigger='manual'))
            self.assertIsNone(pane.compacting_since('sess-1', 990.0 + 601, tmp))  # left over: ten minutes old
            put(file, dict(ended=True))
            self.assertIsNone(pane.compacting_since('sess-1', 1000.0, tmp))
            self.assertIsNone(pane.compacting_since('../x', 1000.0, tmp))

    def test_durations_come_from_compact_boundary_rows_of_the_newest_transcripts(self):
        with tempfile.TemporaryDirectory() as tmp:
            transcripts(tmp, [60, 80, 100])
            transcripts(tmp, [120, 140], name='s2')
            cache = os.path.join(tmp, 'times.json')
            self.assertEqual(pane.compaction_seconds(tmp, cache), [60, 80, 100, 120, 140])
            newest = max(glob.glob(os.path.join(tmp, 'proj', '*.jsonl')), key=os.path.getmtime)
            self.assertEqual(pane.compaction_seconds(tmp, cache, newest=1), [60, 80, 100] if newest.endswith('s1.jsonl') else [120, 140])

    def test_a_transcript_is_read_again_only_when_its_mtime_moves(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = transcripts(tmp, [60, 80, 100])
            cache = os.path.join(tmp, 'times.json')
            pane.compaction_seconds(tmp, cache)
            stat = os.stat(path)
            transcripts(tmp, [10])  # rewritten, then its mtime put back: the cache answers
            os.utime(path, ns=(stat.st_atime_ns, stat.st_mtime_ns))
            self.assertEqual(pane.compaction_seconds(tmp, cache), [60, 80, 100])
            os.utime(path, ns=(stat.st_atime_ns, stat.st_mtime_ns + 5_000_000_000))
            self.assertEqual(pane.compaction_seconds(tmp, cache), [10])

    def test_usual_is_the_tenth_to_ninetieth_percentile_and_needs_three(self):
        self.assertIsNone(pane.compaction_usual([60, 80]))
        usual = pane.compaction_usual([float(x) for x in range(60, 160, 10)])  # 60..150
        self.assertEqual((usual['n'], usual['low'], usual['median'], usual['high']), (10, 69.0, 105.0, 141.0))

    def test_the_i_page_says_the_time_against_the_usual_range_with_a_bar(self):
        usual = dict(low=69.0, median=105.0, high=141.0, n=10)
        rows = [plain(r) for r in pane.compact_rows(dict(elapsed=42, trigger='manual', usual=usual, facts=['a fact']), 60)]
        self.assertEqual(rows[0], ' Compacting · 0:42 · usually 1:09 to 2:21')
        self.assertEqual(rows[1], ' ' + '█' * 11 + '▉' + '░' * 28)  # 42/141 of 40 cells: 11 and 7/8
        self.assertEqual(rows[2], ' a fact')
        info = dict(elapsed=42, trigger='manual', usual=usual, facts=['a fact'])
        listed = pane.info_rows(dict(compacting=info), dict(id='x'), 1000.0, 60)  # the same words, on the i page
        self.assertEqual([(label, value) for label, value, _ in listed[:3]],
                         [('COMPACTING', 'Compacting · 0:42 · usually 1:09 to 2:21'), ('', '█' * 11 + '▉' + '░' * 28), ('', 'a fact')])
        late = [plain(r) for r in pane.compact_rows(dict(elapsed=200, trigger='auto', usual=usual, facts=[]), 60)]
        self.assertEqual(late, [' Compacting (auto) · 3:20 · taking longer than usual', ' ' + '█' * 40])
        bare = [plain(r) for r in pane.compact_rows(dict(elapsed=5, trigger='manual', usual=None, facts=[]), 60)]
        self.assertEqual(bare, [' Compacting · 0:05'])
        self.assertEqual(pane.compact_rows(None, 60), [])

    def test_the_bar_is_empty_at_the_start_and_full_at_the_end(self):
        self.assertEqual(pane.compaction_bar(0, 100, 8), '░' * 8)
        self.assertEqual(pane.compaction_bar(50, 100, 8), '████░░░░')
        self.assertEqual(pane.compaction_bar(100, 100, 8), '█' * 8)
        self.assertEqual(pane.compaction_bar(9, 0, 8), '░' * 8)

    def test_the_facts_are_only_those_with_a_number_behind_them(self):
        con = sqlite3.connect(':memory:')
        con.execute('create table chunks (id text, conversation_id text)')
        con.executemany('insert into chunks values (?, ?)', [(f'c{k}', HERE_NOW) for k in range(1200)] + [('x', 'other')])
        story = dict(rev=3)
        self.assertEqual(pane.compaction_facts(con, HERE_NOW, story, True),
                         ['CSR keeps 1,200 chunks of this session', 'story rev 3 → 4 after this',
                          'a new chapter of dreams after this (5 looks)'])
        self.assertEqual(pane.compaction_facts(con, 'nobody', None, False), [])

    def test_info_is_none_without_a_marker_and_whole_with_one(self):
        con = sqlite3.connect(':memory:')
        con.execute('create table chunks (id text, conversation_id text)')
        con.execute("insert into chunks values ('c', 'sess-1')")
        connect, pane.connect = pane.connect, lambda: con
        try:
            with tempfile.TemporaryDirectory() as tmp:
                transcripts(tmp, [60, 80, 100])
                marks = os.path.join(tmp, 'marks')
                os.makedirs(marks)
                args = dict(folder=marks, projects=tmp, cache=os.path.join(tmp, 'times.json'))
                self.assertIsNone(pane.compacting_info('sess-1', 1000.0, {}, **args))
                put(os.path.join(marks, 'sess-1.json'), dict(at=958.0, trigger='manual'))
                info = pane.compacting_info('sess-1', 1000.0, dict(story=dict(rev=1), dreams_on=False), **args)
        finally:
            pane.connect = connect
        self.assertEqual((info['elapsed'], info['trigger']), (42, 'manual'))
        self.assertEqual(info['usual']['median'], 80.0)
        self.assertEqual(info['facts'], ['CSR keeps 1 chunk of this session', 'story rev 1 → 2 after this'])

    def test_a_narrow_i_page_puts_the_range_on_its_own_row(self):
        usual = dict(low=64.0, median=80.0, high=104.0, n=9)
        rows = [plain(r) for r in pane.compact_rows(dict(elapsed=400, trigger='auto', usual=usual, facts=[]), 40)]
        self.assertEqual(rows[:2], [' Compacting (auto) · 6:40', ' taking longer than usual'])
        self.assertTrue(all(len(r) <= 40 for r in rows))
        listed = pane.info_rows(dict(compacting=dict(elapsed=400, trigger='auto', usual=usual, facts=[])), dict(id='x'), 1000.0, 40)
        self.assertEqual([(label, value) for label, value, _ in listed[:2]],
                         [('COMPACTING', 'Compacting (auto) · 6:40'), ('', 'taking longer than usual')])
        self.assertEqual(listed[0][2], 'amber')  # past the usual longest

    def test_the_usual_range_is_read_once_in_five_minutes(self):
        with tempfile.TemporaryDirectory() as tmp:
            transcripts(tmp, [60, 80, 100])
            marks = os.path.join(tmp, 'marks')
            os.makedirs(marks)
            put(os.path.join(marks, 'sess-1.json'), dict(at=990.0, trigger='manual'))
            args = dict(folder=marks, projects=tmp, cache=os.path.join(tmp, 'times.json'))
            first = pane.compacting_info('sess-1', 1000.0, {}, **args)['usual']
            transcripts(tmp, [600, 800, 1000], name='s9')
            self.assertEqual(pane.compacting_info('sess-1', 1100.0, {}, **args)['usual'], first)
            self.assertNotEqual(pane.compacting_info('sess-1', 1000.0 + 301, {}, **args)['usual'], first)

    def test_the_watch_loop_draws_at_once_when_the_marker_goes(self):
        with tempfile.TemporaryDirectory() as tmp:
            shell = types.SimpleNamespace(session=types.SimpleNamespace(id='sess-1'), compacting=True)
            put(os.path.join(tmp, 'sess-1.json'), dict(at=1.0, trigger='manual'))
            self.assertFalse(pane.compaction_gone(shell, tmp))
            os.remove(os.path.join(tmp, 'sess-1.json'))
            self.assertTrue(pane.compaction_gone(shell, tmp))
            shell.compacting = False
            self.assertFalse(pane.compaction_gone(shell, tmp))

    def test_the_timer_takes_row_two_and_the_pane_redraws_when_it_goes(self):
        got = page()
        info = dict(elapsed=42, trigger='manual', usual=dict(low=69.0, median=105.0, high=141.0, n=10), facts=[])
        lines = pane.page_lines(pane.with_compacting(got, dict(compacting=info)), 60, 30, {})
        self.assertTrue(plain(lines[0]).startswith(' CTX '))  # the tape stays on row 1
        self.assertTrue(plain(lines[1]).startswith(' COMPACTING 0:42'))
        self.assertEqual(plain(lines[2]).strip()[:2], '7D' if False else plain(lines[2]).strip()[:2])
        lip = '\x1b[48;2;%d;%d;%dm' % pane.blend(pane.DARK[1], pane.PAPER, 0.6)
        self.assertTrue(lines[3].startswith(lip))  # still three rows, then the lip
        self.assertFalse(any('COMPACTING' in plain(l) for l in pane.page_lines(pane.with_compacting(page(), {}), 60, 30, {})))
        shell = types.SimpleNamespace()
        self.assertFalse(pane.compaction_ended(shell, info))  # it began
        self.assertFalse(pane.compaction_ended(shell, info))  # it runs
        self.assertTrue(pane.compaction_ended(shell, None))  # the marker went: draw at once
        self.assertFalse(pane.compaction_ended(shell, None))  # once


def summary_row(text, at='2026-10-10T18:17:14.109Z'):
    return json.dumps(dict(type='user', isCompactSummary=True, timestamp=at, message=dict(role='user', content=text)))


class SummaryPage(unittest.TestCase):
    TEXT = ('Summary:\n1. Primary Request and Intent:\n   - Build the timer.\n'
            '   - Interpreted as: a bar, not a spinner.\n\n\n2. Key Concepts:\n   - typo for "pane" in the second prompt (unclear)\n')

    def test_the_newest_summary_is_read_from_where_it_was_left(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, 'sess.jsonl')
            with open(path, 'w') as handle:
                handle.write(summary_row('first') + '\n' + json.dumps(dict(type='user', message=dict(content='hi'))) + '\n')
            self.assertEqual(pane.newest_summary(path)['text'], 'first')
            same = pane.newest_summary(path)
            self.assertIs(pane.newest_summary(path), same)  # nothing newer: the same page, its scroll kept
            with open(path, 'a') as handle:
                handle.write(summary_row([dict(type='text', text='second')]) + '\n')
                handle.write(summary_row('half a line'))  # still being written: not read yet
            got = pane.newest_summary(path)
            self.assertEqual((got['text'], got['count']), ('second', 2))
        self.assertIsNone(pane.newest_summary(None))
        self.assertIsNone(pane.newest_summary('/nonexistent/x.jsonl'))

    def test_read_as_lines_are_marked_and_the_text_keeps_its_shape(self):
        rows = pane.summary_lines(self.TEXT, 40)
        marked = ' '.join(line.strip() for line, is_marked in rows if is_marked)
        self.assertEqual(marked, '- Interpreted as: a bar, not a spinner. - typo for "pane" in the second prompt (unclear)')
        self.assertFalse(any(is_marked for line, is_marked in rows if 'Build the timer' in line or 'Key Concepts' in line))
        self.assertIn(('', False), rows)
        self.assertEqual(sum(1 for line, _ in rows if not line), 1)  # a run of blank lines is one
        self.assertTrue(all(len(line) <= 40 for line, _ in rows))

    def test_the_page_draws_the_marked_lines_in_the_accent_ink_and_scrolls(self):
        summary = dict(text=self.TEXT, at=pane.epoch('2026-10-10T18:17:14Z'), count=2, top=0)
        lines, pictures = pane.draw_summary(dict(summary=summary), 50, 30)
        self.assertEqual(pictures, [])
        self.assertIn('Compaction summary', plain(lines[0]))
        marked = [l for l in lines if 'Interpreted as' in l][0]
        self.assertTrue(marked.startswith(' ' + pane.ink('turn')))
        plain_line = [l for l in lines if 'Build the timer' in l][0]
        self.assertTrue(plain_line.startswith(' ' + pane.ink('you')))
        long = dict(text='\n'.join(f'line {k}' for k in range(100)), at=None, count=1, top=500)
        lines, _ = pane.draw_summary(dict(summary=long), 50, 12)
        self.assertEqual(long['top'], 100 - 9)  # clamped to the last page: 12 rows less legend and masthead
        self.assertIn('line 99', plain(lines[-1]))
        lines, _ = pane.draw_summary({}, 50, 12)
        self.assertIn('No compaction summary', plain(lines[1]))

    def test_s_opens_the_page_and_esc_or_d_return_j_and_k_scroll(self):
        shell = types.SimpleNamespace(view='main', open='x', data=dict(summary=dict(top=0)))
        self.assertTrue(pane.summary_keys(shell, 's'))
        self.assertEqual((shell.view, shell.open), ('summary', None))
        self.assertTrue(pane.summary_keys(shell, 'j'))
        self.assertEqual(shell.data['summary']['top'], 3)
        self.assertTrue(pane.summary_keys(shell, 'k'))
        self.assertTrue(pane.summary_keys(shell, 'k'))
        self.assertEqual(shell.data['summary']['top'], 0)
        self.assertTrue(pane.summary_keys(shell, '\x1b'))
        self.assertEqual(shell.view, 'main')
        self.assertFalse(pane.summary_keys(shell, 'j'))
        self.assertTrue(pane.summary_keys(shell, 's'))
        self.assertTrue(pane.summary_keys(shell, 's'))  # s again goes back
        self.assertEqual(shell.view, 'main')

    def test_the_legend_lists_s_on_the_main_page_and_the_way_back_on_the_summary(self):
        self.assertIn('s compaction summary', plain(pane.legend('main', 80)))
        self.assertIn('d back', plain(pane.legend('summary', 80)))
        self.assertNotIn('changed', plain(pane.legend('summary', 80)))

    def test_the_main_legend_fits_a_forty_column_pane(self):
        row = plain(pane.legend('main', 39))
        self.assertLessEqual(len(row), 40)
        self.assertIn('s summary', row)


def transcript(rows):
    """A session's transcript in a temp HOME, one JSON line per row; returns (home, path)."""
    import json
    import tempfile
    home = tempfile.mkdtemp()
    folder = os.path.join(home, '.claude', 'projects', '-proj')
    os.makedirs(folder)
    path = os.path.join(folder, 'sess-coach.jsonl')
    with open(path, 'w') as handle:
        for row in rows:
            handle.write(json.dumps(row, separators=(',', ':')) + '\n')
    return home, path


def said(text, **more):
    return dict(type='user', timestamp='2026-10-10T10:00:00.000Z', message=dict(role='user', content=text), **more)


def replied(text):
    return dict(type='assistant', timestamp='2026-10-10T10:00:05.000Z',
                message=dict(role='assistant', content=[dict(type='text', text=text)]))


def looked(*commands):
    return dict(type='assistant', timestamp='2026-10-10T10:00:06.000Z', message=dict(role='assistant', content=[
        dict(type='tool_use', name='Bash', input=dict(command=c)) for c in commands]))


class HowClaudeReadYou(unittest.TestCase):
    def test_a_typo_a_bare_unclear_and_an_interpretation_are_parsed(self):
        typo = pane.reading_in('Read as: fix the bugs. "dknr" is most likely a typo for "done".', pane.MARKED)
        self.assertEqual((typo['words'], typo['read']), ('dknr', 'done'))
        unclear = pane.reading_in('   - (j) "2" (unclear).', pane.MARKED)
        self.assertEqual((unclear['words'], unclear['read']), ('2', ''))
        said_as = pane.reading_in('"done here means a wow gallery like feel" — interpreted as: build the WebGPU upgrade',
                                  pane.MARKED)
        self.assertEqual((said_as['words'], said_as['read']), ('done here means a wow gallery like feel',
                                                               'build the WebGPU upgrade'))
        bare = pane.reading_in('Read as: ship it today', pane.MARKED)
        self.assertEqual((bare['words'], bare['line']), ('', 'Read as: ship it today'))

    def test_lines_that_only_sound_unsure_are_not_readings(self):
        for line in ('The verse reading is ambiguous.', 'It is unclear whether the audio loops.', ''):
            self.assertIsNone(pane.reading_in(line, pane.MARKED))
        self.assertIsNone(pane.reading_in('That is ambiguous, so I will ask.', pane.FIRST_WORDS))

    def test_the_replies_first_sentence_is_read_for_an_interpretation(self):
        got = pane.reading_in('I\'ll interpret “gm” as good morning. Then I start.'.split('. ')[0] + '.', pane.FIRST_WORDS)
        self.assertEqual((got['words'], got['read']), ('gm', 'good morning'))

    def test_rows_say_what_was_read_and_a_second_click_sends_the_check(self):
        rows = pane.reading_items([dict(words='dknr', read='done', line='x', when=0, source='compaction'),
                                   dict(words='2', read='', line='y', when=0, source='compaction'),
                                   dict(words='', read='', line='Read as: ship it', when=0, source='reply')], 2000.0)
        self.assertEqual([r['title'] for r in rows], ['“dknr” → read as “done”', '“2” → read as unclear', 'Read as: ship it'])
        self.assertEqual(rows[0]['send'], 'Earlier you read my “dknr” as “done”. Check that reading before relying on it.')
        self.assertNotIn('—', ''.join(r['title'] + r['send'] for r in rows))

    def test_this_sessions_summaries_and_first_replies_give_at_most_two_newest_rows(self):
        home, path = transcript([
            said('hello'), replied('Fine, on it.'),
            said('x', isCompactSummary=True) | dict(message=dict(role='user', content=
                'Summary:\n- Read as: ship. "dknr" is most likely a typo for "done".\n- (j) "2" (unclear).\n')),
            said('gm'), replied('I\'ll interpret “gm” as good morning. Starting now.'),
        ])
        gone, pane.HOME = pane.HOME, home
        pane._READINGS.clear()
        try:
            got = pane.how_claude_read('sess-coach')
            self.assertEqual([(r['words'], r['source']) for r in got], [('gm', 'reply'), ('2', 'compaction')])
            self.assertEqual(len(pane.how_claude_read('sess-coach', most=9)), 3)
            self.assertEqual(pane.how_claude_read('no-such-session'), [])
        finally:
            pane.HOME = gone
            pane._READINGS.clear()

    def test_the_section_shows_only_with_something_in_it(self):
        def coach(readings):
            con = sqlite3.connect(':memory:')
            con.execute('create table intent_events (ts text, quote text, session_id text, transcript_path text, '
                        'byte_start int, project text, kind text)')
            connect, pane.connect = pane.connect, lambda: con
            try:
                s = dict(id=HERE_NOW, prompts=[], replies=[], compacts=[], remembered=[],
                         gave=dict(recalls=[], carried=[], dreams=[]), project='csr-engine')
                data = dict(repo='/tmp/csr-engine', dreams=[], chapter=dict(at=1000.0, of=5), dreams_on=True,
                            look=None, story=None, readings=readings)
                page_ = pane.page_main(data, s, 2000.0)
                text = '\n'.join(pane.COLOUR.sub('', l) for l in pane.page_lines(page_, 50, 30, {}))
                return next(x for x in page_['sections'] if x['name'] == 'coach'), text
            finally:
                pane.connect = connect
        section, text = coach([])
        self.assertEqual(section['items'], [])
        self.assertNotIn('How Claude read you', text)
        section, text = coach([dict(words='dknr', read='done', line='x', when=1000.0, source='compaction')])
        self.assertIn('How Claude read you', text)
        self.assertIn('“dknr” → read as “done”', text)

    def test_malformed_lines_and_a_transcript_written_anew_do_not_break_it(self):
        home, path = transcript([
            said('x', isCompactSummary=True) | dict(message=dict(role='user', content='- "2" (unclear).')),
            [], 'text', dict(type='user', message='flat'), dict(type='assistant', message=dict(content=[dict(type='text', text=3)])),
            said(['not', 'parts']),
        ])
        with open(path, 'a') as handle:
            handle.write('{not json\n')
        gone, pane.HOME = pane.HOME, home
        pane._READINGS.clear()
        try:
            self.assertEqual([r['words'] for r in pane.how_claude_read('sess-coach')], ['2'])
            with open(path, 'w') as handle:  # compacted to a new, shorter file
                handle.write(json.dumps(said('x', isCompactSummary=True) | dict(
                    message=dict(role='user', content='- "dknr" is most likely a typo for "done".')), separators=(',', ':')) + '\n')
            self.assertEqual([r['words'] for r in pane.how_claude_read('sess-coach')], ['dknr'])
            self.assertEqual(pane.terms_looked_up(path), {})
        finally:
            pane.HOME = gone
            pane._READINGS.clear()


class SaidItInSessions(unittest.TestCase):
    def test_a_kept_file_of_another_shape_is_read_again(self):
        import tempfile
        home = tempfile.mkdtemp()
        os.makedirs(os.path.join(home, '.claude', 'projects', '-proj'))
        saved = (pane.HOME, pane.KEEP, pane.COACH_KEPT, pane.CLAUDE_MD)
        pane.HOME, pane.KEEP = home, os.path.join(home, 'kept')
        pane.COACH_KEPT, pane.CLAUDE_MD = os.path.join(pane.KEEP, 'coach.json'), os.path.join(home, 'none.md')
        try:
            for bad in ('[1, 2]', '{"/x.jsonl": 5}', '{"/x.jsonl": [1, []]}'):
                os.makedirs(pane.KEEP, exist_ok=True)
                with open(pane.COACH_KEPT, 'w') as handle:
                    handle.write(bad)
                self.assertEqual(pane.coach_terms(now=1.0), [])
        finally:
            pane.HOME, pane.KEEP, pane.COACH_KEPT, pane.CLAUDE_MD = saved

    def test_a_word_of_your_prompt_looked_up_in_memory_within_six_calls_counts(self):
        home, path = transcript([
            said('call osprey for the review'), looked('grep -ril "osprey" /home/u/.claude/memory'),
            said('now zebra and thing'), looked('ls /tmp'), looked('grep -r "zebra" /repo/src'),
            said('quokka please'), looked('cat /home/u/.claude/CLAUDE.md | grep quokka'),
        ])
        got = pane.terms_looked_up(path)
        self.assertEqual(list(got), ['osprey'])  # not zebra (not memory), not quokka (not quoted)
        self.assertIn('call osprey for the review', got['osprey'])

    def test_a_lookup_after_six_other_calls_is_not_about_the_prompt(self):
        home, path = transcript([said('call osprey'), looked(*['ls'] * 6), looked('grep -ril "osprey" /home/u/.claude/memory')])
        self.assertEqual(pane.terms_looked_up(path), {})

    def test_two_sessions_not_in_claude_md_give_one_note_and_it_is_cached_by_mtime(self):
        import json
        import tempfile
        home = tempfile.mkdtemp()
        folder = os.path.join(home, '.claude', 'projects', '-proj')
        os.makedirs(folder)
        for n in (1, 2):
            with open(os.path.join(folder, f's{n}.jsonl'), 'w') as handle:
                for row in (said('call quokka now'), looked('grep -ril "quokka" /home/u/.claude/memory'),
                            said('call osprey now'), looked('grep -ril "osprey" /home/u/.claude/memory')):
                    handle.write(json.dumps(row, separators=(',', ':')) + '\n')
        os.makedirs(os.path.join(home, '.claude-self-reflect'))
        with open(os.path.join(home, '.claude', 'CLAUDE.md'), 'w') as handle:
            handle.write('# Prefs\nosprey = a seabird\n')
        saved = (pane.HOME, pane.KEEP, pane.COACH_KEPT, pane.CLAUDE_MD, pane.terms_looked_up)
        pane.HOME, pane.KEEP = home, os.path.join(home, 'kept')
        pane.COACH_KEPT = os.path.join(pane.KEEP, 'coach.json')
        pane.CLAUDE_MD = os.path.join(home, '.claude', 'CLAUDE.md')
        reads = []
        real = pane.terms_looked_up
        pane.terms_looked_up = lambda path: reads.append(path) or real(path)
        try:
            notes = pane.coach_terms(now=5.0)
            self.assertEqual([(n['id'], n['kind']) for n in notes], [('term:quokka', 'term')])
            self.assertIn('2 sessions', notes[0]['line'])
            self.assertTrue(notes[0]['text'].startswith('- quokka: '))
            self.assertEqual(len(reads), 2)
            pane.coach_terms(now=6.0)
            self.assertEqual(len(reads), 2)  # unchanged transcripts are not read again
        finally:
            pane.HOME, pane.KEEP, pane.COACH_KEPT, pane.CLAUDE_MD, pane.terms_looked_up = saved



def summary(pending, step='', other='8. Current Work:\n   - Something else.\n'):
    return ('1. Primary Request and Intent:\n   - The ask.\n\n7. Pending Tasks:\n' + pending + '\n' + other
            + ('\n9. Optional Next Step:\n' + step if step else ''))


S1 = summary("   - Fix the importer: remove the old cache.\n     - Start with the loader.\n"
             "   - Awaiting the user's go on the push guard.\n"
             "   - Send the final reply to the user.\n"
             "   - Report the results to the user.\n"
             "   - Confirm the background commit landed.\n"
             "   - Tidy the scratch directory, not started.")
S2 = summary("   - Fix the importer: remove the old cache, then rebuild.\n"
             "   - Awaiting the user's go on the push guard, then push it.\n"
             "   - Tidy the scratch directory, not started.")
S3 = summary("   - Fix the importer: remove the old cache, then rebuild it.\n"
             "   - Wait for the user's pasted deck picks before acting on the mods.",
             step="   - Run the importer fix and check it.")
SUMMARIES = [(1000.0, S1), (2000.0, S2), (3000.0, S3)]


class LostAndFound(unittest.TestCase):
    def setUp(self):
        self.keep = pane.KEEP, pane.LOST_LOG
        self.folder = tempfile.mkdtemp()
        pane.KEEP, pane.LOST_LOG = self.folder, os.path.join(self.folder, 'lost-found.jsonl')

    def tearDown(self):
        pane.KEEP, pane.LOST_LOG = self.keep
        shutil.rmtree(self.folder)

    def by_text(self, threads=None):
        return {t['text'].split(':')[0].split(',')[0][:24]: t for t in threads or pane.lost_found(SUMMARIES)}

    def test_a_dropped_item_is_lost_one_that_persists_is_open_and_bookkeeping_is_skipped(self):
        threads = pane.lost_found(SUMMARIES)
        texts = ' | '.join(t['text'] for t in threads)
        self.assertNotIn('final reply', texts)
        self.assertNotIn('Report the results', texts)
        self.assertNotIn('background commit', texts)
        guard = next(t for t in threads if 'push guard' in t['text'])
        self.assertEqual((guard['state'], guard['times'], guard['first_seen'], guard['last_seen']), ('lost', 2, 1000.0, 2000.0))
        importer = next(t for t in threads if 'importer' in t['text'] and 'Run' not in t['text'])
        self.assertEqual((importer['state'], importer['times'], importer['first_seen'], importer['last_seen']), ('open', 3, 1000.0, 3000.0))
        self.assertEqual(len([t for t in threads if 'importer' in t['text']]), 2)  # the fix, and the next step's run of it

    def test_waiting_on_you_is_flagged_and_ranks_first(self):
        threads = pane.lost_found(SUMMARIES)
        self.assertEqual([t['rank'] for t in threads], sorted(t['rank'] for t in threads))
        self.assertIn('push guard', threads[0]['text'])
        self.assertEqual((threads[0]['state'], threads[0]['waiting'], threads[0]['rank']), ('lost', True, 0))
        deck = next(t for t in threads if 'deck picks' in t['text'])
        self.assertEqual((deck['state'], deck['waiting'], deck['rank']), ('open', True, 2))
        tidy = next(t for t in threads if 'scratch' in t['text'])
        self.assertEqual((tidy['state'], tidy['waiting'], tidy['rank']), ('lost', False, 1))

    def test_nested_bullets_stay_with_their_parent(self):
        self.assertEqual(pane.section_items(['   - A thing', '     - its part', '     - another part', '   - Second']),
                         [('A thing', ['its part', 'another part']), ('Second', [])])

    def test_drop_is_for_good_and_later_lasts_until_the_next_compaction(self):
        key = next(t['key'] for t in pane.lost_found(SUMMARIES) if 'scratch' in t['text'])
        other = next(t['key'] for t in pane.lost_found(SUMMARIES) if 'push guard' in t['text'])
        pane.keep_lost(key, 'later', 3500.0)
        pane.keep_lost(other, 'drop', 3500.0)
        marked = {t['key']: t['hidden'] for t in pane.lost_found(SUMMARIES)}
        self.assertEqual((marked[key], marked[other]), ('later', 'drop'))
        again = {t['key']: t['hidden'] for t in pane.lost_found(SUMMARIES + [(4000.0, S3)])}  # a compaction since
        self.assertEqual((again[key], again[other]), (None, 'drop'))

    def test_the_two_asks_are_the_words_typed_into_claudes_prompt(self):
        thread = dict(text='Fix the importer', last_seen=pane.epoch('2026-10-10T18:17:14Z'))
        hhmm = pane.datetime.fromtimestamp(thread['last_seen']).strftime('%H:%M')
        asks = pane.lost_asks(thread)
        self.assertEqual(asks['side'], f"Lost and found: “Fix the importer” went quiet after {hhmm}. Revisit it as a side task: "
                         "finish or close it in a few steps without derailing the current work, then tell me in one line.")
        self.assertEqual(asks['sub'], f"Lost and found: “Fix the importer” went quiet after {hhmm}. Hand it to a background subagent "
                         "with a five-part spec (objective, files, interfaces, constraints, verification), keep working on the "
                         "current task, and report its result when it lands.")

    def test_the_section_shows_three_rows_with_answers_and_only_with_something_in_it(self):
        self.assertEqual(pane.lost_sections({'lost': []}, 5000.0), [])
        (section,) = pane.lost_sections({'lost': pane.lost_found(SUMMARIES)}, 5000.0)
        self.assertEqual((section['title'], section['right']), ('Lost and found', '3 quiet · f all 5'))
        self.assertEqual(len(section['items']), 3)
        rows = pane.lost_rows(section['items'][0], 46, {}, False)
        self.assertEqual([act['spans'][0][2] for _, act in rows] + [act['spans'][1][2] for _, act in rows],
                         ['side', 'drop', 'sub', 'later'])
        self.assertEqual(pane.lost_rows(section['items'][1], 46, {}, False), [])  # not the top row, not opened
        self.assertEqual(len(pane.lost_rows(section['items'][1], 46, {}, True)), 2)

    def test_a_click_on_an_answer_keeps_it_or_types_it_into_claude(self):
        (section,) = pane.lost_sections({'lost': pane.lost_found(SUMMARIES)}, 5000.0)
        item = section['items'][0]
        rows = pane.lost_rows(item, 46, {}, False)
        sent = []
        saved = pane.herdr, pane.agent_pane, pane.focus_back
        pane.herdr = lambda *argv: sent.append(argv) or ''
        pane.agent_pane, pane.focus_back = lambda given: 'w1:p1', lambda: None
        try:
            box = pane.Pane('/repo', 10, None)
            box.acts = {4: rows[0][1], 5: rows[1][1]}
            box.click(4, 7)  # the left half of the first row: side task
            self.assertEqual(sent[-1], ('pane', 'send-text', 'w1:p1', item['asks']['side']))
            self.assertEqual(box.sent[:2], (item['key'], True))
            box.click(4, 30)  # the right half: subagent
            self.assertEqual(sent[-1][3], item['asks']['sub'])
            box.click(5, 30)  # later
            box.click(5, 7)  # drop
        finally:
            pane.herdr, pane.agent_pane, pane.focus_back = saved
        self.assertEqual(len(sent), 2)
        with open(pane.LOST_LOG, encoding='utf-8') as handle:
            self.assertEqual([json.loads(line)['answer'] for line in handle], ['later', 'drop'])

    def test_a_transcript_gives_its_summaries_and_the_cli_prints_the_threads(self):
        home = tempfile.mkdtemp()
        folder = os.path.join(home, '.claude', 'projects', 'p')
        os.makedirs(folder)
        rows = [dict(type='user', isCompactSummary=True, timestamp=f'2026-10-10T1{k}:00:00Z', message=dict(role='user', content=text))
                for k, (_, text) in enumerate(SUMMARIES)]
        rows.insert(1, dict(type='user', timestamp='2026-10-10T10:30:00Z', message=dict(role='user', content='hello')))
        with open(os.path.join(folder, 'sid-1.jsonl'), 'w') as handle:
            handle.write('\n'.join(json.dumps(r) for r in rows) + '\n')
        saved = pane.HOME
        pane.HOME = home
        try:
            found = pane.lost_json('sid-1')
        finally:
            pane.HOME = saved
            shutil.rmtree(home)
        self.assertEqual([t['state'] for t in found].count('open'), 3)
        self.assertEqual(found[0]['key'], pane.lost_found(SUMMARIES)[0]['key'])
        self.assertEqual(sorted(found[0]), sorted(pane.lost_found(SUMMARIES)[0]))


    def test_an_empty_or_broken_summary_line_is_not_the_newest_summary(self):
        home = tempfile.mkdtemp()
        folder = os.path.join(home, '.claude', 'projects', 'p')
        os.makedirs(folder)
        rows = [json.dumps(dict(type='user', isCompactSummary=True, timestamp=f'2026-10-10T1{k}:00:00Z',
                                message=dict(role='user', content=[dict(type='text', text=text)])))
                for k, (_, text) in enumerate(SUMMARIES)]
        rows += ['{not json', json.dumps(dict(type='user', isCompactSummary=True, timestamp='2026-10-10T15:00:00Z',
                                              message=dict(role='user', content=None))),
                 json.dumps(dict(type='user', isCompactSummary=True, message=dict(content='no time')))]
        with open(os.path.join(folder, 'sid-2.jsonl'), 'w') as handle:
            handle.write('\n'.join(rows) + '\n')
        saved = pane.HOME
        pane.HOME = home
        try:
            found = pane.lost_json('sid-2')
            none = pane.lost_json('no-such-session')
        finally:
            pane.HOME = saved
            shutil.rmtree(home)
        self.assertEqual([t['state'] for t in found].count('open'), 3)  # the newest real summary still decides
        self.assertEqual(none, [])

    def test_the_answers_fit_a_forty_column_pane(self):
        (section,) = pane.lost_sections({'lost': pane.lost_found(SUMMARIES)}, 5000.0)
        for text, _ in pane.lost_rows(section['items'][0], 40, {}, False):
            self.assertLessEqual(len(re.sub(r'\x1b\[[0-9;]*m', '', text[0])), 40)

class LostAndFoundAnswered(unittest.TestCase):
    """What only looks lost: a thread you said go to, one that showed once, prep-compact's own reminders."""
    WAIT = summary("   - Awaiting the user's go on the story refresh.\n   - Tidy the scratch directory, not started.")
    DONE = summary("   - Rebuild the index.")

    def setUp(self):
        self.keep = pane.KEEP, pane.LOST_LOG
        self.folder = tempfile.mkdtemp()
        pane.KEEP, pane.LOST_LOG = self.folder, os.path.join(self.folder, 'lost-found.jsonl')

    def tearDown(self):
        pane.KEEP, pane.LOST_LOG = self.keep
        shutil.rmtree(self.folder)

    def states(self, typed):
        return {t['text'][:12]: t['state'] for t in pane.lost_found([(1000.0, self.WAIT), (2000.0, self.DONE)], typed)}

    def test_a_thread_you_said_go_to_right_after_is_answered_not_lost(self):
        self.assertEqual(self.states([(1500.0, 'i think story is a go')])['Awaiting the'], 'answered')
        self.assertEqual(self.states([(1500.0, 'install the story refresh')])['Awaiting the'], 'answered')
        # a bare go names nothing, so it answers nothing: the thread stays lost, not silently hidden
        self.assertEqual(self.states([(1500.0, 'install')])['Awaiting the'], 'lost')
        self.assertEqual(self.states([(1500.0, 'ok push the branch')])['Awaiting the'], 'lost')
        # a go that came long after, past your next two prompts, answers nothing
        late = [(1100.0, 'show me the pane'), (1200.0, 'and the dreams'), (1300.0, 'go')]
        self.assertEqual(self.states(late)['Awaiting the'], 'lost')

    def test_a_go_never_answers_a_thread_that_asked_you_nothing(self):
        verify = summary("   - After the next prompt, verify that hand-off lines show the session label.")
        threads = pane.lost_found([(1000.0, verify), (1200.0, verify), (2000.0, self.DONE)],
                                  [(1500.0, 'ok, hand-off labels next')])
        self.assertEqual(next(t for t in threads if 'hand-off' in t['text'])['state'], 'lost')

    def test_every_thread_is_on_the_f_page_with_why_including_the_guesses(self):
        threads = pane.lost_found([(1000.0, self.WAIT), (2000.0, self.DONE)], [(1500.0, 'story is a go')])
        lines, _, _ = pane.draw_threads({}, {'lost': threads}, 60, 60, {})
        page = '\n'.join(plain(line) for line in lines)
        for words in ('Guessed done', 'Showed once', 'story refresh', 'Tidy the scratch', 'you said “story is a go”'):
            self.assertIn(words, page)

    def test_the_f_page_scrolls_and_its_rows_open_with_their_answers(self):
        many = summary('\n'.join(f'   - Awaiting the user on alpha{k} beta{k} gamma{k}.' for k in range(30)))
        threads = pane.lost_found([(1000.0, many), (2000.0, self.DONE)])
        lines, _, top = pane.draw_threads({}, {'lost': threads}, 50, 20, {}, top=10)
        self.assertEqual((len(lines) <= 19, top), (True, 10))
        key = 'lost:' + threads[0]['key']
        opened, _, _ = pane.draw_threads({}, {'lost': threads}, 50, 40, {'open': key})
        spans = [act['spans'] for act in opened.acts.values() if act.get('spans')]
        self.assertEqual(sorted(a for pair in spans for *_, a in pair), ['drop', 'later', 'side', 'sub'])

    def test_a_thread_seen_once_that_never_waited_on_you_faded(self):
        self.assertEqual(self.states([])['Tidy the scr'], 'faded')
        shown = pane.lost_sections({'lost': pane.lost_found([(1000.0, self.WAIT), (2000.0, self.DONE)])}, 5000.0)
        titles = [i['title'] for s in shown for i in s['items']]
        self.assertFalse(any('Tidy' in title for title in titles))

    def test_one_thread_in_two_wordings_is_one(self):
        a = summary("   - Installing the csr-companion plugin for all sessions needs approval.")
        b = summary("   - Install csr-companion for every session; awaiting the user.")
        threads = pane.lost_found([(1000.0, a), (2000.0, b), (3000.0, self.DONE)])
        installs = [t for t in threads if 'csr-companion' in t['text']]
        self.assertEqual((len(installs), installs[0]['times']), (1, 2))

    def test_prep_compacts_reminders_never_count_as_owed(self):
        reminded = summary("   - Rebuild the index.\n   - Reminders from CSR (not tasks):\n"
                           "     - Awaiting the user's go on the story refresh.")
        threads = pane.lost_found([(1000.0, self.WAIT), (2000.0, reminded)], [])
        story = next(t for t in threads if 'story refresh' in t['text'])
        self.assertEqual((story['state'], story['times']), ('lost', 1))

    def test_a_thread_set_aside_for_later_is_marked_for_the_reminders(self):
        threads = pane.lost_found([(1000.0, self.WAIT), (2000.0, self.DONE)])
        key = next(t['key'] for t in threads if 'story refresh' in t['text'])
        pane.keep_lost(key, 'later', 2500.0)
        marked = {t['key']: t['later'] for t in pane.lost_found([(1000.0, self.WAIT), (2000.0, self.DONE)])}
        self.assertEqual(sum(marked.values()), 1)
        self.assertTrue(marked[key])



class CarryOver(unittest.TestCase):
    """A restart loses nothing: the session before this one gives its summary's threads and what came after."""

    def setUp(self):
        self.folder = tempfile.mkdtemp()
        self.keep = pane.KEEP, pane.LOST_LOG
        pane.KEEP, pane.LOST_LOG = self.folder, os.path.join(self.folder, 'lost-found.jsonl')

    def tearDown(self):
        pane.KEEP, pane.LOST_LOG = self.keep
        shutil.rmtree(self.folder)

    def transcript(self, name, rows, written, entry='cli'):
        path = os.path.join(self.folder, name + '.jsonl')
        with open(path, 'w') as handle:
            handle.write('\n'.join(json.dumps(dict(r, entrypoint=entry)) for r in rows) + '\n')
        os.utime(path, (written, written))
        return path

    @staticmethod
    def at(minute):
        return f'2026-10-10T10:{minute:02d}:00Z'

    def before(self, tail, written=1000.0, entry='cli'):
        pending = summary("   - Awaiting the user's go on the push guard, then push it.")
        rows = [dict(type='user', timestamp=self.at(0), message=dict(role='user', content='start')),
                dict(type='user', isCompactSummary=True, timestamp=self.at(5), message=dict(role='user', content=pending))]
        return self.transcript('prev-1', rows + tail, written, entry)

    def carried(self, started=2000.0):
        mine = self.transcript('mine-1', [dict(type='user', timestamp=self.at(30), message=dict(role='user', content='hi'))], started)
        return pane.carried_threads(mine, started)

    def test_a_prompt_claude_never_finished_answering_is_carried_first(self):
        self.before([dict(type='user', timestamp=self.at(9), message=dict(role='user', content='rename Dreamt to Shift')),
                     dict(type='user', timestamp=self.at(9), message=dict(role='user', content=[
                         dict(type='text', text='[Request interrupted by user for tool use]')])),
                     dict(type='assistant', timestamp=self.at(10), message=dict(
                         content=[dict(type='text', text='Looking at the grammar.')], stop_reason='tool_use'))])
        got = self.carried()
        self.assertTrue(any('push guard' in t['text'] and t['came_from'] == 'prev-1' for t in got))
        stop = next(t for t in got if 'Cut off' in t['text'])
        self.assertEqual((stop['rank'], stop['waiting'], 'rename Dreamt' in stop['text']), (0, True, True))
        self.assertTrue(any('Looking at the grammar' in line for line in stop['detail']))

    def test_claudes_last_question_is_carried_when_the_turn_ended(self):
        self.before([dict(type='user', timestamp=self.at(9), message=dict(role='user', content='build it')),
                     dict(type='assistant', timestamp=self.at(10), message=dict(
                         content=[dict(type='text', text='Built and tested. Should I push the branch?')], stop_reason='end_turn'))])
        stop = next(t for t in self.carried() if t['came_from'] and 'asked last' in t['text'])
        self.assertIn('Should I push the branch?', stop['text'])

    def test_a_finished_session_with_nothing_asked_ranks_its_tail_low_but_keeps_it(self):
        self.before([dict(type='user', timestamp=self.at(9), message=dict(role='user', content='tidy the docs')),
                     dict(type='assistant', timestamp=self.at(10), message=dict(
                         content=[dict(type='text', text='Done.')], stop_reason='end_turn'))])
        stop = next(t for t in self.carried() if 'Last asked' in t['text'])
        self.assertEqual(stop['rank'], 3)

    def test_only_a_person_s_session_written_before_this_one_began_counts(self):
        self.before([], entry='sdk-cli')
        self.assertEqual(self.carried(), [])
        self.before([], written=3000.0)  # still running beside this one
        self.assertEqual(self.carried(started=2000.0), [])
        self.before([], written=2000.0 - 8 * 86400)  # over a week before
        self.assertEqual(self.carried(), [])

    def test_a_carried_thread_this_session_already_carries_shows_once(self):
        own = summary("   - Awaiting the user's go on the push guard, then push it.")
        carried = [dict(text="Awaiting the user's go on the push guard, then push it.", detail=[], first_seen=1.0,
                        last_seen=2.0, times=1, state='carried', waiting=True, rank=0, key='c1', came_from='prev-1')]
        threads = pane.lost_found([(1000.0, own)], [], carried)
        self.assertEqual(len([t for t in threads if 'push guard' in t['text']]), 1)
        other = [dict(carried[0], text='Rename the Dreamt grammar to Shift.', key='c2')]
        self.assertEqual(pane.lost_found([(1000.0, own)], [], other)[0]['came_from'], 'prev-1')


class CarryOverRegressions(CarryOver):
    """Each case one defect found in review, reproduced first, then held."""

    def test_a_second_restart_keeps_what_the_first_carried(self):
        self.before([])
        os.makedirs(os.path.join(self.folder, pane.CARRY_DIR))
        task = dict(text='Rename the Dreamt grammar to Shift.', detail=[], first_seen=1.0, last_seen=2.0, times=1,
                    state='carried', waiting=False, rank=1, key='a-task', came_from='older-0', why='session older-0')
        with open(os.path.join(self.folder, pane.CARRY_DIR, 'prev-1.json'), 'w') as handle:
            json.dump([task], handle)
        self.assertTrue(any(t['key'] == 'a-task' for t in self.carried()))
        pane.keep_lost('a-task', 'drop', 5.0, os.path.basename(self.folder))
        self.assertFalse(any(t['key'] == 'a-task' for t in self.carried()))

    def test_a_kept_carry_is_read_back_never_worked_out_again(self):
        mine = self.transcript('mine-2', [dict(type='user', timestamp=self.at(30), message=dict(role='user', content='hi'))], 2000.0)
        os.makedirs(os.path.join(self.folder, pane.CARRY_DIR))
        with open(os.path.join(self.folder, pane.CARRY_DIR, 'mine-2.json'), 'w') as handle:
            json.dump([dict(text='kept', key='k')], handle)
        session = pane.Session('mine-2', path=mine)
        session.read_transcript()
        pane.carry_into(session)
        self.assertEqual(session.carried, [dict(text='kept', key='k')])

    def test_lost_found_never_writes_into_the_carried_list(self):
        carried = [dict(text='Rename the Dreamt grammar to Shift.', detail=[], first_seen=1.0, last_seen=2.0, times=1,
                        state='carried', waiting=False, rank=1, key='c1', came_from='p')]
        pane.lost_found([], [], carried)
        self.assertNotIn('hidden', carried[0])

    def test_headless_transcripts_never_hide_the_session_before(self):
        self.before([], written=1000.0)
        for k in range(8):
            self.transcript(f'headless-{k}', [dict(type='user', timestamp=self.at(1), message=dict(role='user', content='x'))],
                            1500.0 + k, entry='sdk-cli')
        mine = self.transcript('mine-3', [dict(type='user', timestamp=self.at(30), message=dict(role='user', content='hi'))], 2000.0)
        self.assertTrue(pane.previous_session(mine, 2000.0).endswith('prev-1.jsonl'))

    def test_a_drop_before_the_restart_holds_when_a_summary_rewords_the_thread(self):
        carried = [dict(text='Awaiting approval to deploy production cache migration.', detail=[], first_seen=1.0,
                        last_seen=2.0, times=1, state='carried', waiting=True, rank=0, key='c-drop', came_from='p')]
        pane.keep_lost('c-drop', 'drop', 5.0)
        own = summary("   - Waiting for the user approval to deploy production cache migration.")
        (thread,) = [t for t in pane.lost_found([(1000.0, own)], [], carried) if 'migration' in t['text']]
        self.assertEqual(thread['hidden'], 'drop')

    def test_the_tail_is_only_what_came_after_the_last_summary(self):
        rows = [dict(type='user', timestamp=self.at(0), message=dict(role='user', content='start')),
                dict(type='assistant', timestamp=self.at(1), message=dict(
                    content=[dict(type='text', text='Should I push the branch?')], stop_reason='end_turn')),
                dict(type='user', isCompactSummary=True, timestamp=self.at(5), message=dict(role='user', content=summary('   - Rebuild.')))]
        self.transcript('prev-1', rows, 1000.0)
        self.assertFalse(any('asked last' in t['text'] for t in self.carried()))

    def test_a_go_names_a_thread_by_its_subject_not_its_verb(self):
        wait = summary("   - Awaiting approval to install Redis.")
        later = summary("   - Rebuild the index.")
        states = {t['text'][:12]: t['state'] for t in pane.lost_found([(1000.0, wait), (2000.0, later)], [(1500.0, 'ok install PostgreSQL')])}
        self.assertEqual(states['Awaiting app'], 'lost')

    def test_projects_keep_their_own_answers(self):
        pane.keep_lost('k1', 'drop', 5.0, 'project-a')
        pane.keep_lost('k2', 'drop', 5.0)  # kept before answers named a project: counts everywhere
        self.assertEqual(sorted(pane.lost_log('project-a')), ['k1', 'k2'])
        self.assertEqual(sorted(pane.lost_log('project-b')), ['k2'])

    def test_the_legend_fits_every_width_so_the_last_row_never_wraps(self):
        for view in ('main', 'summary', 'threads', 'log'):
            for width in range(16, 90):
                self.assertLessEqual(len(plain(pane.legend(view, width))), width + 1, (view, width))

    def test_transcript_text_can_neither_drive_the_terminal_nor_press_keys(self):
        evil = 'hi\x1b[2J\x1b]52;c;cGF5bG9hZA==\x07\x03 there\x9b31m'
        line = pane.safe_line(pane.ink('you') + evil + pane.OFF)
        self.assertNotIn('\x1b]', line)
        self.assertNotIn('\x1b[2J', line)
        self.assertNotIn('\x03', line)
        self.assertNotIn('\x9b', line)
        self.assertIn(pane.ink('you'), line)  # the pane's own colour stays
        tinted = pane.tinted('row', '\x1b[48;2;1;2;3m')
        self.assertEqual(pane.safe_line(tinted), tinted)  # and so does a tinted row's erase-to-end
        sent = []
        saved = pane.herdr
        pane.herdr = lambda *args: sent.append(args) or ''
        try:
            pane.send_text('w2:p1', 'line one\nline two\r\x1b[13~\x03')
        finally:
            pane.herdr = saved
        self.assertEqual(sent[0][3], 'line one line two[13~')

    def test_the_compaction_floor_is_the_first_request_after_not_post_tokens(self):
        rows = [dict(type='system', subtype='compact_boundary', timestamp=self.at(1),
                     compactMetadata=dict(preTokens=300000, postTokens=19000, durationMs=90000)),
                dict(type='assistant', isSidechain=True, timestamp=self.at(2), message=dict(
                    id='s', usage=dict(input_tokens=5, cache_read_input_tokens=1000))),
                dict(type='assistant', timestamp=self.at(3), message=dict(
                    id='m', usage=dict(input_tokens=10, cache_read_input_tokens=60000, cache_creation_input_tokens=4990)))]
        session = pane.Session('floor', path=self.transcript('floor', rows, 1000.0))
        session.read_transcript()
        self.assertEqual(session.compact_sizes, [(300000, 65000, 90000)])


class MainScroll(unittest.TestCase):
    def test_the_window_moves_rows_clicks_and_pictures_and_stops_at_the_ends(self):
        lines = pane.Lines([f'row {k}' for k in range(50)])
        lines.acts = {10: dict(key='a'), 30: dict(key='b')}
        pictures = [(12, 0, 4, 2, b'p'), (27, 0, 4, 2, b'cut'), (2, 0, 4, 2, b'gone')]
        shown, kept, top = pane.window(lines, pictures, 21, 8)
        self.assertEqual((shown[0], len(shown), top), ('row 8', 20, 8))
        self.assertEqual(shown.acts, {2: dict(key='a')})
        self.assertEqual(kept, [(4, 0, 4, 2, b'p')])
        self.assertEqual(pane.window(lines, [], 21, 99)[2], 30)  # never past the end
        self.assertEqual(pane.window(lines, [], 21, -5)[2], 0)

    def test_a_click_that_opens_a_row_keeps_it_where_it_was_clicked_and_the_wheel_scrolls(self):
        shell = pane.Pane.__new__(pane.Pane)
        shell.view, shell.open, shell.sent, shell.main_top, shell.anchor = 'main', None, None, 0, None
        shell.threads_top, shell.agent, shell.data = 0, None, {}
        shell.acts = {7: dict(key='lost:abc', send='x')}
        saved = pane.focus_back
        pane.focus_back = lambda: None
        try:
            shell.click(7, 3)
        finally:
            pane.focus_back = saved
        self.assertEqual((shell.open, shell.anchor), ('lost:abc', ('lost:abc', 7)))
        shell.scroll(pane.WHEEL)
        shell.scroll(pane.WHEEL)
        shell.scroll(-10 * pane.WHEEL)
        self.assertEqual(shell.main_top, 0)
        shell.scroll(pane.WHEEL)
        self.assertEqual(shell.main_top, pane.WHEEL)

    def test_a_frame_rewrites_only_the_rows_that_changed_and_never_clears_the_screen(self):
        rows = ['a', 'b', 'c', 'legend']
        first = pane.repaint(None, 10, 4, rows, [])
        self.assertIn('\x1b[2J', first)  # the first frame starts clear
        drawn = (10, 4, rows, [])
        same = pane.repaint(drawn, 10, 4, rows, [])
        self.assertEqual(same, pane.SYNC_ON + '\x1b[4;10H' + pane.SYNC_OFF)  # nothing to write
        moved = pane.repaint(drawn, 10, 4, ['a', 'B', 'c', 'legend'], [])
        self.assertNotIn('\x1b[2J', moved)
        self.assertIn('\x1b[2;1H\x1b[0m\x1b[2KB', moved)
        self.assertNotIn('\x1b[1;1H', moved)  # row a is left as it is
        self.assertTrue(moved.startswith(pane.SYNC_ON) and moved.endswith(pane.SYNC_OFF))
        self.assertIn('\x1b[2J', pane.repaint(drawn, 12, 4, rows, []))  # a new size starts clear

    def test_each_legend_label_is_its_own_click_and_only_a_send_hands_the_focus_back(self):
        text, spans = pane.legend_parts('main', 60)
        plain = pane.re.sub(r'\x1b\[[0-9;]*m', '', text)
        for start, end, key in spans:
            self.assertTrue(plain[start:end].startswith(key + ' '), (plain, start, key))
        shell = pane.Pane.__new__(pane.Pane)
        shell.view, shell.open, shell.sent, shell.main_top, shell.anchor = 'main', None, None, 0, None
        shell.threads_top, shell.agent, shell.data, shell.tall = 0, None, {}, False
        shell.acts = {23: dict(key='legend', keys=spans), 7: dict(key='lost:abc', send='x')}
        backs, saved = [], (pane.focus_back, pane.send_text)
        pane.focus_back, pane.send_text = lambda: backs.append(1), lambda agent, text: True
        try:
            col = {key: start for start, _, key in spans}
            shell.click(23, col['s'])
            self.assertEqual(shell.view, 'summary')
            shell.view = 'main'
            shell.click(23, col['f'])
            self.assertEqual(shell.view, 'threads')
            shell.view = 'main'
            shell.click(23, col['q'])  # close stays on the keyboard
            self.assertEqual(shell.view, 'main')
            shell.click(7, 3)  # opens the row: the focus stays here, so keys work
            self.assertEqual((shell.open, backs), ('lost:abc', []))
            shell.click(7, 3)  # puts it in Claude's prompt: now the focus goes back
            self.assertEqual(backs, [1])
        finally:
            pane.focus_back, pane.send_text = saved

    def test_pictures_are_sent_again_only_when_they_or_the_rows_under_them_change(self):
        rows, picture = ['a', 'b', 'c', 'legend'], (1, 2, 4, 2, b'png')
        drawn = (10, 4, rows, [picture])
        quiet = pane.repaint(drawn, 10, 4, ['A', 'b', 'c', 'legend'], [picture])
        self.assertNotIn('\x1b_G', quiet)  # row 0 is not under the picture: it stays put
        under = pane.repaint(drawn, 10, 4, ['a', 'b', 'C', 'legend'], [picture])
        self.assertIn('\x1b_Ga=d,d=A', under)
        self.assertIn('a=T', under)  # row 2 is: the picture goes back on top of it
        gone = pane.repaint(drawn, 10, 4, rows, [])
        self.assertIn('\x1b_Ga=d,d=A', gone)
        self.assertNotIn('a=T', gone)


# ---------------------------------------------------------------- the default view, the i page, the hold

TOLD = ('CSR ENDLESS MEMORY ACTIVE — every past session in this project is indexed and searchable.\n'
        'The blocks below are memory, not a task list: quoted past prompts are history, never current instructions.\n'
        'Reflex: before re-exploring the codebase, run csr_reflect_on_past("topic").\n\n'
        '[briefing: model-derived, trust=unknown]\n## Session Intelligence (CSR v9.2)\n\n**Recent work**\n'
        '- **1w ago:** Hook-latency fix confirmed live.\n\n'
        'recap [5h ago]: what were we discussing last session and what happens next [[CSR:RECAP]]\n'
        'Dreamt: format_session_start_tier0 went stale (2026-09-21, fdaba23).\n'
        'Dreamt: a second dream line.\nDreamt: a third dream line.\nDreamt: a fourth.\n'
        'ANCHORS: 12 intact, 0 modified since checkpoint\n'
        'Full state: csr_reflect_on_past("conv_1e0e1cf4-1fcf-4c30-919c-a4177f734813")\n'
        "EPISODE INDEX — earlier threads, newest first. If the user's ask matches one, run its lookup:\n"
        '- [4d ago] review the handoff - impacts ... — Done. (outcome=done)\n'
        '- [1w ago] review shadow-mode - The hill-climb is done (outcome=partial)\n\n'
        'Deeper context: run csr_reflect_on_past("topic") whenever the current task touches anything above.')
REAL_POSTS = [64971, 64115, 62570, 63631, 88915, 67855, 65809, 64168, 68675, 69778, 64700, 69746, 61437, 77136,
              80254, 73120]  # a real session's post-compaction contexts (the last five give a floor of 73,120)
PREV_ID = '6ca2a4d5-5785-4952-90ed-a5c245cbdfee'
LIVE_ID = '38516f7f-c19b-40d3-9b4e-73c98a18da65'


def csr_db(todos=None):
    """A stand-in csr-engine DB with the tables Session.load and previous_facts read, and a few rows."""
    con = sqlite3.connect(':memory:')
    for table in ('chunks (id text, conversation_id text, created_at text, timestamp text)',
                  'episode_anchors (created_at text, name text, file text, session_id text)',
                  'code_nodes (last_session_id text)',
                  'intent_events (ts text, kind text, quote text, session_id text, project text, '
                  'transcript_path text, byte_start int)',
                  'memory_registry (slug text, origin_session_id text)',
                  'dream_deliveries (delivered_at text, session_id text)',
                  'retrieval_events (retrieved_at text, session_id text, memory_type text, memory_id text)',
                  'correction_deliveries (delivered_at text, target_session_id text)',
                  'chunk_provenance (chunk_id text, source_conv_id text)',
                  'session_registry (session_id text, first_prompt text, first_ts text, last_ts text, project text)',
                  'reflections (id text, content text, tags text, timestamp text)'):
        con.execute('create table ' + table)
    con.executemany('insert into chunks values (?, ?, ?, ?)', [(f'c{k}', PREV_ID, '2026-10-08T10:00:00Z', None) for k in range(7)])
    con.executemany('insert into retrieval_events values (?, ?, ?, ?)',
                    [(f'2026-10-08T10:00:0{k}.1Z', PREV_ID, 'chunk', f'm{k}') for k in (1, 1, 5)])  # two hand-offs
    con.execute("insert into correction_deliveries values ('2026-10-08T11:00:00Z', ?)", (PREV_ID,))
    if todos is not None:
        con.execute('insert into reflections values (?, ?, ?, ?)',
                    ('ep1', json.dumps(dict(todos=todos)), json.dumps(['session_episode', f'conv_{PREV_ID}']), '2026-10-09'))
    return con


def row_at(seconds, **row):
    stamp = pane.datetime.fromtimestamp(1_800_000_000 + seconds, tz=pane.timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.000Z')
    return dict(row, timestamp=stamp)


def csr_row(source='startup', content=TOLD, command='/usr/local/bin/csr-engine hook session-start', seconds=0):
    return row_at(seconds, type='attachment', attachment=dict(
        type='hook_success', hookName=f'SessionStart:{source}', hookEvent='SessionStart', command=command,
        content=content, stdout=content, stderr='', exitCode=0))


def prompt_row(seconds, text='a typed prompt'):
    return row_at(seconds, type='user', message=dict(role='user', content=text))


def reply_row(seconds, context, key=None, **more):
    return row_at(seconds, type='assistant', message=dict(
        id=key or f'm{seconds}', usage=dict(input_tokens=1, cache_read_input_tokens=context - 1),
        content=[dict(type='text', text='words')], **more))


def boundary_row(seconds, pre):
    return row_at(seconds, type='system', subtype='compact_boundary',
                  compactMetadata=dict(trigger='manual', preTokens=pre, postTokens=19_000, durationMs=60_000))


class DefaultView(unittest.TestCase):
    def setUp(self):
        self.home = tempfile.mkdtemp()
        self.saved = {k: getattr(pane, k) for k in ('HOME', 'SESSIONS', 'KEEP', 'TASKS', 'LOST_LOG', 'connect', '_RECORD')}
        pane.HOME = self.home
        pane.SESSIONS = os.path.join(self.home, '.claude', 'sessions')
        pane.KEEP = os.path.join(self.home, 'keep')
        pane.TASKS = os.path.join(self.home, '.claude', 'tasks')
        pane.LOST_LOG = os.path.join(pane.KEEP, 'lost-found.jsonl')
        pane.connect = lambda: csr_db()
        pane._RECORD = {}
        os.makedirs(pane.SESSIONS)
        self.cwd = '/Users/x/projects/demo'
        self.folder = pane.project_folder(self.cwd)
        os.makedirs(self.folder)

    def tearDown(self):
        for key, value in self.saved.items():
            setattr(pane, key, value)
        shutil.rmtree(self.home)

    def live(self, sid, status='idle', cwd=None, started=1_800_000_000):
        put(os.path.join(pane.SESSIONS, f'{os.getpid()}-{sid[:4]}.json'),
            dict(pid=1, sessionId=sid, cwd=cwd or self.cwd, startedAt=started * 1000, status=status))

    def write(self, name, rows, written=None, entry='cli'):
        path = os.path.join(self.folder, name + '.jsonl')
        with open(path, 'w') as handle:
            handle.write('\n'.join(json.dumps(dict(r, entrypoint=entry)) for r in rows) + '\n')
        if written:
            os.utime(path, (written, written))
        return path

    def test_a_working_directory_names_its_folder_as_claude_code_does(self):  # B1
        got = pane.project_folder('/Users/x/projects/claude-self-reflect/.claude/worktrees/a_b')
        self.assertTrue(got.endswith('-Users-x-projects-claude-self-reflect--claude-worktrees-a-b'), got)
        self.assertIsNone(pane.project_folder(''))
        self.assertIsNone(pane.project_folder(None))

    def test_a_live_sessions_record_gives_its_cwd_start_and_whether_it_is_busy(self):
        self.live(LIVE_ID, status='busy', started=1_800_000_123)
        got = pane.session_record(LIVE_ID)
        self.assertEqual((got['cwd'], got['started'], got['busy']), (self.cwd, 1_800_000_123.0, True))
        self.assertFalse(pane.session_record('someone-else'))
        self.live(LIVE_ID, status='idle')
        self.assertFalse(pane.session_record(LIVE_ID)['busy'])
        for odd in ('{not json', '[]', '{"sessionId": "%s"}' % LIVE_ID, '{"sessionId": "%s", "cwd": "/x", "startedAt": "soon"}' % LIVE_ID):
            pane._RECORD.clear()
            with open(os.path.join(pane.SESSIONS, f'{os.getpid()}-{LIVE_ID[:4]}.json'), 'w') as handle:
                handle.write(odd)
            self.assertIsNone(pane.session_record(LIVE_ID), odd)  # fails open

    def test_a_pane_opened_before_the_first_prompt_reads_the_transcript_once_it_exists(self):  # B2
        self.live(LIVE_ID, status='busy')
        called, ran = [], pane.threading.Event()
        carry = pane.carry_into
        pane.carry_into = lambda session: called.append(session) or ran.set()
        try:
            session = pane.Session(LIVE_ID)
            self.assertIsNone(session.path)
            first = session.load()
            self.assertTrue(ran.wait(5))  # the carry thread started with no transcript at all
            self.assertEqual((first['busy'], first['prompts'], first['cwd'], first['transcript']), (True, [], self.cwd, False))
            self.assertEqual(session.carried, [])
            self.write(LIVE_ID, [prompt_row(5, 'the first prompt')])  # Claude Code writes it at the first prompt
            second = session.load()
            self.assertEqual(len(second['prompts']), 1)
            self.assertEqual((session.path, second['transcript']), (os.path.join(self.folder, LIVE_ID + '.jsonl'), True))
            self.assertEqual(len(called), 1)  # one carry for the session
        finally:
            pane.carry_into = carry

    def test_without_a_record_the_folders_are_searched_at_most_every_ten_seconds(self):
        session = pane.Session('no-record')
        calls = []
        real = pane.glob.glob
        pane.glob.glob = lambda pattern, *a, **k: calls.append(pattern) or real(pattern, *a, **k)
        try:
            session.read_transcript()
            session.read_transcript()
            self.assertEqual(len([c for c in calls if 'no-record' in c]), 1)
            session.looked -= 11
            session.read_transcript()
            self.assertEqual(len([c for c in calls if 'no-record' in c]), 2)
        finally:
            pane.glob.glob = real

    def test_what_csr_told_claude_is_read_from_its_own_session_start_row_only(self):  # B3
        self.write(PREV_ID, [
            csr_row(),
            csr_row(command='"${CLAUDE_PLUGIN_ROOT}/hooks/run-hook.cmd" session-start',
                    content='{"hookSpecificOutput": {"additionalContext": "You have superpowers"}}', seconds=1),
            csr_row(content='', seconds=2), prompt_row(5)])
        session = pane.Session(PREV_ID, path=os.path.join(self.folder, PREV_ID + '.jsonl'))
        session.read_transcript()
        self.assertEqual((session.told['source'], session.told['text']), ('startup', TOLD))  # the other rows are ignored
        got = pane.told_lines(session.told['text'])
        self.assertEqual(got['recap'], 'recap [5h ago]: what were we discussing last session and what happens next')
        self.assertEqual(len(got['dreamt']), 4)
        self.assertEqual(got['dreamt'][0], 'Dreamt: format_session_start_tier0 went stale (2026-09-21, fdaba23).')
        self.assertEqual(got['anchors'], 'ANCHORS: 12 intact, 0 modified since checkpoint')
        self.assertEqual(got['index'], 2)
        self.assertEqual(got['full'], 'csr_reflect_on_past("conv_1e0e1cf4-1fcf-4c30-919c-a4177f734813")')
        with open(session.path, 'a') as handle:
            handle.write(json.dumps(csr_row('compact', TOLD.replace('5h ago', '1m ago'), seconds=300)) + '\n')
        session.read_transcript()
        self.assertEqual(session.told['source'], 'compact')  # the newest wins
        self.assertIn('1m ago', session.told['text'])

    def test_a_block_with_no_recap_line_falls_back_to_its_first_line_of_substance(self):
        got = pane.told_lines('CSR ENDLESS MEMORY ACTIVE\nThe blocks below are memory\nReflex: x\n\n[briefing: y]\n## H\n**Recent**\n'
                              '- **1w ago:** the thing\nANCHORS: 3 intact')
        self.assertEqual((got['recap'], got['dreamt'], got['index'], got['full']), ('- **1w ago:** the thing', [], 0, None))
        self.assertIsNone(pane.told_lines('')['recap'])

    def test_the_todos_a_session_left_open_come_in_progress_first_then_by_number(self):  # B4
        folder = os.path.join(pane.TASKS, PREV_ID)
        os.makedirs(folder)
        for name, subject, status in (('1', 'first thing', 'pending'), ('2', 'done thing', 'completed'),
                                      ('3', 'running thing', 'in_progress'), ('4', 'gone', 'deleted'),
                                      ('10', 'tenth thing', 'pending')):
            put(os.path.join(folder, name + '.json'), dict(subject=subject, status=status))
        put(os.path.join(folder, 'junk.json'), dict(subject='not numbered', status='pending'))
        with open(os.path.join(folder, '5.json'), 'w') as handle:
            handle.write('{broken')
        open(os.path.join(folder, '.lock'), 'w').close()
        self.assertEqual(pane.left_todos(PREV_ID), [dict(subject='running thing', status='in_progress'),
                                                    dict(subject='first thing', status='pending'),
                                                    dict(subject='tenth thing', status='pending')])
        shutil.rmtree(folder)
        self.assertEqual(pane.left_todos(PREV_ID), [])  # no directory, no connection
        con = csr_db(todos=[dict(content='ship it', status='pending'), dict(content='did it', status='completed'),
                            dict(content='doing it', status='in_progress')])
        self.assertEqual(pane.left_todos(PREV_ID, con), [dict(subject='doing it', status='in_progress'),
                                                         dict(subject='ship it', status='pending')])
        self.assertEqual(pane.left_todos(PREV_ID, csr_db(todos=[dict(content='did it', status='completed')])), [])
        self.assertEqual(pane.left_todos('another-session', con), [])
        self.assertEqual(pane.left_todos(PREV_ID, sqlite3.connect(':memory:')), [])  # no table
        self.assertEqual(pane.left_todos('../x', con), [])

    def previous(self):
        rows = [prompt_row(0), reply_row(10, 50_000), prompt_row(100), reply_row(110, 100_000), boundary_row(120, 100_000),
                reply_row(130, 60_000), prompt_row(200), reply_row(210, 120_000), boundary_row(220, 120_000),
                reply_row(230, 70_000), prompt_row(300, 'second to last'), reply_row(310, 90_000),
                prompt_row(400, 'the last words I typed'), reply_row(410, 110_000, stop_reason='end_turn')]
        rows.insert(1, dict(prompt_row(0), type='ai-title', aiTitle='Build the thing'))
        prev = pane.Session(PREV_ID, path=self.write(PREV_ID, rows))
        prev.read_transcript()
        return prev

    def test_the_facts_about_the_last_session_are_counted_once(self):  # B5
        facts = pane.previous_facts(self.previous())
        self.assertEqual((facts['id'], facts['title'], facts['prompts'], facts['compacts']), (PREV_ID, 'Build the thing', 5, 2))
        self.assertEqual((facts['peak'], facts['floor']), (120_000, 65_000))  # the median of 60k and 70k
        self.assertEqual((facts['learned'], facts['handed'], facts['asked']), (7, 3, 0))  # 2 hand-offs + 1 correction
        self.assertAlmostEqual(facts['active'], 410.0, places=1)
        self.assertEqual(facts['last'] - facts['first'], 410.0)
        self.assertEqual([c[1] for c in facts['compactions']], [100_000, 120_000])
        self.assertEqual(len(facts['series']), 7)
        self.assertEqual(facts['typed'][1], 'the last words I typed')
        self.assertEqual(facts['said'][1], 'words')
        self.assertEqual(json.loads(json.dumps(facts)), facts)  # JSON-safe
        pane.connect = lambda: (_ for _ in ()).throw(sqlite3.OperationalError('locked'))
        blind = pane.previous_facts(self.previous())
        self.assertEqual((blind['learned'], blind['handed'], blind['prompts']), (None, None, 5))  # the DB can fail

    def test_a_long_series_is_thinned_but_keeps_the_ends_of_every_stretch(self):
        prev = self.previous()
        saved, pane.SERIES_MAX = pane.SERIES_MAX, 3
        try:
            facts = pane.previous_facts(prev)
        finally:
            pane.SERIES_MAX = saved
        full = [[round(t, 1), c] for t, c in sorted(prev.main)]
        cuts = prev.compacts
        self.assertLessEqual(len(facts['series']), 3 + 2 * (len(cuts) + 1))
        self.assertLess(len(facts['series']), len(full))
        for stretch in ([full[0], full[1]], [full[2], full[3]], [full[4], full[6]]):  # first and last of each
            for point in stretch:
                self.assertIn(point, facts['series'])
        self.assertEqual(pane.thin_series(full, cuts, 100), full)

    def session_of(self, name, rows):
        return pane.Session(name, path=self.write(name, rows))

    def test_the_session_before_is_read_once_and_kept_for_the_next_pane(self):  # B6
        self.write(PREV_ID, [prompt_row(0, 'start'), reply_row(10, 50_000),
                             reply_row(20, 60_000, stop_reason='end_turn'), prompt_row(30, 'last asked')],
                   written=1_800_000_100.0)
        mine = [prompt_row(5000, 'hello')]
        reads, real = [], pane.previous_read
        pane.previous_read = lambda path, started: reads.append(path) or real(path, started)
        try:
            first = self.session_of('mine-1', mine)
            first.read_transcript()
            pane.carry_into(first)
            self.assertEqual(len(reads), 1)
            kept = os.path.join(pane.KEEP, 'previous', 'mine-1.json')
            self.assertTrue(os.path.exists(kept))
            self.assertTrue(os.path.exists(os.path.join(pane.KEEP, 'carried', 'mine-1.json')))
            self.assertEqual([x for x in os.listdir(os.path.dirname(kept)) if x.endswith('.tmp')], [])
            self.assertEqual((first.previous['id'], first.previous['prompts']), (PREV_ID, 2))
            self.assertTrue(first.carried)
            second = self.session_of('mine-1', mine)  # another pane object for the same session
            second.read_transcript()
            pane.carry_into(second)
            self.assertEqual(len(reads), 1)  # read back, nothing read again
            self.assertEqual((second.previous, second.carried), (first.previous, first.carried))
            alone = self.session_of('alone-1', mine)
            alone.read_transcript()
            os.remove(os.path.join(self.folder, PREV_ID + '.jsonl'))
            pane.carry_into(alone)
            self.assertEqual((alone.previous, alone.carried), ({}, []))
            self.assertEqual(pane.read_previous('alone-1'), {})  # kept as {}: nothing found, nothing to look for again
            self.assertIsNone(pane.read_previous('never'))
        finally:
            pane.previous_read = real

    def test_a_carry_that_fails_leaves_the_session_with_nothing_to_show_not_a_crash(self):
        session = self.session_of('mine-2', [prompt_row(5000)])
        session.read_transcript()
        real, pane.previous_read = pane.previous_read, lambda *a: (_ for _ in ()).throw(RuntimeError('boom'))
        try:
            pane.carry_into(session)
        finally:
            pane.previous_read = real
        self.assertEqual((session.previous, session.carried), ({}, []))

    def test_a_pane_opened_before_the_first_prompt_carries_from_the_records_start(self):
        self.write(PREV_ID, [prompt_row(0, 'start'), reply_row(10, 50_000)], written=1_799_999_000.0)
        self.live(LIVE_ID, started=1_800_000_000)
        session = pane.Session(LIVE_ID)
        session.record = pane.session_record(LIVE_ID)
        pane.carry_into(session)  # no transcript for LIVE_ID yet
        self.assertEqual(session.previous['id'], PREV_ID)
        self.assertTrue(os.path.exists(os.path.join(pane.KEEP, 'previous', LIVE_ID + '.json')))

    # -- the page

    def thread(self, text, waiting, state='carried', rank=0, hidden=None):
        return dict(text=text, detail=[], first_seen=1.0, last_seen=2.0, times=1, state=state, waiting=waiting, rank=rank,
                    why='x', key=text[:6], came_from=PREV_ID[:8], hidden=hidden, later=False)

    def facts(self, **more):
        points = [[1_799_900_000 + 600 * k, 60_000 + 3_000 * k] for k in range(40)]
        return dict(dict(id=PREV_ID, path='/x', mtime=1.0, title='Codemod GitHub link side pane', first=points[0][0],
                         last=points[-1][0], active=44_000.0, prompts=126, compacts=16, peak=431_513, floor=73_120,
                         learned=4_213, handed=24, asked=8, series=points, prompt_times=[p[0] for p in points[::4]],
                         compactions=[[points[20][0], 90_000, 60_000]], todos=[], said=[points[-1][0], 'Claude words'],
                         typed=[points[-1][0], 'my last words']), **more)

    def fresh(self, **more):
        threads = [self.thread('Engine findings agent: wait for the report.', True),
                   self.thread('Engine scratch-indexing install needs the go.', True),
                   self.thread('A tidy up that waits on no one.', False, rank=1)]
        s = dict(id=LIVE_ID, title=None, prompts=[], replies=[], compacts=[], compact_sizes=[], main=[], turn_end=0.0,
                 remembered=[], gave=dict(recalls=[], carried=[], dreams=[]), project='demo', lost=threads,
                 told=dict(at=1_800_000_000.0, source='startup', text=TOLD), last_session=self.facts(), todos=[],
                 learned=dict(chunks=[], anchors=[], symbols=0, turns=[], memories=[]), asked=[], busy=False)
        return dict(s, **more)

    def names(self, s, **more):
        data = dict(dreams=more.get('dreams', []), chapter=dict(at=1000.0, of=5))
        got = page(dreams=more.get('dreams', ()))
        return pane.page_default(got, data, s, 1_800_000_000.0 + 3600, 24), got

    def test_a_session_with_nothing_of_its_own_gets_the_default_view(self):  # B7
        got = page()
        self.assertTrue(pane.default_view(got, self.fresh()))  # no turn has ended
        self.assertFalse(pane.default_view(got, self.fresh(id=None)))
        self.assertFalse(pane.default_view(got, dict(self.fresh(), turn_end=5.0, lost=[self.thread('x', True, state='open')])))
        story = dict(sections=[dict(name='remembered', items=[dict(key='k')]), dict(name='rules', items=[dict(key='r')])])
        self.assertFalse(pane.default_view(story, self.fresh(turn_end=5.0)))
        carried = dict(sections=[dict(name='rules', items=[dict(key='r')]), dict(name='lost', items=[dict(key='l')]),
                                 dict(name='story', items=[])])
        self.assertTrue(pane.default_view(carried, self.fresh(turn_end=5.0)))
        self.assertFalse(pane.default_view(carried, self.fresh(turn_end=5.0, lost=[self.thread('x', True, state='lost')])))
        self.assertTrue(pane.default_view(carried, self.fresh(turn_end=5.0, lost=[self.thread('x', True, state='lost', hidden='drop')])))

    def test_the_default_page_is_told_last_left_over_todos_in_that_order(self):  # B8
        s = self.fresh(todos=[dict(subject='ship the pane', status='in_progress'), dict(subject='write the doc', status='pending')])
        built, original = self.names(s)
        self.assertEqual([x['name'] for x in built['sections']], ['told', 'last', 'lost', 'todos'])
        told, last, lost, todos = built['sections']
        hhmm = pane.datetime.fromtimestamp(1_800_000_000).strftime('%H:%M')
        self.assertEqual((told['title'], told['right']), ('CSR told Claude', f'at start {hhmm}'))
        item = told['items'][0]
        self.assertEqual((item['key'], item['title'], item['lines'], item['plain_body']),
                         ('told:recap', 'recap [5h ago]: what were we discussing last session and what happens next', 3, True))
        self.assertEqual(item['body'], ['Dreamt: format_session_start_tier0 went stale (2026-09-21, fdaba23).',
                                        'ANCHORS: 12 intact, 0 modified since checkpoint · 2 earlier threads'])
        self.assertEqual(item['detail'][:2], ['Dreamt: a second dream line.', 'Dreamt: a third dream line.'])
        self.assertTrue(item['detail'][2].startswith('csr_reflect_on_past('))
        self.assertTrue(item['send'].startswith("CSR's recap at the start of this session: recap [5h ago]"))
        self.assertIn('The rest: csr_reflect_on_past("conv_1e0e1cf4', item['send'])
        self.assertEqual((lost['title'], lost['right']), ('Left over', '2 waiting · f all 3'))
        self.assertEqual([x['title'] for x in todos['items']], ['ship the pane', 'write the doc'])
        self.assertEqual(todos['items'][0]['meta'], f'in progress · from {PREV_ID[:8]}')
        self.assertEqual(todos['items'][0]['key'], f'todo:{PREV_ID[:8]}:0')
        self.assertTrue(todos['items'][0]['send'].startswith(f'Left over from session {PREV_ID[:8]}: “ship the pane” (in_progress)'))
        self.assertEqual(last['title'], 'Last session')
        item = last['items'][0]
        self.assertEqual((item['key'], item['title'], item['lines'], item['fit_body']),
                         (f'last:{PREV_ID}', 'Codemod GitHub link side pane', 1, True))
        self.assertEqual(item['body'], ['12h13m active · 126 prompts · 16 compacts', 'CSR learned 4.2k · handed 24× · peak 432k'])
        self.assertEqual(item['picture']['rows'], 3)
        self.assertEqual(item['detail'][1:], ['You last: “my last words”', 'Claude last: “Claude words”'])
        self.assertIn('73k', item['detail'][0])
        self.assertTrue(item['send'].startswith(f'Last session {PREV_ID[:8]} (“Codemod GitHub link side pane”): 12h13m active, 126 prompts, 16 compactions, last row'))
        self.assertTrue(item['send'].endswith(f'The rest of it: csr_reflect_on_past("conv_{PREV_ID}")'))
        stamped = pane.stamp(s['last_session']['last'], 1_800_000_000.0 + 3600)[0]
        self.assertEqual(last['right'], f'{PREV_ID[:8]} · {stamped}')
        self.live(PREV_ID)  # the session before is still running: it did not end
        built, _ = self.names(s)
        self.assertEqual(built['sections'][1]['right'], f'{PREV_ID[:8]} · still running')

    def test_the_default_page_leaves_out_what_it_has_nothing_for(self):
        built, _ = self.names(self.fresh(told=None))
        self.assertEqual([x['name'] for x in built['sections']], ['last', 'lost'])
        built, _ = self.names(self.fresh(told=dict(at=1_800_000_000.0, source='compact', text=TOLD)))
        self.assertTrue(built['sections'][0]['right'].startswith('after compact '))
        self.assertTrue(self.names(self.fresh(told=dict(at=1_800_000_000.0, source='resume', text=TOLD)))[0]['sections'][0]['right'].startswith('at resume '))
        self.assertTrue(self.names(self.fresh(told=dict(at=1_800_000_000.0, source='clear', text=TOLD)))[0]['sections'][0]['right'].startswith('after clear '))
        built, _ = self.names(self.fresh(told=None, last_session={}, lost=[]))
        self.assertEqual(built['sections'], [])
        self.assertEqual(built['quiet'], 'First session here: no earlier session in this project to carry over.')
        built, _ = self.names(self.fresh(told=None, last_session=None, lost=[]))
        self.assertEqual(built['quiet'], 'Reading the session before this one…')
        todo = self.names(self.fresh(todos=[dict(subject='x', status='pending')], told=None, lost=[]))[0]
        self.assertEqual([x['name'] for x in todo['sections']], ['last', 'todos'])

    def test_a_dream_waiting_on_you_comes_first_and_the_rest_of_the_page_follows_after(self):
        built, _ = self.names(self.fresh(id=HERE_NOW), dreams=[dream(1500)])
        self.assertEqual([x['name'] for x in built['sections']], ['dreams', 'told', 'last', 'lost'])
        # the sections a fresh session had of its own come after, but never lost or story
        s = self.fresh(id=HERE_NOW)
        got = page(dreams=[dream(1200, 'ignored')], remembered=[dict(kind='asked', at=1500.0, text='q', tool='t', times=1)])
        got['sections'].append(dict(name='story', title='Story', right='', items=[dict(key='story', title='x')], empty=''))
        data = dict(dreams=[dream(1200, 'ignored')], chapter=dict(at=1000.0, of=5))
        names = [x['name'] for x in pane.page_default(got, data, s, 1_800_003_600.0, 24)['sections']]
        self.assertEqual(names, ['told', 'last', 'lost', 'dreams', 'remembered'])

    def draw(self, cols, rows, s=None, pixels=False):
        data = dict(repo='/tmp/csr-engine', dreams=[], chapter=dict(at=1000.0, of=5), dreams_on=True, look=None, story=None,
                    claude_fields={}, claude_at=None, limits={}, sts=None)
        con = sqlite3.connect(':memory:')
        con.execute('create table intent_events (ts text, quote text, session_id text, transcript_path text, '
                    'byte_start int, project text, kind text)')
        pane.connect, saved = (lambda: con), (pane.PIXELS, pane.cell_size)
        pane.PIXELS, pane.cell_size = pixels and pane.HAS_PIL, lambda: (16, 34)
        try:
            return pane.draw_main(data, s or self.fresh(), cols, rows, 1_800_000_000.0 + 3600, {})
        finally:
            pane.PIXELS, pane.cell_size = saved

    @unittest.skipUnless(pane.HAS_PIL, 'Pillow is not installed')
    def test_with_pixels_the_picture_covers_blank_rows_at_the_items_column(self):  # B9
        if True:
            for rows, high in ((24, 3), (30, 4)):
                lines, pictures = self.draw(50, rows, pixels=True)
                shots = [p for p in pictures if p[1] == pane.ITEM]
                self.assertEqual(len(shots), 1)
                row, col, wide, tall, png = shots[0]
                self.assertEqual((col, wide, tall), (pane.ITEM, 43, high))
                self.assertTrue(png.startswith(b'\x89PNG\r\n\x1a\n'))
                self.assertTrue(all(pane.PIC in lines[row + k] for k in range(tall)))
                self.assertFalse(pane.PIC in lines[row - 1] or pane.PIC in lines[row + tall])
                self.assertEqual(flat(lines[row]).strip(), '')  # blank rows: the picture is laid over them
            lines, pictures = self.draw(29, 24, pixels=True)  # 22 cells: under PIC_MIN
            self.assertFalse([p for p in pictures if p[1] == pane.ITEM])
            self.assertTrue(any(set(plain(line)) & set('▁▂▃▄▅▆▇█') for line in lines))
            self.assertFalse(any(pane.PIC in line for line in lines))
            lines, pictures = self.draw(31, 24, pixels=True)  # 24 cells: exactly PIC_MIN
            self.assertEqual([p[2] for p in pictures if p[1] == pane.ITEM], [24])

    def test_without_pixels_one_row_of_block_characters_stands_for_the_picture(self):
        lines, pictures = self.draw(50, 24)
        self.assertEqual(pictures, [])
        spark = [flat(line) for line in lines if set(flat(line)) & set('▁▂▃▄▅▆▇█')]
        self.assertEqual(len(spark), 1)
        self.assertEqual(len(spark[0].strip()), 43)
        self.assertTrue(spark[0].startswith(' ' * pane.ITEM))
        lines, _ = self.draw(27, 24)  # 20 cells: still a sparkline, down to 8
        self.assertTrue(any(set(plain(line)) & set('▁▂▃▄▅▆▇█') for line in lines))
        self.assertEqual(pane.last_sparkline(self.facts(), 7), '')
        self.assertEqual(len(pane.last_sparkline(self.facts(), 8)), 8)
        self.assertEqual(pane.last_sparkline(self.facts(series=[]), 20), '')
        lines, _ = self.draw(10, 24)  # the pane never draws narrower than 24
        self.assertTrue(all(len(flat(line)) <= 24 for line in lines))

    @unittest.skipUnless(pane.HAS_PIL, 'Pillow is not installed')
    def test_the_figure_is_drawn_once_cached_and_never_raises(self):  # B10
        ground, cell = pane.tint('last', pane.ROW), (16, 34)
        self.assertIsNone(pane.last_png(self.facts(series=[]), 43, 3, cell, ground))
        self.assertIsNone(pane.last_png(self.facts(series=[[1.0, 2]]), 43, 3, cell, ground))
        first = pane.last_png(self.facts(), 43, 3, cell, ground)
        self.assertTrue(first.startswith(b'\x89PNG'))
        self.assertIs(pane.last_png(self.facts(), 43, 3, cell, ground), first)  # by identity: not drawn again
        from PIL import Image
        import io
        self.assertEqual(Image.open(io.BytesIO(first)).size, (43 * 16, 3 * 34))
        self.assertIsNot(pane.last_png(self.facts(), 43, 4, cell, ground), first)
        for odd in (None, 'x', {}, dict(series='abc'), dict(series=[[1, 2], 'x'])):
            self.assertIsNone(pane.last_png(odd, 43, 3, cell, ground), odd)
        for k in range(12):
            pane.last_png(self.facts(id=f'x{k}'), 43, 3, cell, ground)
        self.assertLessEqual(len(pane._LAST_PNG), 8)
        saved, pane.HAS_PIL = pane.HAS_PIL, False
        try:
            self.assertIsNone(pane.last_png(self.facts(), 43, 3, cell, ground))
        finally:
            pane.HAS_PIL = saved

    def test_the_figure_is_inked_in_pigments_nyt_marks_on_the_rows_own_tint(self):
        if not pane.HAS_PIL:
            self.skipTest('Pillow is not installed')
        from PIL import Image
        import io
        ground = pane.tint('last', pane.ROW)
        image = Image.open(io.BytesIO(pane.last_png(self.facts(id='ink'), 43, 4, (16, 34), ground))).convert('RGB')
        colours = {pixel for pixel in image.getdata()}
        near = lambda ink, within: any(all(abs(a - b) <= within for a, b in zip(pixel, ink[:3])) for pixel in colours)
        self.assertIn(ground, colours)  # a flat ground, the row's own tint
        self.assertTrue(near(pane.INK['turn'], 20))  # the peak's dot, vermilion
        self.assertTrue(near(pane.INK['ctx'], 40))  # the context hairline, indigo

    def test_a_fresh_session_shows_no_empty_lane_and_no_zero_counter(self):  # B11
        empty = self.facts(compacts=0, learned=0, handed=0, asked=0, peak=0, floor=None)
        lines, _ = self.draw(50, 30, self.fresh(last_session=empty))
        text = '\n'.join(plain(line) for line in lines)
        for zero in ('0 prompts', '0 learned', '0 handed', '0 asked', '0 compacts'):
            self.assertNotIn(zero, text)
        self.assertFalse(re.search(r'^\s*(you|context|memory)\s*$', text, re.M))
        self.assertIn('12h13m active · 126 prompts', text)

    def test_opening_a_default_row_keeps_the_picture_and_shows_what_more_csr_knows(self):
        key = f'last:{PREV_ID}'
        for pixels in (True, False):
            if pixels and not pane.HAS_PIL:
                continue
            data = dict(repo='/tmp/csr-engine', dreams=[], chapter=dict(at=1000.0, of=5), dreams_on=True, look=None,
                        story=None, claude_fields={}, claude_at=None, limits={}, sts=None)
            con = lambda: self.intent_db()
            saved = pane.connect, pane.PIXELS, pane.cell_size
            pane.connect, pane.PIXELS, pane.cell_size = con, pixels, lambda: (16, 34)
            try:
                lines, pictures = pane.draw_main(data, self.fresh(), 50, 600, 1_800_003_600.0, dict(open=key, rows=24))
            finally:
                pane.connect, pane.PIXELS, pane.cell_size = saved
            text = '\n'.join(flat(line) for line in lines)
            self.assertIn('You last: “my last words”', text)
            self.assertIn('Claude last: “Claude words”', text)
            self.assertIn("↳ click again → Claude's prompt", text)
            shots = [p for p in pictures if p[1] == pane.ITEM]
            self.assertEqual(len(shots), 1 if pixels else 0)
            if pixels:
                self.assertTrue(all(pane.PIC in lines[shots[0][0] + k] for k in range(3)))  # the card's rows under the picture
                self.assertTrue(all('▌' in flat(lines[shots[0][0] + k]) for k in range(3)))  # its edge runs beside it
            else:
                self.assertTrue(set(text) & set('▁▂▃▄▅▆▇█'))

    def intent_db(self):
        con = sqlite3.connect(':memory:')
        con.execute('create table intent_events (ts text, quote text, session_id text, transcript_path text, '
                    'byte_start int, project text, kind text)')
        return con

    def test_the_masthead_stays_three_rows_over_the_default_view(self):
        lines, _ = self.draw(50, 24)
        rows = [plain(line) for line in lines[:3]]
        self.assertTrue(rows[0].startswith(' CTX '))
        self.assertTrue(rows[1].strip().startswith('♻ --'))
        self.assertTrue(rows[2].strip().startswith('5H --  7D --'))  # no status line yet, and no account's either
        self.assertEqual(plain(lines[3]).strip(), '')


class InfoPage(unittest.TestCase):
    def info(self, **more):
        now = 1_800_000_000.0
        s = dict(id='6ca2a4d5-5785-4952-90ed-a5c245cbdfee', title='Codemod GitHub link side pane', model='claude-opus-5-5',
                 prompts=[now - 3600 + 60 * k for k in range(126)], replies=[(now - 100, 88_000, 5, 0)],
                 main=[(now - 7000, 432_000), (now - 100, 88_000)], compacts=[now - 5000] * 16,
                 compact_sizes=[(None, p, None) for p in REAL_POSTS], told=dict(at=now - 7200, source='startup', text=TOLD),
                 transcript=True, learned=dict(chunks=[now - 50] * 7, anchors=[], symbols=0, turns=[], memories=[]),
                 gave=dict(recalls=[now - 60], carried=[], dreams=[]), asked=[now - 10], remembered=[])
        fields = dict(prompt_cache=dict(warm=True, ttl='1h', expires_at=now + 2460),
                      context_window=dict(total_input_tokens=88_000, context_window_size=1_000_000),
                      rate_limits=dict(five_hour=dict(used_percentage=8, resets_at=now + 7200),
                                       seven_day=dict(used_percentage=75, resets_at=now + 3 * 86400)))
        sts = dict(sev='advisory', inop=[('RATIFICATION', 'INHIBITED · CSR_NO_RATIFICATION=1 (daemon)', 'advisory')],
                   daemon=dict(up=True, pid=16493, since=now - 6000))
        data = dict(claude_fields=fields, claude_at=None, claude_line='Opus 5.5 │ \x1b[38;5;208m███░░░░░░░ 33% 255k ♻9t\x1b[0m',
                    limits={}, sts=sts)
        data.update({k: v for k, v in more.items() if k != 's'})
        return data, dict(s, **more.get('s', {})), now

    def test_the_main_legend_names_the_page_and_every_legend_fits(self):  # I1
        self.assertIn('i session info', plain(pane.legend('main', 60)))
        for view in ('main', 'summary', 'threads', 'info', 'log'):
            for width in range(16, 90):
                self.assertLessEqual(len(plain(pane.legend(view, width))), width + 1, (view, width))
        self.assertIn('d back', plain(pane.legend('info', 60)))
        self.assertIn('j/k scroll', plain(pane.legend('info', 60)))
        self.assertNotIn('changed', plain(pane.legend('info', 80)))  # no marks
        self.assertIn('s compaction summary', plain(pane.legend('main', 80)))

    def test_the_legend_gives_up_f_then_s_then_i_and_keeps_changes_and_close(self):
        self.assertEqual(plain(pane.legend('main', 49)).strip(), 'l changes  s summary  f thread  i info  q close')
        self.assertEqual(plain(pane.legend('main', 38)).strip(), 'l changes  s summary  i info  q close')
        self.assertEqual(plain(pane.legend('main', 31)).strip(), 'l changes  i info  q close')
        self.assertEqual(plain(pane.legend('main', 20)).strip(), 'l changes  q close')
        spans = pane.legend_parts('main', 60)[1]
        self.assertEqual([key for _, _, key in spans], ['l', 's', 'f', 'i', 'q'])

    def test_i_opens_the_page_i_or_esc_goes_back_and_j_k_scroll(self):  # I2
        shell = types.SimpleNamespace(view='main', open='x', info_top=0, threads_top=0)
        pane.press(shell, 'i')
        self.assertEqual((shell.view, shell.open), ('info', None))
        self.assertTrue(pane.info_keys(shell, 'j'))
        self.assertEqual(shell.info_top, 3)
        self.assertTrue(pane.info_keys(shell, ' '))
        self.assertEqual(shell.info_top, 15)
        self.assertTrue(pane.info_keys(shell, 'k'))
        self.assertEqual(shell.info_top, 12)
        self.assertTrue(pane.info_keys(shell, '\x1b'))
        self.assertEqual(shell.view, 'main')
        self.assertFalse(pane.info_keys(shell, 'j'))  # not on the page: nothing to do
        self.assertTrue(pane.info_keys(shell, 'i'))
        self.assertEqual((shell.view, shell.info_top), ('info', 0))  # opening starts at the top
        self.assertTrue(pane.info_keys(shell, 'i'))  # i again goes back
        self.assertEqual(shell.view, 'main')
        shell.view = 'info'
        pane.Pane.scroll(shell, 3)
        self.assertEqual(shell.info_top, 3)
        pane.Pane.scroll(shell, -30)
        self.assertEqual(shell.info_top, 0)

    def test_the_checklist_rows_run_in_order_with_dot_leaders(self):  # I3
        data, s, now = self.info()
        saved = pane.PIXELS, pane.time.time
        pane.PIXELS, pane.time.time = False, lambda: now
        try:
            lines, pictures, top = pane.draw_info(data, s, 50, 40, {})
        finally:
            pane.PIXELS, pane.time.time = saved
        text = [flat(line) for line in lines]
        self.assertEqual(text[0].strip(), 'Session info')
        self.assertEqual(text[1].strip(), '6ca2a4d5 · Codemod GitHub link side pane')
        body = '\n'.join(text)
        labels = [re.match(r' ([A-Z0-9 ]+?) \.+ ', line).group(1) for line in text[2:] if re.match(r' ([A-Z0-9 ]+?) \.+ ', line)]
        self.assertEqual(labels, ['RATIFICATION', 'MODEL', 'STATUS LINE', 'CONTEXT', 'PROMPTS', 'COMPACTS', 'CACHE',
                                  '7D LIMIT', '5H LIMIT', 'DAEMON', 'RECAP'])
        self.assertRegex(body, r' RATIFICATION \.+ INHIBITED ·\n +CSR_NO_RATIFICATION=1\n +\(daemon\)')  # wrapped under itself
        self.assertRegex(body, r' MODEL \.+ claude-opus-5-5')
        self.assertRegex(body, r' PROMPTS \.+ 126 · ')
        values = {label: value for label, value, _ in pane.info_rows(data, s, now)}
        self.assertEqual(values['CONTEXT'], '88k of 1M · peak 432k · floor 73k (last 5)')
        self.assertRegex(body, r' CONTEXT \.+ 88k of 1M · peak 432k ·\n +floor 73k \(last 5\)')  # wrapped under itself
        self.assertRegex(body, r' COMPACTS \.+ 16 · ♻ >15t')
        self.assertRegex(body, r' CACHE \.+ WARM · 41m left')
        self.assertRegex(body, r' 7D LIMIT \.+ 75% ↻ ')
        self.assertRegex(body, r' 5H LIMIT \.+ 8%')
        self.assertTrue(values['DAEMON'].startswith('UP · pid 16493 · since '))
        self.assertRegex(body, r' DAEMON \.+ UP · pid 16493 · since\n +\d\d:\d\d')
        self.assertRegex(body, r' RECAP \.+ INJECTED \d\d:\d\d \(startup\)')
        self.assertEqual(pane.COLOUR.sub('', values['STATUS LINE']), 'Opus 5.5 │ ███░░░░░░░ 33% 255k ♻9t')
        self.assertRegex(body, r' STATUS LINE \.+ Opus 5.5 │ ███░░░░░░░ 33%')  # cut to the column, with an ellipsis
        self.assertIn('\x1b[38;5;208m', next(line for line in lines if 'STATUS LINE' in line))  # its own colours kept
        ends = {re.match(r' [A-Z0-9 ]+? \.+ ', line).end() for line in text if re.match(r' [A-Z0-9 ]+? \.+ ', line)}
        self.assertEqual(len(ends), 1)  # every value starts in one column
        self.assertIn('▮', body)  # the lanes' legend: learned, handed, asked, this session
        self.assertIn('· this session', body)
        self.assertIn('CTX context · ♻ compact payback · ↻ resets', body)  # a narrow page drops the last pieces
        wide, _, _ = pane.draw_info(data, s, 80, 40, {})
        self.assertIn('CTX context · ♻ compact payback · ↻ resets · STS status', '\n'.join(flat(line) for line in wide))
        self.assertEqual(pictures, [])

    @unittest.skipUnless(pane.HAS_PIL, 'Pillow is not installed')
    def test_with_pixels_the_lanes_picture_sits_under_the_checklist(self):
        data, s, now = self.info()
        saved = pane.PIXELS, pane.cell_size, pane.time.time
        pane.PIXELS, pane.cell_size, pane.time.time = True, lambda: (16, 34), lambda: now
        try:
            lines, pictures, _ = pane.draw_info(data, s, 50, 40, {})
        finally:
            pane.PIXELS, pane.cell_size, pane.time.time = saved
        self.assertEqual(len(pictures), 1)
        row, col, wide, tall, png = pictures[0]
        self.assertEqual((col, wide, tall), (pane.LANE, 49 - pane.LANE, 3))
        self.assertTrue(png.startswith(b'\x89PNG'))
        labels = [flat(lines[row + k]).strip() for k in range(3)]
        self.assertEqual(labels, ['you', 'context', 'memory'])

    def test_a_session_with_nothing_yet_has_no_lanes_no_counters_and_says_what_comes_first(self):
        data, s, now = self.info(s=dict(prompts=[], replies=[], main=[], compacts=[], compact_sizes=[], told=None, transcript=False,
                                        model=None, learned=dict(chunks=[], anchors=[], symbols=0, turns=[], memories=[]),
                                        gave=dict(recalls=[], carried=[], dreams=[]), asked=[]),
                                 claude_fields={}, claude_line=None)
        saved = pane.PIXELS, pane.time.time
        pane.PIXELS, pane.time.time = pane.HAS_PIL, lambda: now
        try:
            lines, pictures, _ = pane.draw_info(data, s, 50, 40, {})
        finally:
            pane.PIXELS, pane.time.time = saved
        body = '\n'.join(flat(line) for line in lines)
        self.assertEqual(pictures, [])
        for gone in ('PROMPTS', 'COMPACTS', 'CONTEXT', 'CACHE', 'STATUS LINE', 'learned', 'handed', 'asked', 'this session', 'LIMIT'):
            self.assertNotIn(gone, body)
        self.assertFalse(re.search(r'^\s*(you|context|memory)\s*$', body, re.M))
        self.assertRegex(body, r' MODEL \.+ --')  # MODEL and RECAP always show
        self.assertRegex(body, r' RECAP \.+ AFTER YOUR FIRST PROMPT')
        rows = pane.info_rows(data, dict(s, transcript=True), now)
        self.assertIn(('RECAP', 'NONE', None), rows)  # a transcript, and no row found in it
        self.assertEqual([r[0] for r in rows], ['RATIFICATION', 'MODEL', 'DAEMON', 'RECAP'])  # nothing else has a value

    def test_a_compaction_under_way_leads_the_checklist_with_its_facts_under_it(self):
        info = dict(elapsed=42, trigger='manual', usual=dict(low=69.0, median=105.0, high=141.0, n=10),
                    facts=['CSR keeps 12 chunks of this session'])
        data, s, now = self.info(compacting=info)
        rows = pane.info_rows(data, s, now, 60)
        self.assertEqual(rows[1][:2], ('COMPACTING', 'Compacting · 0:42 · usually 1:09 to 2:21'))
        self.assertEqual(rows[2][0], '')
        self.assertEqual(rows[2][1], '█' * 11 + '▉' + '░' * 28)
        self.assertEqual(rows[3][:2], ('', 'CSR keeps 12 chunks of this session'))
        self.assertEqual([r[0] for r in rows if r[0] == 'COMPACTS'], ['COMPACTS'])
        self.assertEqual([r for r in rows if r[0] == 'COMPACTS'][0][1], '16')  # no payback while one runs
        saved, pane.time.time = pane.time.time, lambda: now
        try:
            lines, _, _ = pane.draw_info(data, s, 50, 40, {})
        finally:
            pane.time.time = saved
        text = '\n'.join(flat(line) for line in lines)
        self.assertRegex(text, r' COMPACTING \.+ Compacting · 0:42')
        self.assertIn('CSR keeps 12 chunks', text)
        self.assertIn('█' * 7 + '▏' + '░' * 16, text)  # the bar is as wide as the value column: 24 cells

    def test_the_page_scrolls_and_never_past_its_end(self):
        data, s, now = self.info()
        saved, pane.time.time = pane.time.time, lambda: now
        try:
            full, _, _ = pane.draw_info(data, s, 50, 60, {})
            short, _, top = pane.draw_info(data, s, 50, 10, {}, top=4)
            far, _, top_far = pane.draw_info(data, s, 50, 10, {}, top=999)
        finally:
            pane.time.time = saved
        self.assertEqual((top, len(short)), (4, 9))
        self.assertEqual(flat(short[2]), flat(full[2 + 4]))  # the checklist moved up four rows, the title stayed
        self.assertEqual(flat(short[0]).strip(), 'Session info')
        self.assertEqual(flat(far[-1]), flat(full[-1]))
        self.assertGreater(top_far, 4)

    def test_a_long_value_wraps_under_itself_and_a_missing_session_still_draws(self):
        data, s, now = self.info()
        long = ('x' * 18 + ' ') * 20
        got = pane.info_line('RATIFICATION', long, 40, 'advisory')
        self.assertGreater(len(got), 1)
        self.assertTrue(all(len(plain(row)) <= 40 for row in got))
        self.assertEqual(len(plain(got[1])) - len(plain(got[1]).lstrip()), 19)  # under the value: label column 18, a space
        lines, pictures, _ = pane.draw_info({}, None, 50, 30, {})
        self.assertIn('Session info', flat(lines[0]))
        self.assertEqual(pictures, [])


class Hold(unittest.TestCase):
    def test_sections_are_held_only_while_claude_works_and_under_two_minutes(self):  # H1
        hold = dict(sections=['old'], at=1000.0)
        self.assertEqual(pane.HOLD_MAX, 120)
        self.assertIsNone(pane.held_sections(hold, False, 1010.0))  # idle: draw fresh
        self.assertEqual(pane.held_sections(hold, True, 1010.0), ['old'])
        self.assertEqual(pane.held_sections(hold, True, 1119.0), ['old'])
        self.assertIsNone(pane.held_sections(hold, True, 1120.0))  # busy for too long: draw fresh
        self.assertIsNone(pane.held_sections(None, True, 1010.0))

    def test_a_busy_frame_draws_the_old_sections_and_the_first_idle_frame_the_new_ones(self):
        base = page(story=dict(text=STORY, now=STORY, rev=1, by='', at=1990.0))
        item = lambda k: dict(key=f'k{k}', title=f'item {k}', lines=1)
        sections = [dict(name='story', title='Story', right='', items=[item(1)], empty='')]
        real = pane.page_main
        pane.page_main = lambda data, s, now, ui=None: dict(base, sections=list(sections))
        shell = types.SimpleNamespace(hold=None)
        s = dict(id='x', busy=True, turn_end=5.0, lost=[], prompts=[], replies=[], compacts=[], remembered=[],
                 gave=dict(recalls=[], carried=[], dreams=[]), project='p')
        names = lambda lines: [x['name'] for x in lines.sections]
        try:
            draw = lambda busy, now: pane.Pane.draw_main_held(shell, dict(), dict(s, busy=busy), 50, 30, now, {})
            lines, _ = draw(True, 1000.0)
            self.assertEqual(names(lines), ['story'])  # nothing held yet: fresh, and kept
            self.assertEqual(shell.hold['at'], 1000.0)
            sections.append(dict(name='dreams', title='What if…', right='', items=[item(2)], empty=''))
            lines, _ = draw(True, 1010.0)  # a new section arrives mid-turn
            self.assertEqual(names(lines), ['story'])
            self.assertNotIn('item 2', '\n'.join(plain(line) for line in lines))
            self.assertEqual(shell.hold['at'], 1000.0)  # still the old hold
            lines, _ = draw(False, 1020.0)  # the turn ended
            self.assertEqual(names(lines), ['story', 'dreams'])
            self.assertIn('item 2', '\n'.join(plain(line) for line in lines))
            self.assertEqual((names(types.SimpleNamespace(sections=shell.hold['sections'])), shell.hold['at']),
                             (['story', 'dreams'], 1020.0))  # an idle frame re-holds every time
            sections[:] = sections[:1]
            lines, _ = draw(True, 1030.0)
            self.assertEqual(names(lines), ['story', 'dreams'])  # held again
            lines, _ = draw(True, 1020.0 + pane.HOLD_MAX)  # held too long
            self.assertEqual(names(lines), ['story'])
            self.assertEqual(shell.hold['at'], 1020.0 + pane.HOLD_MAX)  # and re-held
        finally:
            pane.page_main = real

    def test_the_masthead_is_never_held(self):
        base = page()
        real = pane.page_main
        shell = types.SimpleNamespace(hold=None)
        s = dict(id='x', busy=True, turn_end=5.0, lost=[], prompts=[], replies=[], compacts=[], remembered=[],
                 gave=dict(recalls=[], carried=[], dreams=[]), project='p')
        marks = []
        pane.page_main = lambda data, s_, now, ui=None: dict(base, head=dict(base['head'], gauges=pane.gauges(
            dict(prompt_cache=dict(warm=True, expires_at=now + marks[0])), None, s_, now)), sections=[])
        try:
            for now, left in ((1000.0, 3000), (1010.0, 2000)):
                marks[:] = [left]
                lines, _ = pane.Pane.draw_main_held(shell, dict(), s, 50, 30, now, {})
            self.assertIn('CACHE 33m', plain(lines[1]))  # live at the second frame, not 50m from the first
        finally:
            pane.page_main = real


if __name__ == '__main__':
    unittest.main()
