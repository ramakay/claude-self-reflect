#!/usr/bin/env python3
"""Qdrant → SQLite importer for CSR Rust engine.

Migrates all conversation chunks and reflections from Qdrant into the Rust
engine's SQLite database, preserving existing 384d embeddings (no re-embedding).

Usage:
    python scripts/qdrant_to_sqlite.py                    # Full import
    python scripts/qdrant_to_sqlite.py --dry-run           # Preview only
    python scripts/qdrant_to_sqlite.py --collections csr   # Only csr_* collections
    python scripts/qdrant_to_sqlite.py --collections conv   # Only conv_* collections
    python scripts/qdrant_to_sqlite.py --collections all    # Both (default)
"""

import argparse
import json
import re
import sqlite3
import struct
import sys
import time
import uuid
from pathlib import Path

try:
    from qdrant_client import QdrantClient
except ImportError:
    print("ERROR: qdrant-client not installed. Run: pip install qdrant-client")
    sys.exit(1)

# --- Configuration ---
QDRANT_URL = "http://localhost:6333"
DB_PATH = Path.home() / ".claude-self-reflect" / "csr-engine.db"
SCROLL_BATCH = 100  # Points per scroll request
EMBEDDING_DIM = 384
MAX_CONTENT_CHARS = 10_000  # Truncate oversized content (conv_* raw messages can be 500K+)

# --- Project normalization (mirrors Rust normalize_project_name) ---
def normalize_project_name(name: str) -> str:
    """Normalize project name from Qdrant to match Rust engine format."""
    if not name:
        return ""
    name = name.rstrip("/")
    # Extract final path component
    final = name.rsplit("/", 1)[-1] if "/" in name else name
    # Claude dash-separated format: -Users-name-projects-<project>
    if final.startswith("-") and "projects" in final:
        idx = final.rfind("projects-")
        if idx >= 0:
            start = idx + len("projects-")
            if start < len(final):
                return final[start:]
    # Collection name format: csr_<project>_local_384d
    m = re.match(r"^csr_(.+?)_(?:local|cloud)_\d+d$", name)
    if m:
        return m.group(1)
    return final if final else name


def normalize_qdrant_project(payload: dict, collection_name: str) -> str:
    """Extract normalized project name from Qdrant payload or collection name."""
    # Try payload.project first (full path)
    project_raw = payload.get("project", "")
    if project_raw:
        return normalize_project_name(project_raw)
    # Fall back to collection name
    return normalize_project_name(collection_name)


# --- Embedding serialization (matches Rust little-endian f32) ---
def vec_to_bytes(vector: list[float]) -> bytes:
    """Serialize float vector to little-endian f32 bytes (matches Rust format)."""
    return struct.pack(f"<{len(vector)}f", *vector)


# --- Deterministic chunk ID (matches Rust UUIDv5 generation) ---
CSR_NAMESPACE = uuid.UUID("6ba7b810-9dad-11d1-80b4-00c04fd430c8")

def make_chunk_id(conversation_id: str, chunk_index: int) -> str:
    """Generate deterministic UUID matching Rust engine's UUIDv5 scheme."""
    return str(uuid.uuid5(CSR_NAMESPACE, f"{conversation_id}:{chunk_index}"))


# --- Timestamp normalization ---
def normalize_timestamp(ts: str) -> str:
    """Ensure timestamp has UTC timezone designator for chrono parsing."""
    if not ts:
        return ""
    ts = ts.strip()
    # Already has timezone info
    if ts.endswith("Z") or "+" in ts[10:] or ts[10:].count("-") > 0 and "T" in ts:
        # Check if there's a timezone offset after the time part
        if ts.endswith("Z") or re.search(r'[+-]\d{2}:\d{2}$', ts):
            return ts
    # Bare ISO timestamp like "2025-10-23T04:22:13.723731" → add Z
    if "T" in ts and len(ts) >= 19:
        return ts + "Z"
    return ts


# --- Content extraction ---
def extract_content_from_messages(messages: list[dict]) -> str:
    """Join messages array into searchable text content."""
    parts = []
    for msg in messages:
        role = msg.get("role", "")
        content = msg.get("content", "")
        if content:
            # Truncate very long tool results
            if role == "user" and content.startswith("[Result]") and len(content) > 500:
                content = content[:500] + "..."
            parts.append(f"{role}: {content}")
    return "\n".join(parts)


