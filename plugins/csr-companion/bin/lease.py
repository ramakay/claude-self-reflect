"""One dreamer per session: the lease, read, checked and written in one process under an OS lock.

usage: python3 -I lease.py take|renew|release --folder DIR --session ID --root ROOT --rank N --now MS [--busy]

The installed plugin and a dev copy of it can both load in one session; they share HOME, so a lease per session
(<folder>/<session>.json) says which copy dreams. The whole transaction (read the lease, check its owner and age,
write it) runs here under flock(LOCK_EX) on <folder>/<session>.lease.lock. The kernel releases that lock when this
process ends, however it ends, so there is no stale lock to reclaim: a lock file left behind is only a file.
astra r2 #4 and r3 #4: two lock schemes built from mkdir, ln and mv in the mod each let two copies take the lease at
once ([true,true], then [true,false,true]): two paid looks.

  take     the lease is this copy's unless another copy holds a fresh one (younger than LEASE_MS) of equal or higher
           rank, or one whose look runs (busy); --busy marks a look running
  renew    a lease this copy holds is fresh again, busy as it was
  release  the look has ended: a busy lease this copy holds stays its, no longer busy

Prints {"held": true|false, "lease": <the lease now>}.
"""
import fcntl
import json
import os
import sys
import time

LEASE_MS = 15 * 60 * 1000  # dreamer.ts LEASE_MS


def decide(step, held, root, rank, now, busy=False):
    """(whether this copy holds the lease, the lease to write or None to write nothing)."""
    held = held if isinstance(held, dict) else None
    mine = held is not None and held.get('root') == root
    if step == 'take':
        at = held.get('at') if held else None
        fresh = isinstance(at, (int, float)) and not isinstance(at, bool) and now - at < LEASE_MS
        if held and not mine and fresh and (held.get('rank', 0) >= rank or held.get('busy')):
            return False, None
        return True, dict(root=root, rank=rank, at=now, **({'busy': True} if busy else {}))
    if step == 'renew':
        return (True, dict(held, at=now)) if mine else (False, None)
    if step == 'release':
        if mine and held.get('busy'):
            return True, dict(root=root, rank=held.get('rank', rank), at=held.get('at', 0))
        return mine, None
    raise ValueError(step)


def read(path):
    try:
        with open(path, encoding='utf-8') as handle:
            return json.load(handle)
    except (OSError, ValueError):
        return None


def write(path, lease):
    with open(path + '.writing', 'w', encoding='utf-8') as handle:
        json.dump(lease, handle)
    os.replace(path + '.writing', path)  # one rename: a reader never sees half a lease


def step_under_lock(step, folder, session, root, rank, now, busy=False):
    os.makedirs(folder, exist_ok=True)
    path = os.path.join(folder, f'{session}.json')
    with open(os.path.join(folder, f'{session}.lease.lock'), 'a') as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)  # released when this process ends, however it ends
        held = read(path)
        pause = os.environ.get('CSR_LEASE_PAUSE_MS')  # tests only: hold the lock between the read and the write
        if pause:
            time.sleep(int(pause) / 1000)
        mine, lease = decide(step, held, root, rank, now, busy)
        if lease is not None:
            write(path, lease)
        return dict(held=mine, lease=lease if lease is not None else held)


def main(argv):
    step, args = argv[0], argv[1:]
    value = lambda name: args[args.index(name) + 1]
    out = step_under_lock(step, value('--folder'), value('--session'), value('--root'), int(value('--rank')),
                          int(float(value('--now'))), '--busy' in args)
    print(json.dumps(out))


if __name__ == '__main__':
    main(sys.argv[1:])
