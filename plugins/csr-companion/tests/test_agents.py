"""The agents of a session: hooks/agents.ts's redaction held to bin/private.py's table, and (below) the pane's side of them."""
import json
import os
import re
import subprocess
from pathlib import Path

from test_r5 import typescript, unavailable  # noqa: F401  the compiler fixture the hook regressions share

ROOT = Path(__file__).resolve().parents[1]
TABLE = ROOT / 'tests' / 'redaction_table.json'

HOST = r'''
const fs = require('node:fs'), vm = require('node:vm')
const ts = require(process.argv[1])
const source = fs.readFileSync(process.argv[2], 'utf8')
const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText
const mod = { exports: {} }
vm.runInThisContext(`(function(require,module,exports){${js}\n})`)((name) => { throw new Error('unexpected import ' + name) }, mod, mod.exports)
const table = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'))
const wrong = table.filter(([input, expected]) => mod.exports.redactAgentValue(input) !== expected)
process.stdout.write(JSON.stringify(wrong.map(([input, expected]) => [input, expected, mod.exports.redactAgentValue(input)])))
'''


def test_agents_ts_redaction_matches_the_helpers_table(typescript):
    """The same table private.redact_value is held to (test_private.py): every pair comes out the same through
    hooks/agents.ts's redactAgentValue, which adds only KEY/TOKEN/SECRET assignment lines and 32-digit hex runs."""
    ran = subprocess.run(['node', '-e', HOST, typescript, str(ROOT / 'hooks' / 'agents.ts'), str(TABLE)],
                         capture_output=True, text=True, timeout=30)
    assert ran.returncode == 0, ran.stdout + ran.stderr
    assert json.loads(ran.stdout) == []


def test_the_hooks_test_holds_a_copy_of_the_table():
    """tests/agents.test.ts cannot import a .json file, so it carries the table: this keeps the copy whole."""
    text = (ROOT / 'tests' / 'agents.test.ts').read_text(encoding='utf-8')
    block = text[text.index('const table: Array<[string, string]> = ['):]
    block = block[:block.index('\n]\n')]
    copy = [json.loads(line.strip().rstrip(',')) for line in block.splitlines()[1:]]
    assert copy == json.loads(TABLE.read_text(encoding='utf-8'))
