"""The pane's error boundary: a frame that raises shows a card and the pane goes on; the guard outlives an edit
that fails to load; the error log rotates.
python3 -I -m unittest discover -s tests -p 'test_*.py'"""
import importlib.util
import io
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('pane', os.path.join(HERE, '..', 'bin', 'pane.py'))
NAME = os.path.basename(spec.origin)  # pane.py here; the dev mod's copy has its own name
pane = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pane)
guard_spec = importlib.util.spec_from_file_location('pane_guard', os.path.join(HERE, '..', 'bin', 'pane_guard.py'))
guard = importlib.util.module_from_spec(guard_spec)
guard_spec.loader.exec_module(guard)

MISSING = "name 'TAPE_MIN' is not defined"
real_sleep = time.sleep  # the boundary tests set the pane's clock by hand; the heartbeat runs on real time
STATUSLINE = os.path.join(HERE, '..', 'bin', 'statusline.cjs')


@unittest.skipUnless(shutil.which('node'), 'node is not installed')
class StatusLineWrapper(unittest.TestCase):
    """PR #317 review: the status line wrapper wrote the wrapped command's stdout alone. A command that failed went
    blank under the prompt, and nothing said why."""

    def line(self, *command):
        home = tempfile.mkdtemp()  # no pane open here: the line passes through
        self.addCleanup(shutil.rmtree, home)
        return subprocess.run(['node', STATUSLINE, *command], input='{"session_id": "sess-1"}', capture_output=True,
                              text=True, env=dict(os.environ, HOME=home), timeout=30)

    def test_the_wrapped_commands_stderr_passes_through_and_a_line_exits_0(self):
        """Codex review #17: Claude Code shows a status line only after exit 0 (code.claude.com/docs/en/statusline,
        "Script errors or hangs"), so a command that printed its line and exited 3 blanked it."""
        ran = self.line('--sh', 'echo line; echo oops >&2; exit 3')
        self.assertEqual((ran.stdout, ran.stderr, ran.returncode), ('line\n', 'oops\n', 0))
        ran = self.line('--sh', 'echo oops >&2; exit 3')  # nothing to show: its own status says it failed
        self.assertEqual((ran.stdout, ran.stderr, ran.returncode), ('', 'oops\n', 3))
        ran = self.line('sh', '-c', 'echo fine')
        self.assertEqual((ran.stdout, ran.stderr, ran.returncode), ('fine\n', '', 0))

    def test_a_command_that_cannot_start_says_why_on_stderr(self):
        ran = self.line('/nonexistent/csr-statusline-command')
        self.assertEqual(ran.stdout, '')
        self.assertIn('statusline: spawnSync /nonexistent/csr-statusline-command ENOENT', ran.stderr)
        self.assertEqual(ran.returncode, 1)


