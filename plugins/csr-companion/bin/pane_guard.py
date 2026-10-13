"""The CSR pane's guard: runs bin/pane.py with the same arguments and keeps the pane alive when an edit to it
fails to load. A pane.py that does not compile, dies as it loads, or asks to start again (exit 75 after frames
kept failing) gets a one-screen error naming file:line, logged to pane-errors.log; the guard then waits until
pane.py changes (checked each second) or 30 s pass, and runs it again. A clean exit (q) ends it. Stdlib only.
usage: python3 -I pane_guard.py <pane.py's arguments>"""
import os
import re
import select
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.realpath(__file__))
PANE = os.path.join(HERE, 'pane.py')
LOG = os.path.join(os.environ.get('HOME', ''), '.claude-self-reflect', 'csr-companion', 'pane-errors.log')
LOG_MAX = 1 << 20  # as pane.py's: past this the log moves to .1
WAIT = 30  # seconds before a retry when pane.py has not changed
WHERE = re.compile(r'File "([^"]+)", line (\d+)')


def mtime(path):
    try:
        return os.stat(path).st_mtime_ns
    except OSError:
        return None


def log(text, path=None):
    """Append to the error log, moving it aside to .1 first once it passes LOG_MAX. Never raises."""
    path = path or LOG
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        if os.path.getsize(path) >= LOG_MAX:
            os.replace(path, path + '.1')
    except OSError:
        pass
    try:
        with open(path, 'a', encoding='utf-8') as handle:
            handle.write(f"{time.strftime('%Y-%m-%dT%H:%M:%S')} guard: {text.rstrip()}\n")
    except OSError:
        pass


def compiled(path):
    """None when pane.py compiles, else 'SyntaxError: ... · pane.py:12'."""
    try:
        with open(path, encoding='utf-8') as handle:
            compile(handle.read(), path, 'exec')
    except SyntaxError as error:
        return f'{type(error).__name__}: {error.msg} · {os.path.basename(path)}:{error.lineno}'
    except (OSError, ValueError) as error:
        return f'{type(error).__name__}: {error}'
    return None


def failure(stderr, code):
    """What a run that failed left on stderr, short: its last line and the deepest pane.py line in the trace."""
    lines = [line for line in stderr.splitlines() if line.strip()]
    places = [m for m in WHERE.finditer(stderr) if os.path.basename(m.group(1)) == os.path.basename(PANE)] \
        or list(WHERE.finditer(stderr))
    where = f'{os.path.basename(places[-1].group(1))}:{places[-1].group(2)}' if places else ''
    said = lines[-1].strip() if lines else f'exit {code}'
    return ' · '.join(x for x in (said[:120], where) if x)


def screen(problem, wait):
    """The one-screen error: what failed, where, and what the guard waits for."""
    sys.stdout.write('\x1b[H\x1b[2J\x1b[1mCSR pane error\x1b[0m\n\n' + problem + '\n\n\x1b[2m'
                     + f'waiting for {os.path.basename(PANE)} to change · retry in {wait}s · q closes\x1b[0m\n')
    sys.stdout.flush()


def changed(path, seen, wait, keys=None):
    """Wait until path's mtime differs from seen, or wait seconds pass, polling each second; False when q was
    pressed. keys: a readable tty to watch for q, or None."""
    end, saved = time.time() + wait, None
    if keys is not None:  # q reaches the guard at once, not after Enter
        import termios
        import tty
        saved = termios.tcgetattr(keys.fileno())
        tty.setcbreak(keys.fileno())
    try:
        while time.time() < end and mtime(path) == seen:
            if keys is None:
                time.sleep(1)
                continue
            ready, _, _ = select.select([keys], [], [], 1)
            if ready and 'q' in os.read(keys.fileno(), 64).decode('utf-8', 'replace').lower():
                return False
        return True
    finally:
        if saved:
            termios.tcsetattr(keys.fileno(), termios.TCSADRAIN, saved)


def run(argv, path):
    """pane.py once, its stderr kept to read back: (exit code, what it wrote to stderr)."""
    with tempfile.TemporaryFile(mode='w+', encoding='utf-8', errors='replace') as err:
        flags = ['-I'] if sys.flags.isolated else []
        code = subprocess.run([sys.executable, *flags, path, *argv], stderr=err,
                              env=dict(os.environ, CSR_PANE_GUARD='1')).returncode
        err.seek(0)
        return code, err.read()


def main(argv, path=PANE, wait=WAIT, waiter=changed, runner=run):
    """Run pane.py until it ends cleanly; a failure shows its screen, logs it, and waits for a change."""
    keys = sys.stdin if sys.stdin.isatty() else None
    while True:
        seen = mtime(path)
        problem = compiled(path)
        if problem is None:
            code, stderr = runner(argv, path)
            if code <= 0:  # q, or closed: a pane killed outright is not started again
                return 0
            problem = failure(stderr, code)
        log(problem)
        screen(problem, wait)
        if not waiter(path, seen, wait, keys):
            return 0


if __name__ == '__main__':
    try:
        sys.exit(main(sys.argv[1:]))
    except KeyboardInterrupt:
        sys.exit(0)