# --- Progress bar ---
class ProgressBar:
    """TQDM-style progress bar without dependencies."""

    def __init__(self, total: int, desc: str = "", width: int = 40):
        self.total = max(total, 1)
        self.current = 0
        self.desc = desc
        self.width = width
        self.start_time = time.time()
        self.last_print = 0

    def update(self, n: int = 1):
        self.current += n
        now = time.time()
        # Rate-limit prints to every 0.1s
        if now - self.last_print < 0.1 and self.current < self.total:
            return
        self.last_print = now
        self._render()

    def _render(self):
        elapsed = time.time() - self.start_time
        pct = min(self.current / self.total, 1.0)
        filled = int(self.width * pct)
        bar = "█" * filled + "░" * (self.width - filled)
        rate = self.current / elapsed if elapsed > 0 else 0
        eta = (self.total - self.current) / rate if rate > 0 else 0

        # Format ETA
        if eta < 60:
            eta_str = f"{eta:.0f}s"
        elif eta < 3600:
            eta_str = f"{eta / 60:.1f}m"
        else:
            eta_str = f"{eta / 3600:.1f}h"

        line = f"\r  {self.desc}: {bar} {pct:6.1%} | {self.current}/{self.total} | {rate:.0f} pts/s | ETA {eta_str}"
        sys.stderr.write(line)
        sys.stderr.flush()

    def close(self):
        self._render()
        sys.stderr.write("\n")
        sys.stderr.flush()


