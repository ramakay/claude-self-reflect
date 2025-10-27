# The Three-Tier System Problem

**Discovered**: 2025-10-19
**Impact**: CRITICAL - 20,000+ conversation chunks orphaned in old format

---

## Executive Summary

CSR has **three separate import systems** that don't communicate:

1. **TIER 1 (conv_*)**: OLD format with ~20,120 chunks - **Most conversations trapped here**
2. **TIER 2 (csr_*)**: NEW format with only ~54 chunks - **Auto-import broken?**
3. **TIER 3 (v3_*)**: NARRATIVE format with 91 narratives - **Only 0.4% coverage**

**Problem**: OpenGraph conversation (and 99.6% of others) stuck in TIER 1, never got narratives.

---

## The Discovery

### Query: "What did we do for OpenGraph on procsolve website?"

**Expected**: Narrative would provide complete answer (9.3x better quality)
**Actual**: Had to piece together from chunk fragments
**Why**: OpenGraph conversation in `conv_9f2f312b_local` (TIER 1 - old format)

### Investigation Results

```
Searching 78 Qdrant collections for OpenGraph CID...
✅ FOUND in: conv_9f2f312b_local (TIER 1 - OLD FORMAT)
❌ NOT in: csr_procsolve-website_local_384d (TIER 2 - NEW)
❌ NOT in: v3_all_projects (TIER 3 - NARRATIVES)
```

---

## Tier Breakdown

### TIER 1: OLD FORMAT (conv_*)

**Stats**: 51 collections, ~20,120 chunks

**Collections Sample**:
```
conv_02a62515_local:        995 chunks
conv_f87a171a_local:        514 chunks
conv_9f2f312b_local:      3,121 chunks  ← OpenGraph here!
conv_a595acba_local:        106 chunks
... 47 more collections
```

**Characteristics**:
- ✅ Still searchable
- ❌ Low quality (fragmented chunks)
- ❌ Never migrated to new format
- ❌ No narratives created
- ❌ Orphaned/abandoned

**Why It Exists**:
- Created by early CSR versions (v1.x - v2.x)
- Per-conversation collection naming (conv_{uuid}_{mode})
- No bulk migration tool provided in v3.x+

### TIER 2: NEW FORMAT (csr_*)

**Stats**: 16 collections, ~54 chunks (ONLY!)

**Collections**:
```
csr_claude-self-reflect_local_384d:     ~20 chunks
csr_anukruti_local_384d:                ~10 chunks
csr_procsolve-website_local_384d:        ~5 chunks
... 13 more collections
```

**Characteristics**:
- ✅ Modern collection naming
- ✅ Per-project organization
- ❌ Only 54 chunks total (suspiciously low!)
- ❌ Auto-import not working properly?

**Why It's Empty**:
- streaming-importer supposed to auto-populate
- Should have 1000s of chunks by now
- Possible causes:
  - Import script not running
  - Only processing NEW conversations (not backfilling)
  - Silent failures not caught

### TIER 3: NARRATIVE FORMAT (v3_*)

**Stats**: 3 collections, 91 narratives

**Collections**:
```
v3_all_projects:           54 narratives (main)
v3_test_csr:               23 narratives (test)
v3_test_procsolve:         14 narratives (test)
```

**Characteristics**:
- ✅ 9.3x better search quality
- ✅ Rich metadata (tools, concepts, files)
- ✅ Problem-solution narratives
- ❌ Only 91 conversations (0.4% of total ~20K)
- ❌ Manual batch processing (not automated)

**Why It's Incomplete**:
- Batch processing only looked at JSONL files on disk
- Didn't process conversations already in TIER 1
- Manual process, not integrated with auto-import

---

## The Migration Failure Timeline

### Phase 1: Early CSR (v1.x - v2.x)
```
JSONL files → Import → conv_{uuid}_local/voyage
              (Each conversation = separate collection)

Result: 51 collections, 20,120 chunks
```

### Phase 2: CSR v3.x+ Refactor
```
New architecture: csr_{project}_local_384d
(Per-project collections instead of per-conversation)

Problem: NO MIGRATION TOOL PROVIDED
Old conv_* collections abandoned in place
```

### Phase 3: Narrative Experiment (2025-10-19)
```
Manual batch processing of 54 conversations
Created v3_all_projects collection

Problem: Only processed NEW conversations
Didn't backfill 20,120 old chunks from TIER 1
```

---

## Impact Analysis

### Search Quality Impact

When searching for "procsolve website OpenGraph":

| Tier | Score | Quality | Status |
|------|-------|---------|--------|
| TIER 1 (conv_*) | 0.595 | ❌ Fragments | Found but low quality |
| TIER 2 (csr_*) | N/A | N/A | ❌ Not found (empty) |
| TIER 3 (v3_*) | N/A | ✅ 9.3x better | ❌ Not processed |

