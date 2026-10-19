"""Development-only capture of the Python mode helpers exercised by their existing tests."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest


def capture(repo, here):
    scratch = repo / 'csr-engine/target/oracle-temp'
    scratch.mkdir(exist_ok=True)
    tempfile.tempdir = str(scratch)
    spec = importlib.util.spec_from_file_location('pane_tests', repo / 'plugins/csr-companion/tests/test_pane.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    pane = module.pane
    records = []
    current = ['']

    def record(name, inputs, expected):
        records.append(json.loads(json.dumps(dict(test=current[0], mode=name, inputs=inputs, expected=expected))))

    def wrap(name):
        original = getattr(pane, name)
        def called(*args, **kw):
            result = original(*args, **kw)
            if name == 'lost_threads':
                record(name, dict(summaries=args[0], typed=args[1] if len(args) > 1 else kw.get('typed', [])), result)
            elif name == 'lost_found':
                names = ['summaries', 'typed', 'carried', 'project', 'handoffs']
                values = dict(zip(names, args))
                values.update(kw)
                project = values.get('project')
                values.update(said=pane.lost_log(project))
                record(name, values, result)
            elif name == 'terms_looked_up':
                record(name, dict(text=Path(args[0]).read_text()), result)
            elif name == 'past_lines':
                record(name, dict(text=Path(args[0]).read_text(), session=args[1]), [dict(result[0]), result[1]])
            elif name == 'dream_rows':
                record(name, dict(dreams=args[0]), result)
            elif name == 'past_rows':
                record(name, dict(near=args[0], now=args[1]), result)
            return result
        setattr(pane, name, called)

    for name in ['lost_threads', 'lost_found', 'terms_looked_up', 'past_lines', 'dream_rows', 'past_rows']:
        wrap(name)
    suite = unittest.TestSuite()
    for cls in ['LostAndFound', 'LostAndFoundAnswered', 'LostAndFoundHanded', 'CarryOver',
                'CarryOverRegressions', 'SaidItInSessions', 'PastLines', 'OldLines', 'SampleDream']:
        suite.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(getattr(module, cls)))

    class Result(unittest.TextTestResult):
        def startTest(self, test):
            current[0] = test._testMethodName
            super().startTest(test)
    stream = io.StringIO()
    with contextlib.redirect_stdout(stream), contextlib.redirect_stderr(stream):
        result = unittest.TextTestRunner(stream=stream, resultclass=Result).run(suite)
    if not result.wasSuccessful():
        raise RuntimeError(stream.getvalue())
    (here / 'python-cases.json').write_text(json.dumps(records, indent=2) + '\n')
    tests = list(dict.fromkeys(r['test'] for r in records))
    body = '\n'.join(f'    #[test]\n    fn {name}() {{ check("{name}"); }}' for name in tests)
    (repo / 'csr-engine/tests/support/companion_python_cases.rs').write_text(
        '// Captured by fixtures/companion/cases.py from the original pytest cases.\n'
        '#[cfg(test)]\nmod cases {\n    include!("companion_cases.rs");\n' + body + '\n}\n')
    return len(tests), len(records)
