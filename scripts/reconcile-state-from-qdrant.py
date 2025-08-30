#!/usr/bin/env python3
"""
Reconcile all state files with Qdrant as the single source of truth.
This fixes the mess of Docker ghosts, partial imports, and conflicting states.
"""
import json
import os
from pathlib import Path
from datetime import datetime
from collections import defaultdict
from qdrant_client import QdrantClient
import hashlib

def get_collection_name(project_name: str, suffix: str = "local") -> str:
    """Generate collection name from project name."""
    name_hash = hashlib.md5(project_name.encode()).hexdigest()[:8]
    return f"conv_{name_hash}_{suffix}"

def normalize_project_name(project_dir: str) -> str:
    """Extract clean project name from directory name."""
    if '-projects-' in project_dir:
        # Extract everything after '-projects-'
        return project_dir.split('-projects-', 1)[1]
    return project_dir

def main():
    print("=" * 70)
    print("CLAUDE SELF-REFLECT STATE RECONCILIATION")
    print("Building true state from Qdrant collections...")
    print("=" * 70)
    
    client = QdrantClient(url="http://localhost:6333")
    
    # Step 1: Inventory what's in Qdrant
    print("\n📊 STEP 1: Scanning Qdrant collections...")
    collections = client.get_collections().collections
    conv_collections = [c for c in collections if c.name.startswith("conv_")]
    
    total_points = 0
    imported_conversations = defaultdict(set)  # project -> set of conversation IDs
    project_chunks = defaultdict(int)  # project -> chunk count
    
    for collection in conv_collections:
        info = client.get_collection(collection.name)
        if info.points_count > 0:
            # Sample points to get conversation IDs
            scroll_result = client.scroll(
                collection_name=collection.name,
                limit=min(1000, info.points_count),
                with_payload=True,
                with_vectors=False
            )
            
            points = scroll_result[0]
            
            for point in points:
                if point.payload:
                    project = point.payload.get("project", "unknown")
                    conv_id = point.payload.get("conversation_id")
                    if conv_id:
                        imported_conversations[project].add(conv_id)
                        project_chunks[project] += 1
            
            total_points += info.points_count
    
    print(f"✅ Found {total_points} total chunks across {len(conv_collections)} collections")
    print(f"✅ Found {sum(len(convs) for convs in imported_conversations.values())} unique conversations")
    
    # Step 2: Map to actual files on disk
    print("\n📁 STEP 2: Mapping to file system...")
    projects_dir = Path.home() / ".claude" / "projects"
    
    true_state = {"imported_files": {}}
    files_to_import = []
    
    for project_dir in projects_dir.iterdir():
        if not project_dir.is_dir():
            continue
            
        project_name = normalize_project_name(project_dir.name)
        
        # Find all JSONL files in this project
        jsonl_files = list(project_dir.glob("*.jsonl"))
        
        # Check which ones are imported based on Qdrant
        for jsonl_file in jsonl_files:
            conv_id = jsonl_file.stem
            file_path = str(jsonl_file)
            
            # Check if this conversation is in any of the Qdrant collections
            is_imported = False
            for proj, convs in imported_conversations.items():
                if conv_id in convs:
                    is_imported = True
                    # Add to true state
                    true_state["imported_files"][file_path] = {
                        "imported_at": datetime.now().isoformat(),
                        "project": project_name,
                        "conversation_id": conv_id,
                        "collection": get_collection_name(project_name)
                    }
                    break
            
            if not is_imported:
                files_to_import.append(file_path)
    
    imported_count = len(true_state["imported_files"])
    total_files = len(list(projects_dir.glob("**/*.jsonl")))
    
    print(f"✅ Mapped {imported_count} imported files")
    print(f"📝 Found {len(files_to_import)} files NOT yet imported")
    print(f"📈 True import percentage: {imported_count / total_files * 100:.1f}%")
    
    # Step 3: Write reconciled state files
    print("\n💾 STEP 3: Writing reconciled state files...")
    
    # Backup existing state files
    state_files = [
        Path.home() / ".claude-self-reflect" / "config" / "imported-files.json",
        Path.home() / "config" / "csr-watcher.json"
    ]
    
    for state_file in state_files:
        if state_file.exists():
            backup_path = state_file.with_suffix('.json.pre-reconcile')
            state_file.rename(backup_path)
            print(f"  Backed up: {state_file.name} -> {backup_path.name}")
    
    # Write MCP state (imported-files.json)
    mcp_state_path = Path.home() / ".claude-self-reflect" / "config" / "imported-files.json"
    mcp_state_path.parent.mkdir(parents=True, exist_ok=True)
    with open(mcp_state_path, 'w') as f:
        json.dump(true_state, f, indent=2)
    print(f"  ✅ Wrote MCP state: {imported_count} files")
    
    # Write watcher state (csr-watcher.json)
    watcher_state_path = Path.home() / "config" / "csr-watcher.json"
    watcher_state_path.parent.mkdir(parents=True, exist_ok=True)
    with open(watcher_state_path, 'w') as f:
        json.dump(true_state, f, indent=2)
    print(f"  ✅ Wrote watcher state: {imported_count} files")
    
    # Step 4: Generate import list for remaining files
    print("\n📋 STEP 4: Files needing import...")
    
    # Group by project for better visibility
    by_project = defaultdict(list)
    for file_path in files_to_import:
        project_dir = Path(file_path).parent.name
        project_name = normalize_project_name(project_dir)
        by_project[project_name].append(file_path)
    
    # Write import queue
    import_queue_path = Path.home() / "config" / "import-queue.json"
    import_queue = {
        "generated_at": datetime.now().isoformat(),
        "total_files": len(files_to_import),
        "files": files_to_import,
        "by_project": {k: len(v) for k, v in by_project.items()}
    }
    
    with open(import_queue_path, 'w') as f:
        json.dump(import_queue, f, indent=2)
    
    print(f"  ✅ Wrote import queue: {len(files_to_import)} files")
    
    # Show breakdown
    print("\n📊 Files to import by project:")
    for project, files in sorted(by_project.items(), key=lambda x: len(x[1]), reverse=True):
        print(f"  {project}: {len(files)} files")
    
    print("\n" + "=" * 70)
    print("✅ RECONCILIATION COMPLETE!")
    print(f"  Current state: {imported_count}/{total_files} files ({imported_count/total_files*100:.1f}%)")
    print(f"  Remaining: {len(files_to_import)} files")
    print(f"  Import queue saved to: {import_queue_path}")
    print("\nNext step: Run import-remaining.py to process the queue")
    print("=" * 70)

if __name__ == "__main__":
    main()