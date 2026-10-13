"""The TypeScript regressions run by pytest (test_r5.py, test_r6.py) skip where there is no compiler, and must not skip in CI.

Codex review of the imported plugin, #3: the plugin job installed only pytest and Pillow, found no TypeScript compiler,
skipped every one of those tests, and went green having run none of them."""
import os
import re
import subprocess
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
CI_YML = os.path.join(HERE, '..', '..', '..', '.github', 'workflows', 'ci.yml')


def pytest_without_node(**env):
    """pytest on test_r5.py with no node on PATH, so the fixture finds nothing: -rs shows skips, the code says failure."""
    clean = {k: v for k, v in os.environ.items() if k != 'CI'}
    clean.update(PATH='/nonexistent', HOME=os.environ.get('HOME', '/tmp'), **env)
    return subprocess.run([sys.executable, '-m', 'pytest', '-q', '-rs', '-p', 'no:cacheprovider', os.path.join(HERE, 'test_r5.py')],
                          capture_output=True, text=True, env=clean, timeout=120)


class WithoutACompiler(unittest.TestCase):
    def test_off_ci_the_hook_regressions_skip(self):
        ran = pytest_without_node()
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertRegex(ran.stdout, r'\d+ skipped')
        self.assertNotIn('failed', ran.stdout.lower().replace('0 failed', ''))

    def test_in_ci_they_fail_and_say_why(self):
        ran = pytest_without_node(CI='true')
        self.assertNotEqual(ran.returncode, 0, ran.stdout)
        self.assertIn('Node is unavailable', ran.stdout)
        self.assertIn('CI installs a pinned typescript', ran.stdout)
        self.assertNotRegex(ran.stdout, r'\d+ skipped')


class TheWorkflow(unittest.TestCase):
    def test_the_plugin_job_installs_a_pinned_typescript_before_pytest(self):
        with open(CI_YML, encoding='utf-8') as handle:
            workflow = handle.read()
        job = workflow[workflow.index('\n  plugin:'):]
        install = re.search(r'npm install -g typescript@\d+\.\d+\.\d+\b', job)
        self.assertIsNotNone(install, 'typescript must be installed, at an exact version')
        self.assertLess(install.start(), job.index('python3 -m pytest'))
        self.assertLess(job.index('actions/setup-node'), install.start())  # npm is the pinned node's


if __name__ == '__main__':
    unittest.main()
