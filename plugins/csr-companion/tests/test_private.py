"""What the companion keeps on disk is private (bin/private.py): 0700 folders, 0600 files, no credentials in it.

The Codex review of the imported plugin: dreams.jsonl, the carried threads and the previous-session facts hold text
from your transcripts, and open()/makedirs() under the usual umask 022 left them readable by every local user."""
import importlib.util
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.path.join(HERE, '..', 'bin')


def load(name):
    spec = importlib.util.spec_from_file_location(name, os.path.join(BIN, f'{name}.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


private = load('private')
pane = load('pane')


def mode(path):
    return stat.S_IMODE(os.stat(path).st_mode)


class Umask022(unittest.TestCase):
    """Every test runs the way a default login shell does: umask 022, a fresh HOME."""

    def setUp(self):
        self.old = os.umask(0o022)
        self.home = tempfile.mkdtemp(prefix='csr-private-')
        self.root = os.path.join(self.home, '.claude-self-reflect', 'csr-companion')
        self.addCleanup(shutil.rmtree, self.home, True)
        self.addCleanup(os.umask, self.old)


class Helpers(Umask022):
    def test_folders_are_0700_from_the_root_down_and_files_0600(self):
        path = os.path.join(self.root, 'carry', 'deep', 'a.json')
        private.write_atomic(path, '{}', root=self.root)
        for folder in (self.root, os.path.join(self.root, 'carry'), os.path.join(self.root, 'carry', 'deep')):
            self.assertEqual(mode(folder), 0o700, folder)
        self.assertEqual(mode(path), 0o600)
        self.assertEqual(os.listdir(os.path.dirname(path)), ['a.json'])  # the temp file is gone

    def test_append_and_open_private_create_0600_and_keep_appending(self):
        path = os.path.join(self.root, 'dreams.jsonl')
        private.append(path, 'one\n', root=self.root)
        private.append(path, 'two\n', root=self.root)
        with private.open_private(os.path.join(self.root, 'w.txt'), 'w', root=self.root) as handle:
            handle.write('x')
        self.assertEqual(open(path).read(), 'one\ntwo\n')
        self.assertEqual((mode(path), mode(os.path.join(self.root, 'w.txt'))), (0o600, 0o600))

    def test_a_0644_file_and_a_0755_folder_made_earlier_are_tightened_on_the_next_write(self):
        os.makedirs(os.path.join(self.root, 'compacting'), mode=0o755)
        old = os.path.join(self.root, 'dreams.jsonl')
        replaced = os.path.join(self.root, 'compacting', 'mine.json')
        for path in (old, replaced):
            with open(path, 'w') as handle:
                handle.write('old\n')
            os.chmod(path, 0o644)
        os.chmod(self.root, 0o755)
        private.append(old, 'new\n', root=self.root)
        private.write_atomic(replaced, 'new', root=self.root)
        self.assertEqual(open(old).read(), 'old\nnew\n')  # appended to, not replaced
        self.assertEqual([mode(self.root), mode(os.path.dirname(replaced)), mode(old), mode(replaced)], [0o700, 0o700, 0o600, 0o600])

    def test_the_first_touch_also_tightens_files_nothing_here_writes_again(self):
        os.makedirs(os.path.join(self.root, 'statusline'), mode=0o755)
        left = os.path.join(self.root, 'statusline', 's.json')
        with open(left, 'w') as handle:
            handle.write('{}')
        os.chmod(left, 0o644)
        private.ensure_dir(self.root, root=self.root)
        self.assertEqual((mode(left), mode(os.path.dirname(left))), (0o600, 0o700))

    def test_a_path_outside_the_root_makes_only_its_own_folder_private(self):
        out = os.path.join(self.home, 'elsewhere', 'lease')
        private.write_atomic(os.path.join(out, 'x.json'), '{}', root=self.root)
        self.assertEqual((mode(out), mode(os.path.join(out, 'x.json'))), (0o700, 0o600))
        self.assertEqual(mode(os.path.join(self.home, 'elsewhere')), 0o755)

    def test_the_script_writes_what_stdin_holds_privately_and_redacts_on_request(self):
        env = dict(os.environ, HOME=self.home)
        path = os.path.join(self.root, 'compact-plan', 's.json')
        text = json.dumps(dict(text='use sk-ant-api03-abcdefghijklmnopqrstuvwx'))
        for flags, want in (([], text), (['--redact'], json.dumps(dict(text='use [redacted]')))):
            ran = subprocess.run([sys.executable, '-I', os.path.join(BIN, 'private.py'), 'write', path, *flags],
                                 input=text.encode(), capture_output=True, env=env)
            self.assertEqual(ran.returncode, 0, ran.stderr)
            self.assertEqual(open(path).read(), want)
            self.assertEqual((mode(path), mode(os.path.dirname(path)), mode(self.root)), (0o600, 0o700, 0o700))

    def test_the_script_tightens_an_old_tree_and_refuses_what_it_does_not_know(self):
        os.makedirs(os.path.join(self.root, 'panes'), mode=0o755)
        left = os.path.join(self.root, 'panes', 'p.json')
        open(left, 'w').close()
        os.chmod(left, 0o644)
        env = dict(os.environ, HOME=self.home)
        ran = subprocess.run([sys.executable, '-I', os.path.join(BIN, 'private.py'), 'tighten'], env=env, capture_output=True)
        self.assertEqual((ran.returncode, mode(left), mode(os.path.dirname(left))), (0, 0o600, 0o700))
        ran = subprocess.run([sys.executable, '-I', os.path.join(BIN, 'private.py'), 'nope'], env=env, capture_output=True)
        self.assertEqual(ran.returncode, 2)


# (what was said, what is kept)
REDACTION = [
    ('use sk-ant-api03-AbCdEf0123456789_-xyz here', 'use [redacted] here'),
    ('OPENAI sk-proj-0123456789abcdefghij0123', 'OPENAI [redacted]'),
    ('xai-0123456789abcdefghijKLMN', '[redacted]'),
    ('token ghp_0123456789abcdefghijABCDEFGHIJ0123 ok', 'token [redacted] ok'),
    ('github_pat_11ABCDEFG0123456789_abcdefghijklmnop', '[redacted]'),
    ('id AKIAIOSFODNN7EXAMPLE and ASIAIOSFODNN7EXAMPLE', 'id [redacted] and [redacted]'),
    ('xoxb-1234567890-abcdefghij', '[redacted]'),
    ('curl -H "Authorization: Bearer abc123.def456-ghi789" x', 'curl -H "Authorization: Bearer [redacted]" x'),
    ('authorization: basic dXNlcjpwYXNzd29yZA==', 'authorization: basic [redacted]'),
    ('password=hunter2 next', 'password=[redacted] next'),
    ('DB_PASSWORD: "correct horse battery"', 'DB_PASSWORD: "[redacted]"'),
    ("API_KEY='abc 123'", "API_KEY='[redacted]'"),
    ('{"token": "abc.def", "ok": 1}', '{"token": "[redacted]", "ok": 1}'),
    ('?access_token=abc123&x=1', '?access_token=[redacted]&x=1'),
    ('-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nabc\n-----END RSA PRIVATE KEY----- after', '[redacted] after'),
    ('-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk', '[redacted]'),  # an excerpt cut before the END line
    ('eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpM', '[redacted]'),
    # what is not a credential stays
    ('the token budget is 10% of Claude tokens: 120k', 'the token budget is 10% of Claude tokens: 120k'),
    ('sk-short and ask-me-anything and task-12345678901234567890', 'sk-short and ask-me-anything and task-12345678901234567890'),
    ('git push origin feat/plugin', 'git push origin feat/plugin'),
]


class Redaction(unittest.TestCase):
    def test_table(self):
        for said, kept in REDACTION:
            with self.subTest(said=said):
                self.assertEqual(private.redact(said), kept)
                self.assertEqual(private.redact(kept), kept)  # twice changes nothing more

    def test_every_string_in_json_shaped_data_but_not_its_keys(self):
        got = private.redact_value({'password': 'sk-0123456789abcdefghij', 'n': 3, 'xs': ['Bearer abcdefgh12345', None, {'a': 'token=zz'}]})
        self.assertEqual(got, {'password': '[redacted]', 'n': 3, 'xs': ['Bearer [redacted]', None, {'a': 'token=[redacted]'}]})
        self.assertEqual(json.loads(private.dumps({'line': 'password=x'})), {'line': 'password=[redacted]'})


class PaneWritesArePrivate(Umask022):
    """Each kind of record the pane keeps, written through the pane's own functions, under umask 022."""

    def setUp(self):
        super().setUp()
        patched = dict(KEEP=self.root, DREAM_LOG=os.path.join(self.root, 'dreams.jsonl'),
                       LIKES=os.path.join(self.root, 'likes.jsonl'), LOST_LOG=os.path.join(self.root, 'lost-found.jsonl'),
                       COACH_KEPT=os.path.join(self.root, 'coach.json'), PANES=os.path.join(self.root, 'panes'),
                       ERROR_LOG=os.path.join(self.root, 'pane-errors.log'))
        for name, value in patched.items():
            old = getattr(pane, name)
            setattr(pane, name, value)
            self.addCleanup(setattr, pane, name, old)

    def assert_private(self, path):
        self.assertEqual(mode(path), 0o600, path)
        folder = os.path.dirname(path)
        while len(folder) >= len(self.root):
            self.assertEqual(mode(folder), 0o700, folder)
            folder = os.path.dirname(folder)

    def test_the_dream_log_the_likes_and_the_lost_log(self):
        pane.keep(dict(kind='dream', line='use password=hunter2 and sk-ant-api03-0123456789abcdefghij'))
        pane.keep_like('k', 'dream', 'Bearer abcdefgh12345 title', True, 1.0)
        pane.keep_lost('k', 'dropped', 1.0, 'proj', text='token=abc')
        for path in (pane.DREAM_LOG, pane.LIKES, pane.LOST_LOG):
            self.assert_private(path)
            body = open(path).read()
            self.assertNotRegex(body, r'hunter2|sk-ant|abcdefgh12345|token=abc')
            self.assertIn('[redacted]', body)

    def test_the_carry_and_the_previous_facts(self):
        pane.keep_json('carry', 's1', [dict(text='remember token=abc123 and sk-0123456789abcdefghij')])
        pane.keep_json('previous', 's1', dict(said=[0, 'password: swordfish']))
        for path in (os.path.join(self.root, 'carry', 's1.json'), os.path.join(self.root, 'previous', 's1.json')):
            self.assert_private(path)
            self.assertNotRegex(open(path).read(), r'abc123|sk-0123|swordfish')

    def test_the_error_log_the_spend_file_and_the_compaction_caches(self):
        pane.log_error('Traceback token=abc')
        self.assert_private(pane.ERROR_LOG)
        spend = pane.Spend(path=os.path.join(self.root, 'spend.json'), projects=self.home)
        spend.begin(1.0)
        spend.dirty = True
        spend.save(100.0, force=True)
        self.assert_private(os.path.join(self.root, 'spend.json'))
        pane.keep_compact_usual(5.0, projects=self.home, path=os.path.join(self.root, 'usual.json'), logs=self.home)
        self.assert_private(os.path.join(self.root, 'usual.json'))

    def test_an_old_world_readable_dream_log_is_tightened_by_the_next_dream(self):
        os.makedirs(self.root, mode=0o755)
        with open(pane.DREAM_LOG, 'w') as handle:
            handle.write('{"old": 1}\n')
        os.chmod(pane.DREAM_LOG, 0o644)
        pane.keep(dict(kind='dream', line='new'))
        self.assert_private(pane.DREAM_LOG)
        self.assertEqual(len(open(pane.DREAM_LOG).read().splitlines()), 2)


@unittest.skipUnless(shutil.which('node'), 'node is not installed')
class StatuslineWrapper(Umask022):
    def test_the_status_line_it_keeps_is_0600_in_0700_folders_and_an_old_one_is_tightened(self):
        keep = self.root
        os.makedirs(os.path.join(keep, 'panes'), mode=0o755)
        os.makedirs(os.path.join(keep, 'statusline'), mode=0o755)
        with open(os.path.join(keep, 'panes', 's1.json'), 'w') as handle:
            json.dump(dict(pid=os.getpid()), handle)
        old = os.path.join(keep, 'statusline', 's1.json')
        with open(old, 'w') as handle:
            handle.write('{}')
        os.chmod(old, 0o644)
        os.chmod(keep, 0o755)
        ran = subprocess.run(['node', os.path.join(BIN, 'statusline.cjs'), '--sh', 'cat >/dev/null; echo drawn'],
                             input=json.dumps(dict(session_id='s1')).encode(), capture_output=True,
                             env=dict(os.environ, HOME=self.home), timeout=30)
        self.assertEqual(ran.returncode, 0, ran.stderr)
        self.assertEqual([mode(keep), mode(os.path.join(keep, 'statusline')), mode(old)], [0o700, 0o700, 0o600])
        self.assertEqual(json.load(open(old))['line'].strip(), 'drawn')  # it was rewritten


class NoWriteBypassesTheHelper(unittest.TestCase):
    """A write added later that opens a file with plain open() would be 0644 again: this finds it."""

    WRITE = re.compile(r"""(?x)
        \bopen\([^()]*(?:\([^()]*\)[^()]*)*,\s*['"](?:a|w|wb|ab|w\+|a\+)['"]
        | \.write_text\( | \.write_bytes\( | os\.makedirs\( | os\.mkdir\( | os\.open\(""")
    # bin/private.py is the helper; the --png export writes where the person said, not into the companion's folder.

    def test_no_python_file_but_the_helper_opens_a_file_for_writing_or_makes_a_folder(self):
        found = []
        for name in sorted(os.listdir(BIN)):
            if not name.endswith('.py') or name == 'private.py':
                continue
            with open(os.path.join(BIN, name), encoding='utf-8') as handle:
                for number, line in enumerate(handle, 1):
                    code = line.split('#', 1)[0]
                    if self.WRITE.search(code) and 'args.png' not in code and 'tempfile' not in code:
                        found.append(f'{name}:{number}: {line.strip()}')
        self.assertEqual(found, [])

    def test_no_hook_writes_into_the_companion_folder_with_fs_write(self):
        found = []
        for name in sorted(os.listdir(os.path.join(HERE, '..', 'hooks'))):
            if not name.endswith(('.ts', '.tsx')):
                continue
            with open(os.path.join(HERE, '..', 'hooks', name), encoding='utf-8') as handle:
                for number, line in enumerate(handle, 1):
                    if '$.fs.write(' in line and 'user file' not in line:
                        found.append(f'{name}:{number}: {line.strip()}')
        self.assertEqual(found, [])


if __name__ == '__main__':
    unittest.main()