class RuntimeBoundary(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.log = os.path.join(self.tmp.name, 'pane-errors.log')
        self.clock = [1000.0]
        saved = dict(frame=pane.Pane.frame, select=pane.select.select, time=pane.time.time, isatty=pane.os.isatty,
                     log=pane.ERROR_LOG, gone=pane.compaction_gone, restart=pane.restart)
        self.addCleanup(lambda: (setattr(pane.Pane, 'frame', saved['frame']), setattr(pane.select, 'select', saved['select']),
                                 setattr(pane.time, 'time', saved['time']), setattr(pane.os, 'isatty', saved['isatty']),
                                 setattr(pane, 'ERROR_LOG', saved['log']), setattr(pane, 'compaction_gone', saved['gone']),
                                 setattr(pane, 'restart', saved['restart'])))
        pane.time.time = lambda: self.clock[0]
        columns = os.environ.get('COLUMNS')
        os.environ['COLUMNS'] = '100'  # the card on one row
        self.addCleanup(lambda: os.environ.pop('COLUMNS') if columns is None else os.environ.__setitem__('COLUMNS', columns))
        pane.os.isatty = lambda fd: False  # no keyboard: the loop only waits
        pane.ERROR_LOG = self.log
        pane.compaction_gone = lambda *a, **k: False
        self.restarts = []
        pane.restart = lambda source, failed=None: self.restarts.append(failed)

    def watch(self, frames, ticks=14):
        """watch() over a pane whose frame does what frames says, a second passing each tick; its stdout."""
        calls = []

        def frame(box, force):
            calls.append(force)
            if frames(len(calls)):
                raise NameError(MISSING)
        pane.Pane.frame = frame
        waited = []

        def select(r, w, x, timeout):
            waited.append(timeout)
            self.clock[0] += 1.1
            if len(waited) >= ticks:
                raise KeyboardInterrupt
            return [], [], []
        pane.select.select = select
        out, real = io.StringIO(), sys.stdout
        sys.stdout = out
        try:
            pane.watch('/tmp/r', 10, None)
        finally:
            sys.stdout = real
        return out.getvalue(), calls

    def test_a_frame_that_raises_draws_the_card_and_the_loop_goes_on(self):
        out, calls = self.watch(lambda n: n <= 2)  # the first two frames fail, then they draw
        self.assertIn('\x1b[1;1H', out)  # the card, on the top row
        self.assertRegex(out, 'CSR pane error · NameError: TAPE_MIN · ' + re.escape(NAME) + r':\d+ · retry in 1s')
        self.assertIn('· retry in 2s', out)  # the second failure waits longer
        self.assertEqual(out.count('CSR pane error'), 2)  # the third frame drew: no card after it
        self.assertGreater(len(calls), 3)  # the loop went on drawing
        self.assertEqual(self.restarts, [])
        with open(self.log, encoding='utf-8') as handle:
            logged = handle.read()
        self.assertEqual(logged.count('Traceback (most recent call last)'), 2)
        self.assertIn(f'NameError: {MISSING}', logged)

    def test_a_narrow_pane_wraps_the_card_over_its_top_rows(self):
        os.environ['COLUMNS'] = '12'
        box = pane.Boundary(clock=lambda: 0.0)
        box.last = 'NameError: TAPE_MIN · pane.py:3871'
        card = box.card(5)
        self.assertEqual(card.count('H\x1b[48;2'), 4)  # four rows, each placed at its own row
        for row in range(4):
            self.assertIn(f'\x1b[{row + 1};1H', card)
        for line in pane.re.findall(r'\x1b\[1m( [^\x1b]*)\x1b\[K', card):
            self.assertLessEqual(len(line), 12)

    def test_retries_wait_out_the_backoff(self):
        _, calls = self.watch(lambda n: True, ticks=6)
        # 1.1 s a tick: the first retry is due after 1 s, the second after 2 s (two ticks)
        self.assertEqual(len(calls), 3)

    def test_three_failures_in_a_row_start_the_pane_again(self):
        out, calls = self.watch(lambda n: True)
        self.assertEqual(len(calls), pane.RELAUNCH_AFTER)
        self.assertEqual(len(self.restarts), 1)
        self.assertIn('NameError: TAPE_MIN', self.restarts[0])

    def test_a_long_frame_never_ages_the_pid_file_and_quitting_stops_the_heartbeat(self):
        """PR #317 review: the pid file was touched only after a frame returned, so a frame over 30 s (a transcript
        read, a busy database) aged it, and statusline.cjs moved Claude's status line back under the prompt."""
        folder = os.path.join(self.tmp.name, 'panes')
        saved = pane.PANES, pane.HEARTBEAT_EVERY
        self.addCleanup(lambda: (setattr(pane, 'PANES', saved[0]), setattr(pane, 'HEARTBEAT_EVERY', saved[1])))
        pane.PANES, pane.HEARTBEAT_EVERY = folder, 0.02
        mine = os.path.join(folder, 'sess-hb.json')
        seen = {}

        def frame(box, force):
            aged = os.path.getmtime(mine) - 100
            os.utime(mine, (aged, aged))
            real_sleep(0.3)  # a long frame: the loop does not come round
            seen['during'] = os.path.getmtime(mine) - aged
        pane.Pane.frame = frame
        pane.select.select = lambda *a: (_ for _ in ()).throw(KeyboardInterrupt)
        out, real = io.StringIO(), sys.stdout
        sys.stdout = out
        try:
            pane.watch('/tmp/r', 10, 'sess-hb')
        finally:
            sys.stdout = real
        self.assertGreater(seen['during'], 50)  # touched while the frame ran
        self.assertFalse(os.path.exists(mine))  # quit: the file goes
        self.assertFalse(any(t.name == 'csr-pane-heartbeat' and t.is_alive() for t in pane.threading.enumerate()))

    def test_the_heartbeat_stops_when_the_loop_stalls_and_when_stopped(self):
        path = os.path.join(self.tmp.name, 'pid.json')
        open(path, 'w').close()
        beat = pane.Heartbeat(path, every=0.02, stall=0.15).start()
        self.addCleanup(beat.stop)
        aged = os.path.getmtime(path) - 100
        os.utime(path, (aged, aged))
        real_sleep(0.1)
        self.assertGreater(os.path.getmtime(path) - aged, 50)  # the loop came round lately: touched
        real_sleep(0.2)  # no loop for longer than stall: a hung pane is let go
        os.utime(path, (aged, aged))
        real_sleep(0.1)
        self.assertEqual(os.path.getmtime(path), aged)
        beat.alive()
        real_sleep(0.1)
        self.assertGreater(os.path.getmtime(path) - aged, 50)  # it came round again
        beat.stop()
        real_sleep(0.05)
        os.utime(path, (aged, aged))
        real_sleep(0.1)
        self.assertEqual(os.path.getmtime(path), aged)

    def test_under_the_guard_a_failure_exits_for_it_to_retry(self):
        saved = os.environ.get(pane.GUARDED)
        os.environ[pane.GUARDED] = '1'
        err, real = io.StringIO(), sys.stderr
        sys.stderr = err
        try:
            with self.assertRaises(SystemExit) as done:
                type(self).real_restart('/tmp/pane.py', 'NameError: TAPE_MIN · pane.py:12')  # not setUp's stub
        finally:
            sys.stderr = real
            os.environ.pop(pane.GUARDED) if saved is None else os.environ.__setitem__(pane.GUARDED, saved)
        self.assertEqual(done.exception.code, pane.RELAUNCH)
        self.assertEqual(err.getvalue(), 'NameError: TAPE_MIN · pane.py:12\n')

    real_restart = staticmethod(pane.restart)


class Guard(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = os.path.join(self.tmp.name, 'pane.py')
        saved = guard.LOG
        guard.LOG = os.path.join(self.tmp.name, 'pane-errors.log')
        self.addCleanup(lambda: setattr(guard, 'LOG', saved))

    def put(self, text):
        with open(self.path, 'w', encoding='utf-8') as handle:
            handle.write(text)
        now = time.time()
        os.utime(self.path, (now, now))

    def recover(self, broken):
        """The guard over a broken pane.py: what its screen said, and its exit, once the file is fixed."""
        self.put(broken)
        waits = []

        def waiter(path, seen, wait, keys):  # the fix lands: the file changes under the guard
            waits.append(seen)
            time.sleep(0.01)
            self.put('import sys\nsys.exit(0)\n')
            return True
        out, real = io.StringIO(), sys.stdout
        sys.stdout = out
        try:
            code = guard.main([], path=self.path, wait=30, waiter=waiter)
        finally:
            sys.stdout = real
        self.assertEqual(len(waits), 1)  # one error screen, then the fixed file ran
        return out.getvalue(), code

    def test_a_file_that_does_not_compile_shows_the_error_and_recovers_when_fixed(self):
        screen, code = self.recover('def broken(:\n    pass\n')
        self.assertEqual(code, 0)
        self.assertIn('CSR pane error', screen)
        self.assertIn('SyntaxError', screen)
        self.assertIn(f'{NAME}:1', screen)
        self.assertIn(f'waiting for {NAME} to change · retry in 30s', screen)
        with open(guard.LOG, encoding='utf-8') as handle:
            self.assertIn('SyntaxError', handle.read())

    def test_a_file_that_dies_as_it_loads_shows_its_line_and_recovers(self):
        screen, code = self.recover('import sys\nTAPE_MIN[0]\n')
        self.assertEqual(code, 0)
        self.assertIn(f'NameError: {MISSING}', screen)
        self.assertIn(f'{NAME}:2', screen)

    def test_a_pane_that_asks_to_start_again_is_retried_and_a_clean_exit_ends_it(self):
        screen, code = self.recover("import sys\nsys.stderr.write('NameError: TAPE_MIN · pane.py:3871\\n')\nsys.exit(75)\n")
        self.assertEqual(code, 0)
        self.assertIn('NameError: TAPE_MIN · pane.py:3871', screen)

    def test_q_while_it_waits_ends_the_guard(self):
        self.put('def broken(:\n')
        out, real = io.StringIO(), sys.stdout
        sys.stdout = out
        try:
            code = guard.main([], path=self.path, wait=30, waiter=lambda *a: False)
        finally:
            sys.stdout = real
        self.assertEqual(code, 0)

    def test_the_wait_ends_when_the_file_changes_or_the_time_is_up(self):
        self.put('x = 1\n')
        seen = guard.mtime(self.path)
        started = time.time()
        self.assertTrue(guard.changed(self.path, seen, 0.2, None))  # nothing changed: the time ran out
        self.assertLess(time.time() - started, 2.5)
        self.put('x = 2\n')
        self.assertTrue(guard.changed(self.path, seen, 30, None))  # changed: at once


class LogRotates(unittest.TestCase):
    def test_the_log_moves_aside_at_one_megabyte_and_starts_again(self):
        for write in (lambda text, path: pane.log_error(text, path), lambda text, path: guard.log(text, path)):
            with tempfile.TemporaryDirectory() as tmp:
                path = os.path.join(tmp, 'pane-errors.log')
                write('first', path)
                self.assertLess(os.path.getsize(path), 100)
                with open(path, 'a') as handle:
                    handle.write('x' * pane.LOG_MAX)
                full = os.path.getsize(path)
                write('after', path)
                self.assertEqual(os.path.getsize(path + '.1'), full)
                with open(path, encoding='utf-8') as handle:
                    fresh = handle.read()
                self.assertIn('after', fresh)
                self.assertLess(len(fresh), 100)
        self.assertEqual(pane.LOG_MAX, 1 << 20)
        self.assertEqual(guard.LOG_MAX, pane.LOG_MAX)


if __name__ == '__main__':
    unittest.main()