# --- Main importer ---
class QdrantToSqliteImporter:
    def __init__(self, db_path: Path, qdrant_url: str, dry_run: bool = False):
        self.qdrant = QdrantClient(url=qdrant_url)
        self.dry_run = dry_run
        self.db_path = db_path

        if not dry_run:
            self.conn = sqlite3.connect(str(db_path))
            self.conn.execute("PRAGMA journal_mode=WAL")
            self.conn.execute("PRAGMA synchronous=NORMAL")
        else:
            self.conn = None

        # Stats
        self.chunks_imported = 0
        self.chunks_skipped = 0
        self.reflections_imported = 0
        self.reflections_skipped = 0
        self.errors = 0
        self.start_time = time.time()

    def get_existing_chunk_ids(self) -> set[str]:
        """Load all existing chunk IDs from SQLite for dedup."""
        if self.conn is None:
            return set()
        cursor = self.conn.execute("SELECT id FROM chunks")
        return {row[0] for row in cursor.fetchall()}

    def get_existing_reflection_ids(self) -> set[str]:
        """Load all existing reflection IDs from SQLite for dedup."""
        if self.conn is None:
            return set()
        cursor = self.conn.execute("SELECT id FROM reflections")
        return {row[0] for row in cursor.fetchall()}

    def list_collections(self) -> list[dict]:
        """List all Qdrant collections with metadata."""
        collections = self.qdrant.get_collections().collections
        result = []
        for col in collections:
            info = self.qdrant.get_collection(col.name)
            result.append({
                "name": col.name,
                "points": info.points_count or 0,
            })
        return sorted(result, key=lambda c: -c["points"])

    def scroll_collection(self, name: str) -> list:
        """Scroll through all points in a collection."""
        all_points = []
        offset = None
        while True:
            result = self.qdrant.scroll(
                collection_name=name,
                limit=SCROLL_BATCH,
                offset=offset,
                with_payload=True,
                with_vectors=True,
            )
            points, next_offset = result
            all_points.extend(points)
            if next_offset is None or len(points) == 0:
                break
            offset = next_offset
        return all_points

    def import_chunk_point(self, point, collection_name: str, existing_ids: set) -> bool:
        """Import a single chunk point from Qdrant to SQLite. Returns True if imported."""
        payload = point.payload or {}
        vector = point.vector

        if not vector or len(vector) != EMBEDDING_DIM:
            self.errors += 1
            return False

        # Extract fields
        conv_id = payload.get("conversation_id", str(point.id))
        chunk_index = payload.get("chunk_index", 0)
        raw_ts = payload.get("created_at") or payload.get("timestamp", "")
        timestamp = normalize_timestamp(raw_ts)
        project = normalize_qdrant_project(payload, collection_name)

        # Generate deterministic ID
        chunk_id = make_chunk_id(conv_id, chunk_index)

        # Dedup
        if chunk_id in existing_ids:
            self.chunks_skipped += 1
            return False

        # Extract content
        messages = payload.get("messages")
        if messages and isinstance(messages, list):
            content = extract_content_from_messages(messages)
            msg_count = len(messages)
        else:
            content = payload.get("text", "")
            msg_count = payload.get("message_count", 0)

        if not content:
            self.chunks_skipped += 1
            return False

        # Truncate oversized content (conv_* raw messages can be 500K+)
        if len(content) > MAX_CONTENT_CHARS:
            content = content[:MAX_CONTENT_CHARS] + "\n\n[truncated]"

        if self.dry_run:
            existing_ids.add(chunk_id)
            self.chunks_imported += 1
            return True

        # Insert into SQLite
        try:
            embedding_bytes = vec_to_bytes(vector)
            self.conn.execute(
                "INSERT OR REPLACE INTO chunks (id, conversation_id, project_name, timestamp, content, message_count) "
                "VALUES (?, ?, ?, ?, ?, ?)",
                (chunk_id, conv_id, project, timestamp, content, msg_count),
            )
            self.conn.execute(
                "INSERT OR REPLACE INTO chunk_embeddings (chunk_id, embedding) VALUES (?, ?)",
                (chunk_id, embedding_bytes),
            )
            existing_ids.add(chunk_id)
            self.chunks_imported += 1
            return True
        except Exception as e:
            print(f"\n  ERROR importing chunk {chunk_id}: {e}", file=sys.stderr)
            self.errors += 1
            return False

    def import_reflection_point(self, point, existing_ids: set) -> bool:
        """Import a single reflection point from Qdrant to SQLite."""
        payload = point.payload or {}
        vector = point.vector

        if not vector or len(vector) != EMBEDDING_DIM:
            self.errors += 1
            return False

        content = payload.get("text", "")
        if not content:
            self.reflections_skipped += 1
            return False

        tags = payload.get("tags", [])
        if isinstance(tags, str):
            tags = [tags]
        raw_ts = payload.get("timestamp", "")
        timestamp = normalize_timestamp(raw_ts)

        # Generate ID from content hash
        ref_id = str(uuid.uuid5(CSR_NAMESPACE, f"reflection:{content[:200]}"))

        if ref_id in existing_ids:
            self.reflections_skipped += 1
            return False

        if self.dry_run:
            existing_ids.add(ref_id)
            self.reflections_imported += 1
            return True

        try:
            embedding_bytes = vec_to_bytes(vector)
            self.conn.execute(
                "INSERT OR REPLACE INTO reflections (id, content, tags, timestamp) VALUES (?, ?, ?, ?)",
                (ref_id, content, json.dumps(tags), timestamp),
            )
            self.conn.execute(
                "INSERT OR REPLACE INTO reflection_embeddings (reflection_id, embedding) VALUES (?, ?)",
                (ref_id, embedding_bytes),
            )
            existing_ids.add(ref_id)
            self.reflections_imported += 1
            return True
        except Exception as e:
            print(f"\n  ERROR importing reflection {ref_id}: {e}", file=sys.stderr)
            self.errors += 1
            return False

    def run(self, collection_filter: str = "all"):
        """Run the full import."""
        mode = "DRY RUN" if self.dry_run else "IMPORT"
        print(f"\n{'=' * 60}")
        print(f"  Qdrant → SQLite Migration ({mode})")
        print(f"  Source: {QDRANT_URL}")
        print(f"  Target: {self.db_path}")
        print(f"{'=' * 60}\n")

        # Discover collections
        all_cols = self.list_collections()

        # Filter collections
        chunk_cols = []
        reflection_cols = []
        for col in all_cols:
            name = col["name"]
            pts = col["points"]
            if pts == 0:
                continue
            if name.startswith("reflections_local"):
                reflection_cols.append(col)
            elif collection_filter == "all":
                if name.startswith("csr_") or name.startswith("conv_"):
                    chunk_cols.append(col)
            elif collection_filter == "csr":
                if name.startswith("csr_"):
                    chunk_cols.append(col)
            elif collection_filter == "conv":
                if name.startswith("conv_"):
                    chunk_cols.append(col)

        total_chunk_pts = sum(c["points"] for c in chunk_cols)
        total_refl_pts = sum(c["points"] for c in reflection_cols)

        print(f"  Collections to import:")
        print(f"    Chunk collections: {len(chunk_cols)} ({total_chunk_pts:,} points)")
        print(f"    Reflection collections: {len(reflection_cols)} ({total_refl_pts:,} points)")
        print()

        # Load existing IDs for dedup
        existing_chunk_ids = self.get_existing_chunk_ids()
        existing_refl_ids = self.get_existing_reflection_ids()
        print(f"  Existing in SQLite: {len(existing_chunk_ids)} chunks, {len(existing_refl_ids)} reflections")
        print()

        # Import chunks
        if chunk_cols:
            print(f"  Phase 1: Importing chunks from {len(chunk_cols)} collections...")
            overall = ProgressBar(total_chunk_pts, desc="Overall chunks")

            for col in chunk_cols:
                name = col["name"]
                pts = col["points"]
                project = normalize_project_name(name)
                sys.stderr.write(f"\n  → {name} ({pts} pts, project: {project})\n")

                points = self.scroll_collection(name)
                col_bar = ProgressBar(len(points), desc=f"  {project[:20]}")

                for point in points:
                    self.import_chunk_point(point, name, existing_chunk_ids)
                    col_bar.update()
                    overall.update()

                col_bar.close()

                if not self.dry_run and self.conn:
                    self.conn.commit()

            overall.close()
            print()

        # Import reflections
        if reflection_cols:
            print(f"  Phase 2: Importing reflections...")
            for col in reflection_cols:
                name = col["name"]
                pts = col["points"]
                sys.stderr.write(f"\n  → {name} ({pts} pts)\n")

                points = self.scroll_collection(name)
                bar = ProgressBar(len(points), desc="Reflections")

                for point in points:
                    self.import_reflection_point(point, existing_refl_ids)
                    bar.update()

                bar.close()

                if not self.dry_run and self.conn:
                    self.conn.commit()
            print()

        # Final stats
        elapsed = time.time() - self.start_time
        total_imported = self.chunks_imported + self.reflections_imported
        rate = total_imported / elapsed if elapsed > 0 else 0

        print(f"{'=' * 60}")
        print(f"  RESULTS")
        print(f"{'=' * 60}")
        print(f"  Chunks imported:     {self.chunks_imported:>6,}")
        print(f"  Chunks skipped:      {self.chunks_skipped:>6,} (duplicates)")
        print(f"  Reflections imported: {self.reflections_imported:>5,}")
        print(f"  Reflections skipped:  {self.reflections_skipped:>5,} (duplicates)")
        print(f"  Errors:              {self.errors:>6,}")
        print(f"  ─────────────────────────────")
        print(f"  Total imported:      {total_imported:>6,}")
        print(f"  Import speed:        {rate:>6.0f} pts/sec")
        print(f"  Elapsed:             {elapsed:>6.1f}s")
        print(f"{'=' * 60}")

        if not self.dry_run and self.conn:
            # Show final DB state
            chunks_count = self.conn.execute("SELECT COUNT(*) FROM chunks").fetchone()[0]
            refl_count = self.conn.execute("SELECT COUNT(*) FROM reflections").fetchone()[0]
            projects = self.conn.execute(
                "SELECT project_name, COUNT(*) FROM chunks GROUP BY 1 ORDER BY 2 DESC"
            ).fetchall()
            print(f"\n  Final DB state:")
            print(f"    Total chunks:      {chunks_count:,}")
            print(f"    Total reflections: {refl_count:,}")
            print(f"    Projects:")
            for proj, count in projects:
                print(f"      {proj}: {count:,}")
            print()

        if self.conn:
            self.conn.close()


