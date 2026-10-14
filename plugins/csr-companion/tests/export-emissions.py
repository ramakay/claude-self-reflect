#!/usr/bin/env python3
"""Export what the pane can type into Claude's prompt, from the pane's own formatters.

csr-engine removes CSR's own emissions from the corpus by grammar. A grammar written from examples drifts from
the formatter it describes (a date shaped YYYY-MM-DD where the pane writes MM-DD, a sentinel the pane strips
before it sends). This script calls the real functions in bin/pane.py on synthetic inputs and writes every send
they produce, so the engine's tests start from the formatter's output and not from a description of it.

    python3 -I tests/export-emissions.py --write   # regenerate both fixture copies
    python3 -I tests/export-emissions.py --check   # fail if either copy is stale

Two byte-identical copies are written: one beside the plugin tests, one inside csr-engine, which must stay
self-contained when it is packaged. The fixture carries the SHA-256 of pane.py; the engine recomputes it when the
plugin is checked out beside it.

Inputs are invented. Nothing is read from a transcript, a database or the home directory.
"""
import hashlib
import importlib.util
import json
import os
import sqlite3
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, '..', '..', '..'))
PANE = os.path.join(HERE, '..', 'bin', 'pane.py')
PLUGIN_COPY = os.path.join(HERE, 'fixtures', 'emissions', 'pane-v1.json')
ENGINE_COPY = os.path.join(ROOT, 'csr-engine', 'tests', 'fixtures', 'contamination', 'pane-v1.json')

# The pane writes times in the local zone; the fixture must not depend on where it is generated.
os.environ['TZ'] = 'UTC'
time.tzset()
os.environ['HOME'] = tempfile.mkdtemp(prefix='csr-emissions-')  # pane.py reads HOME at import; nothing is there

spec = importlib.util.spec_from_file_location('pane', PANE)
pane = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pane)

NOW = 1_800_000_000.0  # 2027-01-15 UTC
SESSION = 'sess-now'
PAST = '00000000-1111-4222-8333-444455556666'

# send prefix -> the engine's contamination reason
REASONS = (
    ('CSR remembered', 'plugin_pane_recall'),
    ('Earlier you read', 'plugin_pane_reading'),
    ('A standing rule from', 'plugin_standing_rule'),
    ("CSR's dreamer asked", 'plugin_dream_forward'),
    ("CSR's story of this session", 'plugin_session_story'),
    ("CSR's compaction card", 'plugin_compaction_card'),
    ("CSR's recap at the start", 'plugin_recap_forward'),
    ('Last session', 'plugin_last_session'),
    ('Left over from session', 'plugin_leftover'),
    ('Since our past sessions', 'plugin_code_shift'),
    ('Lost and found', 'plugin_lost_and_found'),
)

cases = {}


def record(case_id, producer, send):
    """One send the pane can type: its id, the function that wrote it, the engine reason, and the exact text."""
    if not send:
        return
    reason = next((why for prefix, why in REASONS if send.startswith(prefix)), None)
    if reason is None:
        raise SystemExit(f'{case_id}: a send with no registered reason: {send[:60]!r}')
    cases[case_id] = dict(
        id=case_id, producer=producer, reason=reason, text=send,
        chars=len(send), cut=len(send) in (pane.PANE_SEND_MOST, pane.CARD_SEND_MOST, pane.STORY_SEND_MOST) and
        send.rstrip().endswith(('…', '…”')))


def forwarded(case_id, producer, text):
    """A send an action hands on. The text is the item's own, so it must already be a recorded send; if it is
    not, it is recorded now. Then it goes through the pane's transport, which must not change it."""
    if not text:
        return
    if text not in {case['text'] for case in cases.values()}:
        record(case_id, producer, text)
    sent = []
    saved = pane.herdr
    pane.herdr = lambda *args: sent.append(args) or ''
    try:
        pane.send_text('agent', text)
    finally:
        pane.herdr = saved
    if not sent or sent[0][-1] != text:
        raise SystemExit(f'{case_id}: the transport changed a send: {text[:60]!r} -> {sent[:1]!r}')


