"""The plugin runs `python3` from PATH, and on macOS that can be the system Python 3.9. A bin/*.py that uses syntax
newer than that (a backslash or the same quote inside an f-string expression, which only 3.12 allows) does not
parse there, and the whole pane is gone. This compiles every script in bin/ with each older interpreter found."""
import glob
import os
import shutil
import subprocess
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.path.join(HERE, '..', 'bin')
PARSE = 'import ast,sys; ast.parse(open(sys.argv[1]).read())'
OLDER = ('/usr/bin/python3', 'python3.9', 'python3.10', 'python3.11')


def interpreters():
    found = {}
    for name in OLDER:
        path = name if os.path.isabs(name) else shutil.which(name)
        if path and os.access(path, os.X_OK):
            found[os.path.realpath(path)] = path
    return sorted(found.values())


class SyntaxFloor(unittest.TestCase):
    def test_every_script_in_bin_parses_on_the_older_pythons(self):
        found = interpreters()
        if not found:
            self.skipTest('no older Python found')
        scripts = sorted(glob.glob(os.path.join(BIN, '*.py')))
        self.assertTrue(scripts)
        for interpreter in found:
            for script in scripts:
                with self.subTest(python=interpreter, script=os.path.basename(script)):
                    out = subprocess.run([interpreter, '-c', PARSE, script], capture_output=True, text=True)
                    self.assertEqual(out.returncode, 0, f'{interpreter} cannot parse {script}:\n{out.stderr[-600:]}')


if __name__ == '__main__':
    unittest.main()
