"""One dreamer per session: the lease, taken in one process under an OS lock (bin/lease.py).

astra r2 #4 and r3 #4: lock schemes the mod built from mkdir, ln and mv let two copies take the lease at once
([true,true], then [true,false,true] with three): two paid looks. Here real processes race for it."""
import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
LEASE_PY = os.path.join(HERE, '..', 'bin', 'lease.py')
spec = importlib.util.spec_from_file_location('lease', LEASE_PY)
lease = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lease)

NOW = 1_760_000_000_000
DEV = ('/home/u/.claude/dev-mods/s/csr-shifts', 1)
INSTALLED = ('/home/u/.claude/plugins/cache/csr-companion/0.5.0', 0)


class Decide(unittest.TestCase):
    def test_a_fresh_lease_another_copy_holds_is_kept_from_equal_or_lower_rank_and_from_anyone_while_busy(self):
        held = dict(root=INSTALLED[0], rank=0, at=NOW)
        self.assertEqual(lease.decide('take', held, *INSTALLED, NOW + 1), (True, dict(root=INSTALLED[0], rank=0, at=NOW + 1)))
        self.assertEqual(lease.decide('take', held, *DEV, NOW + 1)[0], True)  # idle: the dev copy outranks it
        self.assertEqual(lease.decide('take', dict(held, rank=1, root='/other/dev'), *DEV, NOW + 1), (False, None))
        self.assertEqual(lease.decide('take', dict(held, busy=True), *DEV, NOW + 1), (False, None))  # its look runs
        stale = dict(held, busy=True, at=NOW - lease.LEASE_MS - 1)  # a look that never ended: stale after 15 minutes
        self.assertEqual(lease.decide('take', stale, *DEV, NOW, busy=True), (True, dict(root=DEV[0], rank=1, at=NOW, busy=True)))
        self.assertEqual(lease.decide('take', 'garbled', *DEV, NOW)[0], True)

    def test_release_keeps_the_lease_and_drops_busy_renew_keeps_busy_neither_touches_another_copys(self):
        mine = dict(root=DEV[0], rank=1, at=NOW, busy=True)
        self.assertEqual(lease.decide('release', mine, *DEV, NOW + 5), (True, dict(root=DEV[0], rank=1, at=NOW)))
        self.assertEqual(lease.decide('renew', mine, *DEV, NOW + 5), (True, dict(mine, at=NOW + 5)))
        other = dict(root=INSTALLED[0], rank=0, at=NOW, busy=True)
        self.assertEqual(lease.decide('release', other, *DEV, NOW), (False, None))
        self.assertEqual(lease.decide('renew', other, *DEV, NOW), (False, None))


class Race(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.folder)

    def start(self, root, rank=1, step='take', busy=True, pause='150'):
        argv = [sys.executable, '-I', LEASE_PY, step, '--folder', self.folder, '--session', 's', '--root', root,
                '--rank', str(rank), '--now', str(NOW)] + (['--busy'] if busy else [])
        return subprocess.Popen(argv, stdout=subprocess.PIPE, env=dict(os.environ, CSR_LEASE_PAUSE_MS=pause), text=True)

    def test_of_n_copies_that_ask_at_once_exactly_one_holds_the_lease(self):
        for n in (2, 3, 8):
            with self.subTest(n=n):
                for name in os.listdir(self.folder):
                    os.remove(os.path.join(self.folder, name))
                runs = [self.start(f'/copy/{i}') for i in range(n)]  # equal rank: none outranks a busy holder
                answers = [json.loads(run.communicate(timeout=30)[0]) for run in runs]
                held = [a['held'] for a in answers]
                self.assertEqual(held.count(True), 1, held)
                winner = runs[held.index(True)].args[runs[0].args.index('--root') + 1]
                with open(os.path.join(self.folder, 's.json'), encoding='utf-8') as handle:
                    self.assertEqual(json.load(handle), dict(root=winner, rank=1, at=NOW, busy=True))

    def test_a_copy_killed_holding_the_lock_leaves_nothing_to_reclaim(self):
        dead = self.start('/copy/dead', pause='5000')
        time.sleep(0.5)  # it holds the lock now, before its write
        dead.send_signal(signal.SIGKILL)
        dead.wait(timeout=10)
        began = time.monotonic()
        alive = self.start('/copy/alive', pause='0')
        self.assertEqual(json.loads(alive.communicate(timeout=10)[0])['held'], True)
        self.assertLess(time.monotonic() - began, 5)  # the kernel let go of the lock: no stale-lock wait
        self.assertTrue(os.path.exists(os.path.join(self.folder, 's.lease.lock')))  # a file left behind is only a file


if __name__ == '__main__':
    unittest.main()