def acts_of(act, case_id, producer):
    """Every send one click target can type: its own, and each of its asks."""
    forwarded(case_id, producer, act.get('send'))
    for ask, text in (act.get('asks') or {}).items():
        forwarded(f'{case_id}-{ask}', producer + ':asks', text)


def items_of(page):
    for section in page['sections']:
        for item in section['items']:
            yield section['name'], item


def page(data_more=None, s_more=None, rules=(), dreams=(), story=None, infinite=None, ui=None):
    """pane.page_main over a stand-in CSR: the rules, dreams, story and card as given."""
    con = sqlite3.connect(':memory:')
    con.execute('create table intent_events (ts text, quote text, session_id text, transcript_path text, '
                'byte_start int, project text, kind text)')
    for ts, quote in rules:
        con.execute('insert into intent_events values (?, ?, ?, ?, 0, ?, ?)',
                    (ts, quote, SESSION, '/nowhere.jsonl', 'csr-engine', 'correction'))
    saved = pane.connect, pane.typed_by_you
    pane.connect, pane.typed_by_you = (lambda: con), (lambda *a, **k: True)  # the rules are taken as typed by you
    try:
        s = dict(id=SESSION, prompts=[], replies=[], compacts=[], remembered=[],
                 gave=dict(recalls=[], carried=[], dreams=[]), project='csr-engine', lost=[])
        s.update(s_more or {})
        data = dict(repo='/tmp/csr-engine', dreams=list(dreams), chapter=dict(at=1000.0, of=5), dreams_on=True,
                    look=None, story=story, infinite=infinite, readings=[])
        data.update(data_more or {})
        return pane.page_main(data, s, NOW, ui), data, s
    finally:
        pane.connect, pane.typed_by_you = saved


def dream(at, line, title, text):
    return dict(id=f'dream:{at}', at=at, answer=None, session=SESSION, line=line, title=title, text=text)


def thread(text, last_seen):
    return dict(text=text, detail=[], first_seen=last_seen - 600, last_seen=last_seen, times=1, state='lost', waiting=False,
                rank=0, why='x', key=text[:6], came_from=PAST[:8], hidden=None, later=False)


