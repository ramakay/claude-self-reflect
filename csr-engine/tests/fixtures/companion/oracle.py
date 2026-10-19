"""Development oracle for WP1. Run from any directory with python3 -I."""
import contextlib
import io
import sys
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
        create table chunks(id text,conversation_id text,timestamp text,created_at text,project_name text,content text);
        create table chunk_embeddings(chunk_id text,embedding blob);
        create table narrative_usage(started_at text,tokens_in integer,tokens_out integer,model text,cost_usd real);
        create table reflections(id text,content text,tags text,timestamp text);
        create table session_registry(session_id text,project text,first_prompt text,first_ts text,last_ts text);
        create table retrieval_events(session_id text,retrieved_at text,memory_type text,memory_id text);
        create table intent_events(ts text,quote text,session_id text,transcript_path text,byte_start integer,project text,kind text,file text);
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
    lost_rows = [
        dict(type='user', isCompactSummary=True, timestamp='2026-10-18T06:00:00Z',
             message=dict(content='7. Pending Tasks:\n- Fix the importer retry path, awaiting your approval.\n- Install the radio queue cache.\n8. Current Work:\nElsewhere.')),
        dict(type='user', isCompactSummary=True, timestamp='2026-10-18T07:00:00Z',
             message=dict(content='7. Pending Tasks:\n- Install the radio queue cache.\n8. Current Work:\nElsewhere.')),
    ]
    (projects / 'sess-lost.jsonl').write_text(''.join(json.dumps(r) + '\n' for r in lost_rows))
    own = []
    for ago, pre, seconds, trigger in [(3600, 200000, 80, 'manual'), (7200, 150000, 60, 'manual'),
                                      (9000, 250000, 70, 'auto'), (86400, 300000, 100, 'manual'),
                                      (3 * 86400, 190000, 95, 'manual')]:
        from datetime import datetime, timezone
        own.append(dict(type='system', subtype='compact_boundary',
                        timestamp=datetime.fromtimestamp(NOW - ago, timezone.utc).isoformat(),
                        compactMetadata=dict(trigger=trigger, preTokens=pre, durationMs=seconds * 1000)))
    (projects / 'sess-own.jsonl').write_text(''.join(json.dumps(r) + '\n' for r in own))
    transcript = projects / (SID + '.jsonl')
    rows[-1]['message']['content'].append(dict(type='tool_use', name='Read',
        input=dict(file_path=str(REPO / 'csr-engine/src/main.rs'))))
    transcript.write_text(''.join(json.dumps(r) + '\n' for r in rows))
    (HOME / '.claude/history.jsonl').write_text(json.dumps(dict(
        display='What if the importer always kept a bounded reader?', sessionId='past-voice',
        timestamp=(NOW - 86400) * 1000, project='/projects/fixture')) + '\n')
    import struct
    for chunk, session, content in [('chunk-now', SID, 'The reader is bounded.'),
                                     ('chunk-past', 'past-voice', 'What if the importer always kept a bounded reader?')]:
        con.execute('insert into chunks(id,conversation_id,timestamp,content) values(?,?,?,?)',
                    (chunk, session, '2026-10-18T07:55:00Z', content))
        con.execute('insert into chunk_embeddings values(?,?)', (chunk, struct.pack('3f', 1., 2., 3.)))
    for n in (1, 2):
        coach_rows = [dict(type='user', message=dict(content='call quokka now')),
                      dict(type='assistant', message=dict(content=[dict(type='tool_use', name='Bash',
                           input=dict(command='grep -ril "quokka" /home/u/.claude/memory'))]))]
        (projects / f'coach-{n}.jsonl').write_text(''.join(json.dumps(r, separators=(',', ':')) + '\n' for r in coach_rows))
    agents = keep / 'agents'
    agents.mkdir(exist_ok=True)
    (agents / f'{SID}.json').write_text(json.dumps(dict(version=1, session=SID, at=NOW * 1000,
        omitted_agents=0, agents=[dict(id='a1', parent=None, description='Review the reader',
        type='Explore', model='claude-sonnet-4-5', status='completed', started=(NOW-600)*1000,
        first_seen=(NOW-600)*1000, last_at=(NOW-20)*1000, tools=3, tokens=1200,
        edits=[str(REPO / 'csr-engine/src/main.rs')], edits_omitted=0, overlaps=[], overlaps_omitted=0,
        last='Reviewed the reader', last_tool='Read', current_turn='turn-1', listed=True,
        completion=dict(key='a1:turn-1', turnId='turn-1', at=(NOW-20)*1000, reason='answer',
                        answer='Bounded [reader]\nReady.', disposition='pending'))])))
    tasks = HOME / '.claude/tasks/sess-before'
    tasks.mkdir(parents=True, exist_ok=True)
    (tasks / '1.json').write_text(json.dumps(dict(subject='Verify the reader', status='pending')))
    rollout = HOME / '.codex/sessions/2026/10/18/rollout-fixture.jsonl'
    rollout.parent.mkdir(parents=True, exist_ok=True)
    rollout.write_text(json.dumps(dict(type='session_meta', payload=dict(id='codex-fixture', cwd=str(REPO)))) + '\n')
    (projects / 'sess-before.jsonl').write_text(json.dumps({'type': 'continued-in', 'sessionId': 'sess-before', 'continuedInSessionId': SID}) + '\n')
    (projects / 'sess-older.jsonl').write_text(json.dumps({'type': 'system', 'subtype': 'compact_boundary', 'timestamp': '2026-10-17T07:52:00Z', 'compactMetadata': {'preTokens': 180000, 'postTokens': 20000}}) + '\n')
    con.execute('insert into session_registry values(?,?,?,?,?)', (SID, REPO.name, 'Verify the frame', '2026-10-18T07:50:00Z', '2026-10-18T07:55:00Z'))
    con.execute('insert into intent_events(ts,quote,session_id,transcript_path,byte_start,project,kind) values(?,?,?,?,?,?,?)', ('2026-10-18T10:39:00Z', 'Keep the read-only database boundary.', SID, str(transcript), 0, REPO.name, 'correction'))
    for i, (file, symbol, verdict) in enumerate([('csr-engine/src/main.rs', 'main', 'superseded_by'), ('csr-engine/src/lib.rs', '', 'anchor_obsolete')], 1):
        con.execute('insert into witness_ledger values(?,?,?,?)', (i, str(REPO / file), symbol, '8e4e3f27'))
        con.execute('insert into witness_verdicts values(?,?,?,?,?,?)', (i, i, verdict, '8e4e3f27', '2026-10-18T07:58:00Z', '2026-10-18T07:58:00Z'))
    con.execute('insert into reflections values(?,?,?,?)', ('story', 'The pane reads the session. Work stands at: text parity is ready for review. No model call is needed.', json.dumps(['session_story', 'conv_' + SID, 'story_rev_3', 'story_by_haiku']), '2026-10-18T10:39:30Z'))
    con.execute('insert into intent_events(ts,quote,session_id,project,kind,file) values(?,?,?,?,?,?)',
                ('2026-10-17T07:50:00Z', 'Keep input bounded.', 'past-session', 'other-project', 'correction', 'csr-engine/src/main.rs'))
    con.execute('insert into meta values(?,?)', ('story_map:' + SID, json.dumps(dict(v=1, current=dict(
        rev=2, at=NOW-600, by='Haiku', covered_msgs=3, entries=[dict(kind='shift', claim='Bounded the reader',
        was='The reader loaded a whole transcript.', now='The reader streams chunks.',
        before=[dict(scope='reader.rs', sym='read', mark='~', tier='verified')],
        after=[dict(scope='reader.rs', sym='read', mark='~', tier='verified')],
        receipts=['oid:8e4e3f27'], grounded=True)])))))
    con.commit()
    # *.db is gitignored: the Rust tests build the fixture DB from this dump.
    (HERE / 'home.sql').write_text('\n'.join(con.iterdump()) + '\n')
    con.close()
    dreams = [
        dict(kind='compact', event='compact', outcome='compacted', session=SID,
             at=NOW-480, ctx_before=190000, ctx_after=21000),
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
    lease_spec = importlib.util.spec_from_file_location('lease', REPO / 'plugins/csr-companion/bin/lease.py')
    lease = importlib.util.module_from_spec(lease_spec)
    lease_spec.loader.exec_module(lease)
    leases = []
    for step, held, root, rank, busy in [
        ('take', None, 'dev', 1, True),
        ('take', dict(root='installed', rank=0, at=NOW * 1000, busy=True), 'dev', 1, True),
        ('renew', dict(root='dev', rank=1, at=NOW * 1000, busy=True), 'dev', 1, False),
        ('release', dict(root='dev', rank=1, at=NOW * 1000, busy=True), 'dev', 1, False),
    ]:
        mine, updated = lease.decide(step, held, root, rank, int(NOW * 1000 + 1), busy)
        leases.append(dict(step=step, held=held, root=root, rank=rank, busy=busy,
                           output=dict(held=mine, lease=held if updated is None else updated)))
    (HERE / 'lease.json').write_text(json.dumps(leases, indent=2) + '\n')
    pane.PIXELS = False
    with mock.patch.object(pane.time, 'time', return_value=NOW):
        def normalize(value):
            if isinstance(value, str):
                return value.replace(str(HOME), '<HOME>').replace(str(REPO), '<REPO>')
            if isinstance(value, list):
                return [normalize(v) for v in value]
            if isinstance(value, dict):
                return {normalize(k): normalize(v) for k, v in value.items()}
            return value

        def mode(name, *args, stdin=''):
            output = io.StringIO()
            with mock.patch.object(sys, 'argv', ['pane.py', '--repo', str(REPO), *args]), \
                    mock.patch.object(sys, 'stdin', io.TextIOWrapper(io.BytesIO(stdin.encode()))), \
                    contextlib.redirect_stdout(output):
                pane.main()
            text = output.getvalue()
            if name == 'brief':
                (HERE / (name + '.txt')).write_text(normalize(text))
            else:
                (HERE / (name + '.json')).write_text(json.dumps(normalize(json.loads(text)), indent=2) + '\n')

        mode('brief', '--brief', '--sid', SID)
        mode('coach', '--coach')
        mode('story-news', '--story-news', '--sid', SID)
        mode('lost', '--lost', '--sid', 'sess-lost')
        mode('compact-plan', '--compact-plan', '--sid', SID, '--ctx', '300000')
        mode('compact-usual', '--compact-usual')
        mode('json', '--json', '--limit', '60', '--sid', SID)
        mode('note', '--note', str(REPO / 'csr-engine/src/main.rs'), '--touch', '-',
             stdin=json.dumps(dict(tool='Read', offset=1, limit=20)))
        mode('keep', '--keep', json.dumps(dict(kind='compact', session=SID, at=NOW, event='plan', token='abc')))
        record = json.loads((HOME / '.claude-self-reflect/csr-companion/dreams.jsonl').read_text().splitlines()[-1])
        (HERE / 'keep-record.json').write_text(json.dumps(normalize(record), indent=2) + '\n')
        # Keep the WP1 frames independent of the hook-write fixture.
        log = HOME / '.claude-self-reflect/csr-companion/dreams.jsonl'
        log.write_text('\n'.join(log.read_text().splitlines()[:-1]) + '\n')
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

    cases_spec = importlib.util.spec_from_file_location('cases', HERE / 'cases.py')
    cases = importlib.util.module_from_spec(cases_spec)
    cases_spec.loader.exec_module(cases)
    cases.capture(REPO, HERE)
    for path in HOME.rglob('*'):
        if path.is_file() and path.suffix in ('.json', '.jsonl'):
            path.write_text(path.read_text().replace(str(REPO), '<REPO>'))
    for name in ['.private', 'coach.json', 'compact-usual.json', 'compaction-times.json']:
        (HOME / '.claude-self-reflect/csr-companion' / name).unlink(missing_ok=True)


if __name__ == '__main__':
    main()
