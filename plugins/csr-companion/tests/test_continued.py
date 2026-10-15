"""F2.13: after `claude -c` the conversation has a new session id and the pane follows Claude Code's continued-in row
back to the old one. A temp HOME holds an old transcript ending in that row and a new one; no live CSR is read."""
import importlib.util
import json
import os
import sqlite3

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('pane_continued', os.path.join(HERE, '..', 'bin', 'pane.py'))
pane = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pane)

OLD = '6ca2a4d5-5785-4952-90ed-a5c245cbdfee'
NEW = 'eda7fbcf-964f-4c64-ad87-95e35a9a1b70'
EARLY = '2026-10-15T01:10:00.000Z'  # the copied tail keeps its original time, before the continuation


def write(path, rows, mtime):
    with open(path, 'w', encoding='utf-8') as handle:
        for row in rows:
            handle.write(json.dumps(row, separators=(',', ':')) + '\n')  # as Claude Code writes it
    os.utime(path, (mtime, mtime))


@pytest.fixture
def home(tmp_path, monkeypatch):
    """A projects folder: the old transcript ends in continued-in and was written after the new one began."""
    folder = tmp_path / '.claude' / 'projects' / '-work'
    folder.mkdir(parents=True)
    old = [dict(type='user', timestamp='2026-10-15T00:50:00.000Z', entrypoint='cli', message=dict(role='user', content='hi')),
           dict(type='continued-in', timestamp='2026-10-15T04:25:00.701Z', sessionId=OLD, continuedInSessionId=NEW),
           dict(type='last-prompt', sessionId=OLD)]
    new = [dict(type='ai-title'), dict(type='user', timestamp=EARLY, entrypoint='cli', message=dict(role='user', content='hi'))]
    write(str(folder / f'{OLD}.jsonl'), old, 5000.0)
    write(str(folder / f'{NEW}.jsonl'), new, 4990.0)  # the old file was written to after the new one
    monkeypatch.setattr(pane, 'HOME', str(tmp_path))
    monkeypatch.setattr(pane, '_CONTINUED', {})
    monkeypatch.delenv('CSR_NO_CONTINUED', raising=False)
    return tmp_path, folder


def test_continued_from_finds_the_session_this_one_continues(home):
    assert pane.continued_from(NEW) == OLD
    assert pane.continued_from(OLD) is None
    assert pane.lineage(NEW) == [NEW, OLD]
    assert pane.lineage(OLD) == [OLD]


def test_a_row_naming_another_session_or_an_odd_id_is_not_followed(home):
    tmp, folder = home
    other = 'ffffffff-0000-4000-8000-000000000000'
    write(str(folder / f'{OLD}.jsonl'), [dict(type='continued-in', sessionId=OLD, continuedInSessionId=other)], 5000.0)
    assert pane.continued_from(NEW) is None
    write(str(folder / f'{OLD}.jsonl'), [dict(type='continued-in', sessionId='../x y', continuedInSessionId=NEW)], 5000.0)
    pane._CONTINUED.clear()
    assert pane.continued_from(NEW) is None


def test_none_is_looked_for_again_after_a_minute_and_a_find_is_kept(home, monkeypatch):
    tmp, folder = home
    clock = [1000.0]
    monkeypatch.setattr(pane.time, 'time', lambda: clock[0])
    old_rows = (folder / f'{OLD}.jsonl').read_text()
    (folder / f'{OLD}.jsonl').write_text('')
    os.utime(folder / f'{OLD}.jsonl', (5000.0, 5000.0))
    assert pane.continued_from(NEW) is None
    (folder / f'{OLD}.jsonl').write_text(old_rows)
    os.utime(folder / f'{OLD}.jsonl', (5000.0, 5000.0))
    clock[0] += 59
    assert pane.continued_from(NEW) is None, 'inside the minute: not looked at again'
    clock[0] += 2
    assert pane.continued_from(NEW) == OLD
    (folder / f'{OLD}.jsonl').unlink()
    clock[0] += 3600
    assert pane.continued_from(NEW) == OLD, 'a find is kept for the life of the process'


def test_a_row_speaks_only_for_the_file_it_is_in(home):
    tmp, folder = home
    stranger = 'bbbbbbbb-0000-4000-8000-000000000000'
    write(str(folder / f'{stranger}.jsonl'),  # newer than the genuine one, and it claims another session continued into NEW
          [dict(type='continued-in', sessionId='xxxxxxxx-0000-4000-8000-000000000000', continuedInSessionId=NEW)], 6000.0)
    assert pane.continued_from(NEW) == OLD, 'the genuine row wins; the newer sibling cannot name a stranger'
    pane._CONTINUED.clear()
    (folder / f'{OLD}.jsonl').unlink()
    assert pane.continued_from(NEW) is None, 'a row whose sessionId is not its file is ignored'
    pane._CONTINUED.clear()
    write(str(folder / f'{stranger}.jsonl'), [dict(type='continued-in', sessionId=NEW, continuedInSessionId=NEW)], 6000.0)
    write(str(folder / f'{NEW}.jsonl'), [dict(type='continued-in', sessionId=NEW, continuedInSessionId=NEW)], 6000.0)
    assert pane.continued_from(NEW) is None, 'a session is never its own predecessor'