def export():
    long = 'The importer keeps the exact bytes outside a span and drops nothing else. ' * 14  # ~1000 characters

    # recall_act: a stored note, with and without a date and project, and a past session; each cut at 700.
    for name, item in (
        ('note-where', dict(key='reflection:abc', text='Use component ownership before chunking', when=NOW - 3 * 86400,
                            project='csr-engine', times=2)),
        ('note-bare', dict(key='reflection:def', text='Never delete unproven text', when=None, project=None, times=1)),
        ('note-long', dict(key='reflection:ghi', text=long, when=NOW - 86400, project='csr-engine', times=1)),
        ('session', dict(key=PAST, text='Fix the scrub overmatch', when=NOW - 5 * 86400, project='csr-engine', times=1)),
        ('session-long', dict(key=PAST, text=long, when=NOW - 5 * 86400, project='csr-engine', times=1)),
    ):
        record(f'recall-{name}', 'recall_act', pane.recall_act(item, NOW)['send'])

    # reading_items: the three shapes of how Claude read you, and a line long enough to be cut.
    for name, reading in (
        ('read-as', dict(words='keep it', read='delete it', line='x', when=NOW - 100, source='compaction')),
        ('unclear', dict(words='dknr', read='', line='x', when=NOW - 100, source='reply')),
        ('line', dict(words='', read='', line='Read the whole request as a plan to ship tonight.', when=NOW - 100, source='reply')),
        ('line-long', dict(words='', read='', line=long, when=NOW - 100, source='reply')),
        ('read-as-long', dict(words='keep it', read=long, line='x', when=NOW - 100, source='compaction')),
    ):
        record(f'reading-{name}', 'reading_items', pane.reading_items([reading], NOW)[0]['send'])

    # page_main: the rules (both date forms), the dreams, the story, the card, the lost threads.
    rules = [
        ('2026-10-13T08:00:00Z', 'Never delete unproven text'),   # a parseable time: short_date, MM-DD
        ('sometime in October 2026', 'Keep receipts'),            # not parseable: the first ten characters
        ('2026-10-12T08:00:00Z', long),                           # a quote cut at 700
    ]
    infinite = dict(card=pane.compaction_card(
        [dict(kind='compact', event='compact', at=1030.0, session='s', ctx_before=278_129, why='w',
              outcome='compacted', ctx_after=21_730)], [], 1031.0))
    story = dict(text='We moved the “queue”. The importer now keeps owned spans.', now='We moved the queue.',
                 rev=2, by='Haiku', at=NOW - 20)
    story_long = dict(story, text=long * 2, rev=11)
    dreams = [
        dream(1900, 'What if the pane said “fresh” first?', 'Say “fresh”', '* It reads ” oddly.\n* Two.'),
        dream(1800, 'What if spans were authoritative?', 'Ownership boundary', '* It would avoid text-only deletion.'),
        dream(1700, 'What if we cut the card?', 'Card', '* ' + long),
    ]
    lost = [thread('Run source-gone validation', NOW - 600), thread(long, NOW - 1200)]
    for case, kwargs in (
        ('main', dict(rules=rules, dreams=dreams, story=story, infinite=infinite, s_more=dict(lost=lost))),
        ('story-long', dict(story=story_long)),
    ):
        built, _, _ = page(**kwargs)
        for number, (section, item) in enumerate(items_of(built)):
            producer = f'page_main:{section}'
            if item.get('send'):
                record(f'{case}-{section}-{number}', producer, item['send'])
            for ask, send in (item.get('asks') or {}).items():
                record(f'{case}-{section}-{number}-{ask}', producer + ':asks', send)

    # the sites the first pass left out: the earlier dreams a click unfolds (ui folds), a hand-off Claude opened,
    # the populated readings section, and a standing rule whose date is empty
    readings = [dict(words='keep it', read='delete it', line='x', when=NOW - 100, source='compaction'),
                dict(words='', read='', line=long, when=NOW - 200, source='reply')]
    remembered = [dict(kind='recall', opened=True, at=NOW - 50, key='reflection:abc', text='Use **component** ownership',
                       when=NOW - 3 * 86400, project='csr-engine', times=2),
                  dict(kind='recall', opened=True, at=NOW - 60, key=PAST, text=long, when=NOW - 86400, project=None, times=1)]
    unfolded, data, s = page(rules=rules, dreams=dreams, story=story,
                             data_more=dict(readings=readings), s_more=dict(remembered=remembered, lost=lost),
                             ui=dict(folds={'passed'}))
    for number, (section, item) in enumerate(items_of(unfolded)):
        producer = f'page_main:{section}'
        recorded = {case['text'] for case in cases.values()}
        if item.get('send') and item['send'] not in recorded:
            record(f'unfolded-{section}-{number}', producer, item['send'])
        for ask, send in (item.get('asks') or {}).items():
            if send not in recorded:
                record(f'unfolded-{section}-{number}-{ask}', producer + ':asks', send)
        # the click targets under the page: dream answers, lost answers
        for rows_, act in pane.answer_rows(item, 60, {}):
            acts_of(act, f'answers-{section}-{number}', 'answer_rows')
        for rows_, act in pane.lost_rows(item, 60, {}, True):
            acts_of(act, f'lost-rows-{number}', 'lost_rows')

    nodate, _, _ = page(rules=[('', 'Keep a rule with no date')])
    for number, (section, item) in enumerate(items_of(nodate)):
        record(f'rule-nodate-{section}-{number}', f'page_main:{section}', item.get('send'))

    # the f page: every thread, one opened, the click targets it draws
    every = [thread('Run source-gone validation', NOW - 600), thread(long, NOW - 1200),
             dict(thread('Sweep the stale index', NOW - 900), state='open', waiting=True, key='sweep-')]
    out, _, _ = pane.draw_threads(data, dict(s, lost=every), 80, 80, dict(open='lost:' + every[1]['key']))
    for number, act in enumerate(out.acts.values()):
        acts_of(act, f'threads-{number}', 'draw_threads')

    # "-> subagent": the ask, then the handoff envelope on lines of its own, typed unsent by send_draft (lost_click). The
    # request id is random in production; a fixed one stands in. The draft goes through the real send_draft against a
    # fake herdr, and what it types must be the draft, line by line. Agent rows (flight_item) carry send='' and are never typed.
    for number, t in enumerate(every):
        draft = pane.lost_asks(t)['sub'] + '\n' + pane.agents_lib.handoff_envelope('synthetic-session', 'req-0123456789abcdef', f'{number + 1:012x}')
        typed = []
        saved = pane.herdr, pane.claude_in_front
        pane.herdr, pane.claude_in_front = (lambda *args: typed.append(args) or ''), (lambda agent: True)
        try:
            went = pane.send_draft('agent', draft, 'agent')
        finally:
            pane.herdr, pane.claude_in_front = saved
        lines = [args[-1] for args in typed if args[1] == 'send-text']
        if not went or lines != [line for line in draft.split('\n') if line]:
            raise SystemExit(f'draft-{number}: send_draft typed something other than the draft: {lines[:2]!r}')
        record(f'draft-sub-{number}', 'lost_click:send_draft', draft)

    # the other stages of the card, each as compact_section writes it, and a card draft cut at 1,200
    rec = lambda event, at, **more: dict(kind='compact', event=event, at=at, session='s', ctx_before=278_129, why='w', **more)
    for name, records in (
        ('soon', [rec('countdown', 1005.0, ends_at=1025.0)]),
        ('running', [rec('start', 1025.0)]),
        ('skipped', [rec('skip', 1030.0, reason='a hook said no')]),
        ('skipped-long', [rec('skip', 1030.0, reason=long * 3)]),
        ('done', [rec('compact', 1030.0, outcome='compacted', ctx_after=21_730)]),
    ):
        card = pane.compaction_card(records, [], 1031.0 if name != 'soon' else 1011.0)
        record(f'card-{name}', 'compact_section', pane.compact_section(card)['items'][0]['send'])

    # page_default: what CSR told Claude (recap, with and without its Full state), the last session, what it left over
    told = ('CSR ENDLESS MEMORY ACTIVE — every past session in this project is indexed and searchable.\n\n'
            'recap [5h ago]: Fix the importer span ownership: Preserve owned spans (abc1234). [[CSR:RECAP]]\n'
            'Dreamt: scrub went stale (2026-10-13, abc1234).\n'
            f'Full state: csr_reflect_on_past("conv_{PAST}")\n')
    told_bare = 'recap [5h ago]: Fix the importer span ownership. [[CSR:RECAP]]\n'
    told_plain = 'Welcome back. Nothing is pending.\n'
    told_long = ('recap [5h ago]: ' + 'Fix the importer span ownership and keep every retained byte. ' * 11 + '[[CSR:RECAP]]\n'
                 f'Full state: csr_reflect_on_past("conv_{PAST}")\n')
    facts = dict(id=PAST, path='/x', mtime=1.0, title='Fix scrub', first=NOW - 7200, last=NOW - 600, active=720.0,
                 prompts=6, compacts=1, peak=431_513, floor=73_120, learned=4_213, handed=2, asked=1,
                 series=[[NOW - 7000 + 600 * k, 60_000 + 3_000 * k] for k in range(10)], prompt_times=[], compactions=[],
                 todos=[], said=[NOW - 600, 'words'], typed=[NOW - 700, 'words'])
    for name, text, title in (('recap-full', told, 'Fix scrub'), ('recap-bare', told_bare, 'Fix scrub'),
                              ('recap-plain', told_plain, 'Fix “scrub” and the importer'),
                              ('long-title', told_bare, long), ('long-recap', told_long, 'Fix scrub')):
        built, data, s = page()
        s.update(id='sess-live', title=None, compact_sizes=[], main=[], turn_end=0.0, busy=False, asked=[],
                 told=dict(at=NOW - 3600, source='startup', text=text), last_session=dict(facts, title=title),
                 todos=[dict(subject='Run the clone replay', status='pending'),
                        dict(subject='Write “the” report', status='in_progress'),
                        dict(subject='x' * 800, status='pending')],
                 learned=dict(chunks=[], anchors=[], symbols=0, turns=[], memories=[]))
        default = pane.page_default(built, data, s, NOW, 24)
        for number, (section, item) in enumerate(items_of(default)):
            if section in ('told', 'last', 'todos') and item.get('send'):
                record(f'default-{name}-{section}-{number}', f'page_default:{section}', item['send'])

    # page_log: code that changed under past sessions, with and without named files
    shifts = [dict(date='2026-10-13', oid='abc1234def5678', file='csr-engine/src/import/mod.rs', symbol='sanitize_content',
                   verb='changed', subject='fix importer', at=NOW - 100, id=1, wids=[]),
              dict(date='2026-10-12', oid='0123456fedcba98', file='csr-engine/src/import/scrub.rs', symbol=None,
                   verb='changed', subject='tighten “scrub”', at=NOW - 200, id=2, wids=[])]
    wide = [dict(date='2026-10-11', oid='fedcba9876543210', file=f'csr-engine/src/import/module_{k}.rs', verb='changed',
                 symbol='symbol_' + chr(97 + k) * 260, subject='a wide change', at=NOW - 300, id=3 + k, wids=[])
            for k in range(3)]
    log = pane.page_log(dict(shifts=shifts + wide, total=5))
    for number, item in enumerate(log['sections'][0]['items']):
        record(f'log-{number}', 'page_log', item['send'])

    # lost_asks directly, for a thread the page may not show
    asks = pane.lost_asks(thread('Tidy', NOW - 60))
    record('lost-side', 'lost_asks', asks['side'])
    record('lost-sub', 'lost_asks', asks['sub'])

    with open(PANE, 'rb') as handle:
        digest = hashlib.sha256(handle.read()).hexdigest()
    ordered = [cases[key] for key in sorted(cases)]
    document = dict(schema='pane-emissions-v1', generator='plugins/csr-companion/tests/export-emissions.py',
                    source=dict(file='plugins/csr-companion/bin/pane.py', sha256=digest),
                    limits=dict(send=pane.PANE_SEND_MOST, card=pane.CARD_SEND_MOST, story=pane.STORY_SEND_MOST),
                    cases=ordered)
    return json.dumps(document, ensure_ascii=False, indent=1, sort_keys=True) + '\n'


def main(argv):
    if argv not in (['--write'], ['--check']):
        raise SystemExit(__doc__)
    text = export()
    stale = []
    for path in (PLUGIN_COPY, ENGINE_COPY):
        if argv == ['--write']:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, 'w', encoding='utf-8') as handle:
                handle.write(text)
        else:
            try:
                with open(path, encoding='utf-8') as handle:
                    if handle.read() != text:
                        stale.append(path)
            except OSError:
                stale.append(path)
    if stale:
        raise SystemExit('stale (run with --write): ' + ', '.join(os.path.relpath(p, ROOT) for p in stale))
    print(f'{len(cases)} sends, {sum(1 for c in cases.values() if c["cut"])} cut at their limit')


if __name__ == '__main__':
    main(sys.argv[1:])