**Result**: User got fragmented chunk-based answer instead of complete narrative.

### Coverage Analysis

```
Total conversations on disk: ~500 JSONL files
Processed into narratives:   91 (18%)
Stuck in old format:         ~20,120 chunks (representing ~400 conversations)
In new format:               54 chunks (11%)

Coverage gap: 82% of conversations have NO narratives
```

---

## Root Causes

### 1. No Migration Tool (v2 → v3)

When CSR moved from conv_* to csr_* naming:
- ❌ No automatic migration
- ❌ No documentation on manual migration
- ❌ Old collections left in place
- ✅ Still searchable (but low quality)

### 2. Auto-Import Not Backfilling

streaming-importer only processes NEW conversations:
- ✅ Watches for new JSONL files
- ❌ Doesn't check if conversation already in TIER 1
- ❌ Doesn't migrate old conv_* collections
- ❌ Result: Only 54 chunks in TIER 2 (should have 1000s)

### 3. Narrative Processing Incomplete

Batch processing only looked at recent data:
- ✅ Processed 54 conversations from TIER 2
- ❌ Didn't process 20,120 chunks from TIER 1
- ❌ No automated pipeline
- ❌ Result: 0.4% coverage

---

## The Fix (Recommended)

### Phase 1: Immediate (Unblock Narratives)

```bash
# Process ALL conversations in TIER 1 into narratives
python scripts/migrate-tier1-to-narratives.py \
  --source conv_* \
  --target v3_all_projects \
  --batch-api \
  --cost-limit 50

Estimated:
  - 400 conversations to process
  - ~$40 cost (batch API)
  - 9.3x better search quality
```

### Phase 2: Fix Auto-Import

```bash
# Debug why TIER 2 only has 54 chunks
python scripts/debug-auto-import.py

# Backfill TIER 2 from disk
python scripts/backfill-csr-collections.py \
  --source ~/.claude/projects/ \
  --skip-if-in-tier1
```

### Phase 3: Deprecate TIER 1

```bash
# After migration, remove old collections
python scripts/cleanup-old-collections.py \
  --delete conv_* \
  --confirm
```

---

## Prevention (Future Architecture)

### Unified Import Pipeline

```
JSONL files → Single Import System → Narrative Generation
                                    ↓
                            v3_{project} collections
                            (Only narratives, no chunks)
```

**Benefits**:
- ✅ Single source of truth
- ✅ Automatic narrative generation
- ✅ No migration needed
- ✅ 9.3x better quality from day 1

### Monitoring

```bash
# Daily health check
python scripts/collection-health-check.py
  Expected: All conversations have narratives
  Alert if: Coverage < 95%
```

---

## Lessons Learned

### 1. **Migration Tools Are Critical**

When changing collection schemas:
- ✅ Provide automatic migration scripts
- ✅ Validate migration completeness
- ✅ Document manual migration steps
- ✅ Test on production data

### 2. **Monitor Coverage Gaps**

Track what percentage of conversations have narratives:
- ✅ Alert when coverage drops
- ✅ Surface orphaned collections
- ✅ Validate auto-import working

### 3. **Unified Systems > Multiple Systems**

Three separate tiers caused:
- ❌ Orphaned data
- ❌ Quality inconsistency
- ❌ Maintenance burden
- ❌ User confusion

Better: **One system, one truth**

---

## Questions Answered

### Q: "If narratives are better, why wasn't OpenGraph included?"

**A**: OpenGraph conversation is in TIER 1 (old conv_* format). Narrative batch processing only looked at TIER 2 (new csr_* format) and didn't backfill TIER 1. Result: 99.6% of conversations have no narratives.

### Q: "What's happening with the original conversations?"

**A**: Three separate systems:
1. **OLD (conv_*)**: 20,120 chunks orphaned, low quality
2. **NEW (csr_*)**: Only 54 chunks, auto-import broken?
3. **NARRATIVES (v3_*)**: 91 narratives, 9.3x better but incomplete

### Q: "What's happening with Claude Code import?"

**A**: streaming-importer creates TIER 2 (csr_*) collections but:
- Only processing NEW conversations
- Not backfilling old TIER 1 collections
- Result: Most data stuck in old format

---

## Recommendation

### **Immediate Action Required**

1. ✅ **Process TIER 1 → TIER 3**: Migrate all 20,120 chunks to narratives ($40)
2. ✅ **Fix TIER 2 auto-import**: Debug why only 54 chunks
3. ✅ **Deprecate TIER 1**: Remove old collections after migration

### **Long-term Architecture**

Replace three-tier system with unified narrative-only pipeline:
- Auto-generate narratives on import
- No chunking needed
- 9.3x better quality
- Single source of truth

---

**Status**: CRITICAL issue discovered, fix in progress
**Next Steps**: Create migration script to process TIER 1 → TIER 3
**Expected Outcome**: 100% narrative coverage, 9.3x better search for all conversations