def test_without_a_birth_time_there_is_no_time_bound(home, monkeypatch):
    tmp, folder = home
    os.utime(folder / f'{OLD}.jsonl', (1000.0, 1000.0))  # the old file went quiet long before the new one's last write
    os.utime(folder / f'{NEW}.jsonl', (1300.0, 1300.0))
    real = os.stat

    def no_birthtime(path, *a, **k):  # a Linux stat: no st_birthtime
        found = real(path, *a, **k)
        return type('Stat', (), dict(st_mtime=found.st_mtime, st_size=found.st_size))()
    monkeypatch.setattr(pane.os, 'stat', no_birthtime)
    assert pane.continued_from(NEW) == OLD


def test_a_miss_is_not_kept_while_the_own_transcript_is_missing(home):
    tmp, folder = home
    (folder / f'{NEW}.jsonl').rename(folder / 'parked.jsonl')
    (folder / f'{OLD}.jsonl').rename(folder / 'parked-old.jsonl')
    assert pane.continued_from(NEW) is None
    assert NEW not in pane._CONTINUED
    (folder / 'parked.jsonl').rename(folder / f'{NEW}.jsonl')
    (folder / 'parked-old.jsonl').rename(folder / f'{OLD}.jsonl')
    assert pane.continued_from(NEW) == OLD, 'no clock advance: the miss was never written'


def test_the_kill_switch_returns_none(home, monkeypatch):
    monkeypatch.setenv('CSR_NO_CONTINUED', '1')
    assert pane.continued_from(NEW) is None
    assert pane.lineage(NEW) == [NEW]


def test_a_cycle_ends_the_lineage(home, monkeypatch):
    monkeypatch.setattr(pane, 'continued_from', lambda sid, path=None: {NEW: OLD, OLD: NEW}.get(sid))
    assert pane.lineage(NEW) == [NEW, OLD]


def test_previous_session_returns_the_continued_from_file_though_it_was_written_after_this_began(home):
    tmp, folder = home
    mine = str(folder / f'{NEW}.jsonl')
    assert pane.previous_session(mine, 4000.0) == str(folder / f'{OLD}.jsonl')
    pane._CONTINUED.clear()
    os.environ['CSR_NO_CONTINUED'] = '1'
    try:
        assert pane.previous_session(mine, 4000.0) is None  # today's rule: nothing was last written before it began
    finally:
        del os.environ['CSR_NO_CONTINUED']


def memory(path, *, stories=(), recalls=(), maps=()):
    con = sqlite3.connect(path)
    con.execute('create table reflections (id text, content text, tags text, timestamp text)')
    con.execute('create table retrieval_events (session_id text)')
    con.execute('create table meta (key text primary key, value text not null)')
    for sid, rev in stories:
        con.execute('insert into reflections values (?, ?, ?, ?)',
                    (f'story_{sid}', f'Story of {sid[:4]}. Where it stands: done.', json.dumps(['session_story', f'conv_{sid}', f'story_rev_{rev}']),
                     '2026-10-15T01:00:00Z'))
    for sid, count in recalls:
        con.executemany('insert into retrieval_events values (?)', [(sid,)] * count)
    for sid in maps:
        con.execute('insert into meta values (?, ?)', (f'story_map:{sid}', json.dumps(MAP)))
    con.commit()
    con.close()


MAP = dict(v=1, current=dict(rev=3, at=1000.0, by='Haiku', covered_msgs=5, entries=[
    dict(kind='shift', claim='Added a thing', before=[], after=[dict(scope='a.py', sym='f', mark='+', tier='verified')],
         receipts=['oid:abcdef0'], grounded=True)]))


@pytest.fixture
def db(home, monkeypatch):
    path = str(home[0] / 'csr.db')

    def use(**kwargs):
        if os.path.exists(path):
            os.unlink(path)
        memory(path, **kwargs)
        monkeypatch.setattr(pane, 'connect', lambda: sqlite3.connect(path))
    return use


