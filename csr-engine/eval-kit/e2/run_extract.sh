#!/bin/zsh
set -u
SCRATCH="$SCRATCH"
E2="$SCRATCH/e2"
PROTO="$E2/extraction_prompt.md"
OUTDIR="$E2/extract_grok"
RAWDIR="$E2/grok_raw"
mkdir -p "$OUTDIR" "$RAWDIR"

TBIN=$(command -v gtimeout || command -v timeout || true)

extract_json_block() {
  python3 -c '
import sys, re
s = sys.stdin.read()
s = re.sub(r"^```(?:json)?\s*", "", s.strip())
s = re.sub(r"```\s*$", "", s.strip())
s = s.strip()
start = s.find("{")
end = s.rfind("}")
if start != -1 and end != -1 and end > start:
    print(s[start:end+1])
else:
    print(s)
'
}

run_one() {
  local qid="$1"
  local attempt="$2"
  local extra="$3"
  local digest="$E2/digests/${qid}.md"
  local promptfile
  promptfile=$(mktemp -t grok-spec.XXXXXX)
  {
    cat "$PROTO"
    echo ""
    echo "DIGEST FOLLOWS:"
    echo ""
    cat "$digest"
    echo ""
    echo "Return ONLY the JSON object for qid=${qid}. No markdown fences, no commentary before or after."
    if [ -n "$extra" ]; then
      echo ""
      echo "$extra"
    fi
  } > "$promptfile"

  local rawfile="$RAWDIR/${qid}.attempt${attempt}.txt"
  if [ -n "$TBIN" ]; then
    "$TBIN" 600 grok --prompt-file "$promptfile" \
      -m grok-4.5 \
      --output-format plain \
      --no-subagents \
      --disable-web-search \
      --no-memory \
      --cwd "$E2" \
      > "$rawfile" 2>&1
  else
    grok --prompt-file "$promptfile" \
      -m grok-4.5 \
      --output-format plain \
      --no-subagents \
      --disable-web-search \
      --no-memory \
      --cwd "$E2" \
      > "$rawfile" 2>&1
  fi
  rm -f "$promptfile"
  echo "$rawfile"
}

QIDS=(Q1 Q2 Q3 Q4 Q5 Q6 Q7 Q8 Q9 Q10 Q11 Q12 A1 A2 A3 A4 A5 A6 A7 A8)

for qid in "${QIDS[@]}"; do
  echo "=== $qid ===" >&2
  rawfile=$(run_one "$qid" 1 "")
  jsonfile="$OUTDIR/${qid}.json"
  extract_json_block < "$rawfile" > "$jsonfile.tmp"
  if python3 -c "import json,sys; json.load(open('$jsonfile.tmp'))" 2>/dev/null; then
    mv "$jsonfile.tmp" "$jsonfile"
    echo "$qid: OK (attempt 1)" >&2
  else
    echo "$qid: attempt 1 invalid JSON, retrying" >&2
    rawfile2=$(run_one "$qid" 2 "Your previous output was invalid JSON. Output only the JSON object.")
    extract_json_block < "$rawfile2" > "$jsonfile.tmp"
    if python3 -c "import json,sys; json.load(open('$jsonfile.tmp'))" 2>/dev/null; then
      mv "$jsonfile.tmp" "$jsonfile"
      echo "$qid: OK (attempt 2)" >&2
    else
      echo "{\"qid\": \"$qid\", \"error\": \"extraction_failed\"}" > "$jsonfile"
      rm -f "$jsonfile.tmp"
      echo "$qid: FAILED both attempts" >&2
    fi
  fi
done
echo "DONE" >&2
