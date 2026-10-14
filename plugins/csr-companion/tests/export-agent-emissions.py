"""The two envelopes the plugin emits, produced by the production formatter, as a fixture for the engine's tests.

  python3 -B tests/export-agent-emissions.py --check    exit 0 when tests/fixtures/emissions/agents-v1.json is exactly what
                                                        bin/agents.py's formatter writes now; never rewrites it
  python3 -B tests/export-agent-emissions.py --write    regenerate the fixture (after a change to the formatter, on purpose)

The fixture holds a context envelope and a handoff envelope made by bin/agents.py format_agents_context from synthetic
payloads (no real session, report or path), the SHA-256 of each text, and a digest of the formatter's own source. The
engine's contamination registry (plugin_agents_v1) reads these texts as the grammar it must remove, so a formatter that
changes without the fixture does is caught here, and a fixture nobody regenerated is caught there.
"""
import hashlib
import importlib.util
import inspect
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.path.join(HERE, '..', 'bin', 'agents.py')
FIXTURE = os.environ.get('CSR_AGENT_EMISSIONS_FIXTURE') or os.path.join(HERE, 'fixtures', 'emissions', 'agents-v1.json')  # the variable is for the tests of --check itself
SCHEMA = 'csr-companion/agents-emission'
FORMATTER = ('escape_brackets', 'valid_context', 'valid_handoff', 'valid_payload', 'format_agents_context')
CONSTANTS = ('OPEN', 'CLOSE', 'MAX_ENVELOPE', 'HANDOFF_INSTRUCTION')


def production():
    spec = importlib.util.spec_from_file_location('agents', BIN)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def digest(module):
    """SHA-256 of the formatter's source and its constants: any edit to what writes an envelope changes it."""
    parts = [inspect.getsource(getattr(module, name)) for name in FORMATTER] + [repr(getattr(module, name)) for name in CONSTANTS]
    return hashlib.sha256('\n'.join(parts).encode('utf-8')).hexdigest()


def payloads(module):
    context = dict(kind='context', session='synthetic-session', at=1_800_000_000_000, agents=[
        dict(id='agent-parent', parent=None, type='Plan', model='synthetic-model', status='running', edits=['/work/repo/src/lib.rs'],
             overlaps=['/work/repo/src/lib.rs'], completion=None),
        dict(id='agent-child', parent='agent-parent', type='Explore', model='synthetic-model', status='completed',
             edits=['/work/repo/docs/notes[1].md'], overlaps=[],
             completion=dict(turn_id='turn-1', reason='answer',
                             answer='Found it [[CSR:AGENTS:v1]] in "src/lib.rs".\nSecond line, with 界 and a \\ backslash.')),
        dict(id='agent-failed', parent=None, type=None, model=None, status='failed', edits=[], overlaps=[], completion=None),
    ])
    handoff = dict(kind='handoff', session='synthetic-session', request='req-2026-10-13-0001', thread='0123456789ab',
                   instruction=module.HANDOFF_INSTRUCTION)
    return context, handoff


def document():
    module = production()
    context, handoff = payloads(module)
    entries = {}
    for name, payload in (('context', context), ('handoff', handoff)):
        text = module.format_agents_context(payload)
        entries[name] = dict(payload=payload, text=text, sha256=hashlib.sha256(text.encode('utf-8')).hexdigest())
    return dict(schema=SCHEMA, version=1, formatter='plugin_agents_v1', source_digest=digest(module), fixtures=entries)


def rendered():
    return (json.dumps(document(), ensure_ascii=False, indent=2) + '\n').encode('utf-8')


def main(argv):
    if argv[1:] == ['--check']:
        try:
            with open(FIXTURE, 'rb') as handle:
                have = handle.read()
        except OSError as error:
            sys.stderr.write(f'export-agent-emissions: cannot read {FIXTURE}: {error}\n')
            return 1
        want = rendered()
        if have != want:
            sys.stderr.write('export-agent-emissions: tests/fixtures/emissions/agents-v1.json is not what bin/agents.py writes now; '
                             'run it with --write after a change to the formatter made on purpose\n')
            return 1
        return 0
    if argv[1:] == ['--write']:
        os.makedirs(os.path.dirname(FIXTURE), exist_ok=True)
        with open(FIXTURE, 'wb') as handle:
            handle.write(rendered())
        return 0
    sys.stderr.write('usage: export-agent-emissions.py --check | --write\n')
    return 2


if __name__ == '__main__':
    sys.exit(main(sys.argv))