def test_session_story_own_wins_and_the_fallback_names_where_it_came_from(db):
    db(stories=[(OLD, 39)])
    story = pane.session_story(NEW)
    assert story['rev'] == 39 and story['from_sid'] == OLD
    assert 'from_sid' not in pane.session_story(OLD)
    db(stories=[(OLD, 39), (NEW, 1)])
    own = pane.session_story(NEW)
    assert own['rev'] == 1 and 'from_sid' not in own


def test_the_story_heading_says_where_an_inherited_story_is_from(db):
    db(stories=[(OLD, 39)])
    inherited = pane.story_rights(pane.session_story(NEW), 0)
    assert inherited[0].startswith('rev 39 · from 6ca2a4d5')
    db(stories=[(OLD, 39)])
    assert 'from' not in pane.story_rights(pane.session_story(OLD), 0)[0]


def test_display_story_map_prefers_its_own_map_then_the_predecessors(db):
    db(maps=[OLD])
    smap, source = pane.display_story_map(NEW)
    assert smap and source == OLD
    db(maps=[OLD, NEW])
    assert pane.display_story_map(NEW)[1] == NEW
    db()
    assert pane.display_story_map(NEW) == (None, NEW)


def test_the_map_title_carries_from_only_when_inherited(db):
    db(maps=[OLD])
    smap, source = pane.display_story_map(NEW)
    assert pane.shift_map_rows(smap, 9, source=source)[0].endswith(' · from 6ca2a4d5')
    assert 'from' not in pane.shift_map_rows(smap, 9)[0]


def test_recalled_sums_the_lineage(db, monkeypatch):
    db(recalls=[(OLD, 174), (NEW, 2), ('someone-else', 9)])
    monkeypatch.setattr(pane, 'load_shifts', lambda con, root: (0, []))
    monkeypatch.setattr(pane, 'load_changes', lambda *a, **k: [])
    monkeypatch.setattr(pane, 'reminded', lambda con, floor: (0, 0))
    monkeypatch.setattr(pane, 'load_dream_log', lambda root=None: [])
    monkeypatch.setattr(pane, 'last_look', lambda root=None: None)
    monkeypatch.setattr(pane, 'last_chapter', lambda *a: None)
    monkeypatch.setattr(pane, 'dreams_on', lambda: False)
    assert pane.load('/tmp/r', 10, NEW)['recalled'] == 176
    assert pane.load('/tmp/r', 10, OLD)['recalled'] == 174
    assert pane.load('/tmp/r', 10, None)['recalled'] == 0


def test_story_news_stays_on_the_own_session(db):
    db(maps=[OLD])
    assert pane.story_news(NEW, None, 0.0) == dict(story=False)


def test_how_claude_read_falls_back_to_the_predecessor_that_has_readings(home, monkeypatch):
    tmp, folder = home
    summary = dict(type='user', isCompactSummary=True, timestamp=EARLY,
                   message=dict(role='user', content='Interpreted as: "dknr" meant "done".'))
    write(str(folder / f'{OLD}.jsonl'), [summary, dict(type='continued-in', sessionId=OLD, continuedInSessionId=NEW)], 5000.0)
    monkeypatch.setattr(pane, '_READINGS', {})
    found = pane.how_claude_read(NEW)
    assert found and found == pane.how_claude_read(OLD)
    assert pane.how_claude_read('not-a-session') == []


def test_the_page_draws_an_inherited_map_against_the_old_transcript_with_from_in_its_title(db, monkeypatch):
    db(maps=[OLD])
    smap, source = pane.display_story_map(NEW)
    def rules_only():  # the page reads the rules too; there are none
        con = sqlite3.connect(':memory:')
        con.execute('create table intent_events (ts text, quote text, session_id text, transcript_path text, '
                    'byte_start int, project text, kind text)')
        return con
    monkeypatch.setattr(pane, 'connect', rules_only)
    data = dict(repo='/tmp/r', total=0, story=None, story_map=smap, story_map_from=source, story_map_messages=9, dreams_on=False, dreams=[],
                recalled=0, likes={}, readings=[])
    page = pane.page_main(data, dict(id=NEW, messages=2), 1000.0, dict(open='shifts', cols=60, rows=40))
    (shown,) = [item for section in page['sections'] for item in section['items'] if item['key'] == 'shifts']
    assert shown['title'] == 'Shift Map · as of 4 messages ago · from 6ca2a4d5'  # 9 in the old transcript, 5 covered
    data.update(story_map_from=None)
    page = pane.page_main(data, dict(id=NEW, messages=2), 1000.0, dict(open='shifts', cols=60, rows=40))
    (shown,) = [item for section in page['sections'] for item in section['items'] if item['key'] == 'shifts']
    assert shown['title'] == 'Shift Map · current'  # own map: counted against this session's own messages (2 < 5)
