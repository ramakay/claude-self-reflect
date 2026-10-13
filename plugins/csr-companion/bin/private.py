"""Everything the companion keeps on disk is private to its owner, and no key or password is kept in it.

The companion's folder (~/.claude-self-reflect/csr-companion/ and what is under it) holds text from your
transcripts: the prompts you typed, the threads carried between sessions, the dreams shown and what you said to them.
Python's open() and mkdir create files 0644 and folders 0755 under the usual umask 022, which lets any other local
user read them. Every write in the companion goes through here instead:

  ensure_dir(path)            the folder and each folder above it down from the companion's root: 0700, tightened
                              if it was made looser before
  open_private(path, 'a'|'w') a file opened for writing at 0600; a file that was 0644 is tightened on this first touch
  write_atomic(path, text)    the whole file at 0600 through a temp file and one rename
  append(path, text)          one more piece on the end of a file at 0600
  redact(text)                the text with API keys, bearer tokens, password=/token= values and PEM blocks removed
  redact_value(value)         the same, for every string inside JSON-shaped data (keys are left as they are)

Run as a script it serves the TypeScript hooks, whose $.fs.write has no mode to ask for:

  python3 -I private.py write PATH [--redact] < text      write the text from stdin to PATH, privately
  python3 -I private.py tighten                           tighten what an earlier version left looser

Nothing here raises for the sake of tidiness: a write that cannot be made private is not made, and says OSError.
"""
import json
import os
import re
import sys

DIR_MODE = 0o700
FILE_MODE = 0o600
HOME = os.environ.get('HOME', '')
ROOT = os.path.join(HOME, '.claude-self-reflect', 'csr-companion')
TIGHT = '.private'  # in the root: the tree was walked once and tightened; every write since is private as it is made


def _inside(path, root):
    return bool(root) and (path == root or path.startswith(root.rstrip(os.sep) + os.sep))


def ensure_dir(path, root=None):
    """The folder at path, 0700, and each folder from the companion's root down to it. A path outside the root
    gets only itself made private; the folders above it are made as usual (they are not the companion's)."""
    path, root = os.path.abspath(path), os.path.abspath(root or ROOT)
    chain = [path]
    if _inside(path, root):
        chain = [root]
        for part in os.path.relpath(path, root).split(os.sep):
            if part not in ('.', ''):
                chain.append(os.path.join(chain[-1], part))
    os.makedirs(os.path.dirname(chain[0]), exist_ok=True)
    for folder in chain:
        try:
            os.mkdir(folder, DIR_MODE)
        except FileExistsError:
            pass
        os.chmod(folder, DIR_MODE)  # the umask only ever removes bits; a folder made earlier may be wider
    if _inside(path, root):
        tighten_once(root)
    return path


def tighten(root=None):
    """Every folder under the root 0700 and every file 0600, as an earlier version left them 0755 and 0644."""
    root = root or ROOT
    if not os.path.isdir(root):
        return
    os.chmod(root, DIR_MODE)
    for folder, dirs, files in os.walk(root):
        for name in dirs:
            try:
                os.chmod(os.path.join(folder, name), DIR_MODE)
            except OSError:
                pass
        for name in files:
            try:
                os.chmod(os.path.join(folder, name), FILE_MODE)
            except OSError:
                pass


def tighten_once(root=None):
    """tighten(), the first time only: a marker in the root says it was done, and new files are private as made."""
    root = root or ROOT
    marker = os.path.join(root, TIGHT)
    if os.path.exists(marker):
        return
    tighten(root)
    descriptor = os.open(marker, os.O_WRONLY | os.O_CREAT, FILE_MODE)
    os.fchmod(descriptor, FILE_MODE)
    os.close(descriptor)


def open_private(path, mode='a', encoding='utf-8', root=None):
    """path opened for writing ('a' appends, 'w' replaces), created 0600; a file already there is made 0600 too."""
    if mode not in ('a', 'w'):
        raise ValueError(mode)
    ensure_dir(os.path.dirname(os.path.abspath(path)), root)
    flags = os.O_WRONLY | os.O_CREAT | (os.O_APPEND if mode == 'a' else os.O_TRUNC)
    descriptor = os.open(path, flags, FILE_MODE)
    try:
        os.fchmod(descriptor, FILE_MODE)
        return os.fdopen(descriptor, mode, encoding=encoding)
    except BaseException:
        os.close(descriptor)
        raise