def main():
    parser = argparse.ArgumentParser(description="Import Qdrant data into CSR Rust engine SQLite DB")
    parser.add_argument("--dry-run", action="store_true", help="Preview import without writing")
    parser.add_argument("--collections", choices=["all", "csr", "conv"], default="all",
                        help="Which collections to import (default: all)")
    parser.add_argument("--db", type=str, default=str(DB_PATH), help=f"SQLite DB path (default: {DB_PATH})")
    parser.add_argument("--qdrant-url", type=str, default=QDRANT_URL, help=f"Qdrant URL (default: {QDRANT_URL})")
    parser.add_argument("--flush", action="store_true",
                        help="Delete all existing data before import (full re-import)")
    args = parser.parse_args()

    # Flush existing data if requested
    if args.flush and not args.dry_run:
        db_path = Path(args.db)
        if db_path.exists():
            print(f"\n  FLUSH: Clearing all data from {db_path}...")
            conn = sqlite3.connect(str(db_path))
            for table in ["chunk_embeddings", "chunks", "reflection_embeddings", "reflections",
                          "import_state", "enrichment_state"]:
                try:
                    conn.execute(f"DELETE FROM {table}")
                except sqlite3.OperationalError:
                    pass  # Table might not exist
            conn.commit()
            conn.execute("VACUUM")
            conn.close()
            print(f"  FLUSH: Done. DB cleared.\n")

    importer = QdrantToSqliteImporter(
        db_path=Path(args.db),
        qdrant_url=args.qdrant_url,
        dry_run=args.dry_run,
    )
    importer.run(collection_filter=args.collections)


if __name__ == "__main__":
    main()
