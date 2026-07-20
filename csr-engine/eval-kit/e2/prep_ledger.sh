#!/bin/zsh
# E2 ledger harvest: git events per repo + npm publishes (CSR only) + release-train.yaml.
# Output: e2/ledger.json
E2="$(cd "$(dirname "$0")" && pwd)"
OUT="$E2/ledger.json"
{
echo '{"git": {'
first_repo=1
for repo in \
  $HOME/projects/claude-self-reflect \
  $HOME/projects/anukriti \
  $HOME/projects/anukriti-command-center \
  $HOME/projects/Anukriti-Campaigns
do
  [ -d "$repo/.git" ] || continue
  [ $first_repo -eq 0 ] && echo ','
  first_repo=0
  name=$(basename "$repo")
  echo "\"$name\": ["
  git -C "$repo" log --all --pretty=format:'COMMIT|%H|%aI|%s' --name-only --until=2026-07-16 2>/dev/null | \
  python3 -c '
import sys, json
events, cur = [], None
for line in sys.stdin:
    line = line.rstrip("\n")
    if line.startswith("COMMIT|"):
        if cur: events.append(cur)
        _, h, d, s = line.split("|", 3)
        cur = {"hash": h[:12], "date": d, "subject": s[:120], "files": []}
    elif line.strip() and cur is not None:
        if len(cur["files"]) < 40: cur["files"].append(line.strip())
if cur: events.append(cur)
print(",\n".join(json.dumps(e) for e in events))
'
  echo "]"
done
echo '},'
echo '"tags": {'
first_repo=1
for repo in \
  $HOME/projects/claude-self-reflect \
  $HOME/projects/anukriti
do
  [ -d "$repo/.git" ] || continue
  [ $first_repo -eq 0 ] && echo ','
  first_repo=0
  name=$(basename "$repo")
  echo -n "\"$name\": "
  git -C "$repo" for-each-ref refs/tags --format='%(refname:short)|%(creatordate:iso-strict)' | \
  python3 -c 'import sys,json; print(json.dumps([dict(zip(["tag","date"],l.strip().split("|"))) for l in sys.stdin if "|" in l]))'
done
echo '},'
echo -n '"npm_claude_self_reflect": '
npm view claude-self-reflect time --json 2>/dev/null || echo '{}'
echo ','
echo -n '"release_train": '
python3 -c '
import json, os
path = os.path.expandvars("$HOME/projects/anukriti/anukriti-mvp-expo/release-train.yaml")
try:
    import yaml
    d = yaml.safe_load(open(path))
    print(json.dumps(d, default=str))
except Exception as e:
    print(json.dumps({"raw": open(path).read()[:8000]}))
'
echo '}'
} > "$OUT"
python3 -c "import json; d=json.load(open('$OUT')); print('ledger.json OK:', {k: (len(v) if isinstance(v,(list,dict)) else '...') for k,v in d.items()})"