def write_atomic(path, text, root=None):
    """All of text in path at 0600, by a temp file made 0600 and one rename: a reader never sees half of it."""
    path = os.path.abspath(path)
    ensure_dir(os.path.dirname(path), root)
    temp = f'{path}.{os.getpid()}.tmp'
    try:
        descriptor = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, FILE_MODE)
        os.fchmod(descriptor, FILE_MODE)
        with os.fdopen(descriptor, 'w', encoding='utf-8') as handle:
            handle.write(text)
        os.replace(temp, path)
    except BaseException:
        try:
            os.unlink(temp)
        except OSError:
            pass
        raise


def append(path, text, root=None):
    with open_private(path, 'a', root=root) as handle:
        handle.write(text)


# ---------------------------------------------------------------- redaction

REDACTED = '[redacted]'
_WORD = r'[A-Za-z0-9_.\-]*?'
_NAMES = r'(?:password|passwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret)'
SECRETS = (
    # a PEM block, or the start of one a cut-short excerpt lost the end of
    (re.compile(r'-----BEGIN [A-Z0-9 ]+-----.*?(?:-----END [A-Z0-9 ]+-----|\Z)', re.S), REDACTED),
    # API keys by their prefixes: Anthropic and OpenAI sk-, xAI xai-, GitHub, AWS, Slack, Google, Hugging Face, npm
    (re.compile(r'\bsk-[A-Za-z0-9_\-]{16,}'), REDACTED),
    (re.compile(r'\bxai-[A-Za-z0-9]{16,}'), REDACTED),
    (re.compile(r'\bgh[pousr]_[A-Za-z0-9]{20,}'), REDACTED),
    (re.compile(r'\bgithub_pat_[A-Za-z0-9_]{20,}'), REDACTED),
    (re.compile(r'\b(?:AKIA|ASIA)[A-Z0-9]{16}\b'), REDACTED),
    (re.compile(r'\bxox[abposr]-[A-Za-z0-9\-]{10,}'), REDACTED),
    (re.compile(r'\bAIza[0-9A-Za-z_\-]{35}'), REDACTED),
    (re.compile(r'\b(?:hf|npm)_[A-Za-z0-9]{30,}'), REDACTED),
    (re.compile(r'\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}'), REDACTED),
    # Authorization headers: the scheme stays, the credential goes
    (re.compile(r'\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=\-]{8,}', re.I), r'\1 ' + REDACTED),
    # password=..., "token": "...", API_KEY: ...: the name and the separator stay, the value goes
    (re.compile(r'(?i)(\b' + _WORD + _NAMES + r'["\']?\s*[=:]\s*)(?!\[redacted\])("[^"\n]*"|\'[^\'\n]*\'|[^\s,;&"\'`)\]}]+)'),
     lambda m: m.group(1) + (REDACTED if m.group(2)[:1] not in '"\'' else m.group(2)[0] + REDACTED + m.group(2)[0])),
)


def redact(text):
    """text without credentials. Applying it twice changes nothing more."""
    if not isinstance(text, str) or not text:
        return text
    for pattern, replacement in SECRETS:
        text = pattern.sub(replacement, text)
    return text


def redact_value(value):
    """value with every string in it redacted; a dict's keys, numbers and the rest are left as they are."""
    if isinstance(value, str):
        return redact(value)
    if isinstance(value, dict):
        return {key: redact_value(inner) for key, inner in value.items()}
    if isinstance(value, (list, tuple)):
        return [redact_value(inner) for inner in value]
    return value


def dumps(record, **options):
    """record as JSON with its strings redacted, the form every kept record takes."""
    return json.dumps(redact_value(record), **options)


def main(argv):
    if len(argv) >= 3 and argv[1] == 'write':
        text = sys.stdin.buffer.read().decode('utf-8', 'replace')
        write_atomic(argv[2], redact(text) if '--redact' in argv[3:] else text)
        return 0
    if len(argv) == 2 and argv[1] == 'tighten':
        tighten()
        return 0
    sys.stderr.write('usage: private.py write PATH [--redact] < text | private.py tighten\n')
    return 2


if __name__ == '__main__':
    try:
        sys.exit(main(sys.argv))
    except OSError as error:
        sys.stderr.write(f'private.py: {error}\n')
        sys.exit(1)
