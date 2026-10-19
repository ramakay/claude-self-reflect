"""Development oracle for WP1. Run from any directory with python3 -I."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sqlite3
from unittest import mock

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
HOME = HERE / 'home'
NOW = 1792320000.0
SID = 'sess-now'


def build():
    keep = HOME / '.claude-self-reflect' / 'csr-companion'
    projects = HOME / '.claude' / 'projects' / 'fixture'
    projects.mkdir(parents=True, exist_ok=True)
    (keep / 'statusline').mkdir(parents=True, exist_ok=True)
    db = HOME / '.claude-self-reflect' / 'csr-engine.db'
    db.unlink(missing_ok=True)
    con = sqlite3.connect(db)
    con.executescript('''
        create table meta(key text,value text);
        create table chunks(id text,conversation_id text,timestamp text,created_at text,project_name text);
        create table narrative_usage(started_at text,tokens_in integer,tokens_out integer,model text,cost_usd real);
        create table reflections(id text,content text,tags text,timestamp text);
        create table session_registry(session_id text,project text,first_prompt text,first_ts text,last_ts text);
        create table retrieval_events(session_id text,retrieved_at text,memory_type text,memory_id text);
        create table intent_events(ts text,quote text,session_id text,transcript_path text,byte_start integer,project text,kind text);
        create table witness_ledger(id integer,file text,symbol text,at_oid text);
        create table witness_verdicts(id integer,witness_id integer,verdict text,receipt_oid text,receipt_committed_at text,created_at text);
        create table witness_chunk_bindings(witness_id integer,chunk_id text);
        create table episode_anchors(session_id text,created_at text,name text,file text);
        create table code_nodes(last_session_id text,last_conv_id text,last_seen text,name text,file text);
        create table memory_registry(origin_session_id text,slug text);
        create table chunk_provenance(chunk_id text,source_conv_id text);
        create table correction_deliveries(delivered_at text,target_session_id text);
        create table dream_deliveries(delivered_at text,session_id text);
    ''')
    rows = [
        {'type': 'user', 'timestamp': '2026-10-18T07:50:00Z', 'message': {'content': 'Keep the read-only database boundary.'}},
        {'type': 'assistant', 'timestamp': '2026-10-18T07:51:00Z', 'message': {'id': 'm1', 'model': 'claude-sonnet-4-5', 'stop_reason': 'end_turn', 'usage': {'input_tokens': 95000}, 'content': [{'type': 'text', 'text': 'The reader keeps the boundary.'}]}},
        {'type': 'system', 'timestamp': '2026-10-18T07:52:00Z', 'subtype': 'compact_boundary', 'compactMetadata': {'preTokens': 190000, 'postTokens': 21000, 'durationMs': 42000}},
        {'type': 'user', 'timestamp': '2026-10-18T07:53:00Z', 'isCompactSummary': True, 'message': {'content': '## Decisions\nRead the database through Storage.'}},
        {'type': 'user', 'timestamp': '2026-10-18T07:54:00Z', 'message': {'content': 'Now verify the frame.'}},
        {'type': 'assistant', 'timestamp': '2026-10-18T07:55:00Z', 'message': {'id': 'm2', 'stop_reason': 'end_turn', 'usage': {'input_tokens': 70000}, 'content': [{'type': 'text', 'text': 'The text frame is ready.'}]}},
    ]
    transcript = projects / (SID + '.jsonl')
    transcript.write_text(''.join(json.dumps(r) + '\n' for r in rows))
    (projects / 'sess-before.jsonl').write_text(json.dumps({'type': 'continued-in', 'sessionId': 'sess-before', 'continuedInSessionId': SID}) + '\n')
    (projects / 'sess-older.jsonl').write_text(json.dumps({'type': 'system', 'subtype': 'compact_boundary', 'timestamp': '2026-10-17T07:52:00Z', 'compactMetadata': {'preTokens': 180000, 'postTokens': 20000}}) + '\n')
    con.execute('insert into session_registry values(?,?,?,?,?)', (SID, REPO.name, 'Verify the frame', '2026-10-18T07:50:00Z', '2026-10-18T07:55:00Z'))
    con.execute('insert into intent_events values(?,?,?,?,?,?,?)', ('2026-10-18T10:39:00Z', 'Keep the read-only database boundary.', SID, str(transcript), 0, REPO.name, 'correction'))
    for i, (file, symbol, verdict) in enumerate([('csr-engine/src/main.rs', 'main', 'superseded_by'), ('csr-engine/src/lib.rs', '', 'anchor_obsolete')], 1):
        con.execute('insert into witness_ledger values(?,?,?,?)', (i, str(REPO / file), symbol, '8e4e3f27'))
        con.execute('insert into witness_verdicts values(?,?,?,?,?,?)', (i, i, verdict, '8e4e3f27', '2026-10-18T07:58:00Z', '2026-10-18T07:58:00Z'))
    con.execute('insert into reflections values(?,?,?,?)', ('story', 'The pane reads the session. Work stands at: text parity is ready for review. No model call is needed.', json.dumps(['session_story', 'conv_' + SID, 'story_rev_3', 'story_by_haiku']), '2026-10-18T10:39:30Z'))
    con.commit()
    # *.db is gitignored: the Rust tests build the fixture DB from this dump.
    (HERE / 'home.sql').write_text('\n'.join(con.iterdump()) + '\n')
    con.close()
    dreams = [
        dict(kind='chapter', session=SID, at=NOW - 3600, of=5),
        dict(kind='dream', id='older', session=SID, at=NOW - 500, line='What if every read stayed bounded?', title='A small reader', text='- Bound the tail before parsing.'),
        dict(kind='answer', id='older', answer='helpful', at=NOW - 450),
        dict(kind='dream', id='newest', session=SID, at=NOW - 60, line='What if a frame failed while the agent was busy?', title='Keep the pane alive', text='- Catch a failing frame without losing the terminal.\n- Keep the heartbeat independent of the renderer.\n- Retry after a short backoff.'),
    ]
    (keep / 'dreams.jsonl').write_text(''.join(json.dumps(r) + '\n' for r in dreams))
    status = dict(at=NOW - 30, line='fixture status', fields=dict(
        context_window=dict(total_input_tokens=155000, context_window_size=1000000),
        prompt_cache=dict(warm=True, ttl='1h', expires_at=NOW+1800),
        rate_limits=dict(five_hour=dict(used_percentage=12.5, resets_at=NOW+3600), seven_day=dict(used_percentage=34, resets_at=NOW+400000))))
    (keep / 'statusline' / (SID + '.json')).write_text(json.dumps(status))


def main():
    build()
    os.environ['HOME'] = str(HOME)
    spec = importlib.util.spec_from_file_location('pane', REPO / 'plugins/csr-companion/bin/pane.py')
    pane = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(pane)
    pane.PIXELS = False
    with mock.patch.object(pane.time, 'time', return_value=NOW):
        data = pane.load(str(REPO), 120, SID)
        data.update(story=pane.session_story(SID), claude_fields=pane.claude_fields(SID, NOW),
                    claude_at=NOW-30, limits=pane.account_limits(NOW), sts=pane.csr_status(NOW),
                    claude_line=pane.claude_status(SID, NOW))
        session = pane.Session(SID)
        session.carried, session.previous = [], {}
        s = session.load()
        for cols, rows in [(46, 30), (100, 40)]:
            with mock.patch.object(pane.shutil, 'get_terminal_size', return_value=os.terminal_size((cols, rows))):
                for page in ['main', 'log']:
                    ui = dict(likes={})
                    feed = pane.Feed(None)
                    if page == 'main':
                        lines, _ = pane.draw_main(data, s, cols, rows, NOW, ui, feed=feed)
                    else:
                        lines, _ = pane.draw_log(data, cols, rows, ui)
                    lines = [pane.safe_line(line) for line in lines[:rows-1]]
                    lines += [''] * (rows-1-len(lines))
                    lines.append(pane.legend(page, cols-1, reload=60 if page == 'main' else None))
                    (HERE / f'{page}-{cols}x{rows}.txt').write_text('\n'.join(lines) + '\n')


if __name__ == '__main__':
    main()
