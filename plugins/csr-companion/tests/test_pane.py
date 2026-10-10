"""The pane script's counting, without a live CSR: python3 -m unittest discover -s tests -p 'test_*.py'"""
import importlib.util
import os
import sqlite3
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('pane', os.path.join(HERE, '..', 'bin', 'pane.py'))
pane = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pane)

PAST = 'a3e857eb-1111-4222-8333-444455556666'
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

    def test_what_claude_asked_csr_folds_into_the_count(self):
        asked = [dict(kind='asked', at=1500.0 + k, text=f'query {k}', tool='reflect_on_past', times=1) for k in range(3)]
        handed = next(s for s in page(remembered=asked)['sections'] if s['name'] == 'remembered')
        self.assertEqual([i['title'] for i in handed['items']], ['Claude asked 3×'])


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


if __name__ == '__main__':
    unittest.main()
