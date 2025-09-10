#!/usr/bin/env python3
"""
Process staged conversation imports from PreCompact hook
Designed to be called by the streaming watcher or run standalone
"""

import json
import os
import sys
import fcntl
import tempfile
from pathlib import Path
from datetime import datetime, timedelta
import shutil
import logging

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))
sys.path.insert(0, str(Path(__file__).parent))

# Set up logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class StagedImportProcessor:
    """Process conversations staged by PreCompact hook"""
    
    def __init__(self):
        # Use secure temp directory
        temp_base = os.environ.get('CLAUDE_STAGING_DIR', tempfile.gettempdir())
        self.staging_dir = Path(temp_base) / "claude-reflect-staging"
        self.processed_dir = self.staging_dir / "processed"
        self.failed_dir = self.staging_dir / "failed"
        
        # Create directories with proper permissions
        self.staging_dir.mkdir(exist_ok=True, mode=0o755)
        self.processed_dir.mkdir(exist_ok=True, mode=0o755)
        self.failed_dir.mkdir(exist_ok=True, mode=0o755)
    
    def find_staged_files(self):
        """Find files ready for import"""
        ready_files = []
        
        if not self.staging_dir.exists():
            return ready_files
        
        # Look for .ready markers
        for ready_marker in self.staging_dir.glob("*.ready"):
            conversation_id = ready_marker.stem
            jsonl_file = self.staging_dir / f"{conversation_id}.jsonl"
            meta_file = self.staging_dir / f"{conversation_id}.meta"
            
            # Check if both files exist
            if jsonl_file.exists() and meta_file.exists():
                ready_files.append({
                    'conversation_id': conversation_id,
                    'jsonl_file': jsonl_file,
                    'meta_file': meta_file,
                    'ready_marker': ready_marker
                })
        
        return ready_files
    
    def process_staged_file(self, staged_info):
        """Process a single staged file"""
        conversation_id = staged_info['conversation_id']
        jsonl_file = staged_info['jsonl_file']
        meta_file = staged_info['meta_file']
        ready_marker = staged_info['ready_marker']
        
        try:
            # Load metadata
            with open(meta_file, 'r') as f:
                metadata = json.load(f)
            
            logger.info(f"Processing staged conversation: {conversation_id[:8]}... (trigger: {metadata.get('trigger', 'unknown')})")
            
            # Import to Qdrant using the unified importer
            # This reuses existing import logic
            project_name = metadata.get('project', 'unknown')
            
            # Fix for old metadata that has full path as project name
            # If project name starts with - and contains -projects-, extract the actual project name
            if project_name.startswith('-') and '-projects-' in project_name:
                # Extract the project name after the last '-projects-'
                actual_project = project_name.split('-projects-')[-1]
                logger.info(f"Correcting project name from '{project_name}' to '{actual_project}'")
                project_name = actual_project
            
            # Call the import function directly
            success = self._import_to_qdrant(str(jsonl_file), project_name, conversation_id)
            
            if success:
                # Update MCP state file so it knows about this import
                self._update_mcp_state(metadata, conversation_id)
                
                # Move to processed
                processed_jsonl = self.processed_dir / f"{conversation_id}.jsonl"
                processed_meta = self.processed_dir / f"{conversation_id}.meta"
                
                shutil.move(str(jsonl_file), str(processed_jsonl))
                shutil.move(str(meta_file), str(processed_meta))
                ready_marker.unlink()
                
                logger.info(f"✅ Successfully imported staged conversation: {conversation_id[:8]}...")
                return True
            else:
                # Move to failed
                failed_jsonl = self.failed_dir / f"{conversation_id}.jsonl"
                failed_meta = self.failed_dir / f"{conversation_id}.meta"
                
                shutil.move(str(jsonl_file), str(failed_jsonl))
                shutil.move(str(meta_file), str(failed_meta))
                ready_marker.unlink()
                
                logger.error(f"❌ Failed to import staged conversation: {conversation_id[:8]}...")
                return False
                
        except Exception as e:
            logger.error(f"Error processing {conversation_id}: {e}")
            
            # Move to failed directory
            try:
                failed_jsonl = self.failed_dir / f"{conversation_id}.jsonl"
                failed_meta = self.failed_dir / f"{conversation_id}.meta"
                
                if jsonl_file.exists():
                    shutil.move(str(jsonl_file), str(failed_jsonl))
                if meta_file.exists():
                    shutil.move(str(meta_file), str(failed_meta))
                if ready_marker.exists():
                    ready_marker.unlink()
            except:
                pass
            
            return False
    
    def _import_to_qdrant(self, jsonl_path, project_name, conversation_id):
        """Import conversation to Qdrant using unified importer logic"""
        try:
            # Import the necessary functions
            # Add parent directory to path to allow import
            sys.path.insert(0, str(Path(__file__).parent))
            import import_conversations_unified
            from qdrant_client import QdrantClient
            
            # Connect to Qdrant
            qdrant_url = os.getenv('QDRANT_URL', 'http://localhost:6333')
            client = QdrantClient(url=qdrant_url)
            
            # Parse the JSONL file
            messages = import_conversations_unified.parse_jsonl_file(jsonl_path)
            
            if not messages:
                logger.warning(f"No messages found in {jsonl_path}")
                return False
            
            # Process messages into chunks
            chunks = import_conversations_unified.process_messages_to_chunks(messages, conversation_id)
            
            if not chunks:
                logger.warning(f"No chunks created from {len(messages)} messages")
                return False
            
            # Sanitize project name for collection
            collection_name = f"conv_{project_name.replace('-', '_').replace('/', '_')[:50]}_local"
            
            # Ensure collection exists
            collection = import_conversations_unified.get_or_create_collection(client, collection_name)
            
            # Upload chunks
            success = import_conversations_unified.upload_chunks_to_qdrant(client, collection_name, chunks)
            
            if success:
                logger.info(f"Uploaded {len(chunks)} chunks to collection {collection_name}")
            
            return success
            
        except (ImportError, ModuleNotFoundError) as e:
            logger.info(f"Module import not available ({e}), using subprocess (this is normal)")
            # Fallback to calling the script directly - this is actually fine and works well
            return self._import_via_subprocess(jsonl_path, project_name)
        except Exception as e:
            logger.error(f"Import to Qdrant failed: {e}")
            return False
    
    def _import_via_subprocess(self, jsonl_path, project_name):
        """Fallback: Import via subprocess call to unified importer"""
        try:
            import subprocess
            
            script_path = Path(__file__).parent / "import-conversations-unified.py"
            if not script_path.exists():
                logger.error(f"Import script not found: {script_path}")
                return False
            
            # Set up environment
            env = os.environ.copy()
            # Use the staging directory as LOGS_DIR so importer can find the file
            env['LOGS_DIR'] = str(Path(jsonl_path).parent.parent)
            env['IMPORT_PROJECT'] = project_name
            
            # Call the import script with the specific file
            # Timeout configurable via environment variable
            timeout = int(os.environ.get('IMPORT_TIMEOUT', '120'))
            
            result = subprocess.run(
                [sys.executable, str(script_path), str(jsonl_path)],
                env=env,
                capture_output=True,
                text=True,
                timeout=timeout
            )
            
            if result.returncode == 0:
                logger.info("Import via subprocess succeeded")
                return True
            else:
                logger.error(f"Import subprocess failed: {result.stderr}")
                return False
                
        except subprocess.TimeoutExpired:
            logger.error("Import subprocess timed out")
            return False
        except Exception as e:
            logger.error(f"Subprocess import failed: {e}")
            return False
    
    def process_all(self):
        """Process all staged files"""
        staged_files = self.find_staged_files()
        
        if not staged_files:
            return 0
        
        logger.info(f"Found {len(staged_files)} staged conversations")
        
        processed = 0
        for staged_info in staged_files:
            if self.process_staged_file(staged_info):
                processed += 1
        
        logger.info(f"Processed {processed}/{len(staged_files)} staged conversations")
        return processed
    
    def _update_mcp_state(self, metadata, conversation_id):
        """Update MCP state file to track this import with atomic operations"""
        try:
            # Find the MCP state file - check common locations
            state_file_locations = [
                Path.home() / ".claude-self-reflect" / "config" / "imported-files.json",
                Path.home() / "claude-self-reflect" / "config" / "imported-files.json",
                Path("config") / "imported-files.json"
            ]
            
            state_file = None
            for location in state_file_locations:
                if location.exists():
                    state_file = location
                    break
            
            if not state_file:
                # Try to create in the first valid location
                for location in state_file_locations:
                    try:
                        location.parent.mkdir(parents=True, exist_ok=True)
                        state_file = location
                        break
                    except:
                        continue
                
                if not state_file:
                    logger.warning("Could not find or create MCP state file")
                    return
            
            # Use atomic write with file locking
            temp_file = state_file.with_suffix('.tmp')
            lock_file = state_file.with_suffix('.lock')
            
            # Ensure lock file exists
            lock_file.touch(exist_ok=True)
            
            with open(lock_file, 'w') as lock:
                try:
                    # Acquire exclusive lock
                    # Try to acquire lock with brief retry on contention
                    import time
                    max_attempts = 3
                    for attempt in range(max_attempts):
                        try:
                            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                            break
                        except BlockingIOError:
                            if attempt < max_attempts - 1:
                                time.sleep(0.1)  # Brief wait before retry
                            else:
                                raise
                    
                    # Load existing state or create new
                    if state_file.exists():
                        with open(state_file, 'r') as f:
                            state = json.load(f)
                    else:
                        state = {}
                    
                    # Add this file to the state
                    original_path = metadata.get('original_path', '')
                    if original_path:
                        state[original_path] = {
                            'imported_at': metadata.get('staged_at', datetime.now().isoformat()),
                            'chunks': 1,  # We don't have exact count but at least 1
                            'project': metadata.get('project', 'unknown'),
                            'conversation_id': conversation_id
                        }
                        
                        # Write to temp file first
                        with open(temp_file, 'w') as f:
                            json.dump(state, f, indent=2)
                        
                        # Atomic rename (on POSIX systems)
                        temp_file.replace(state_file)
                        
                        logger.info(f"Updated MCP state file: {state_file}")
                    
                finally:
                    # Release lock
                    fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
            
        except BlockingIOError:
            logger.warning("Could not acquire lock on MCP state file - another process is updating")
        except Exception as e:
            logger.warning(f"Could not update MCP state: {e}")
    
    def cleanup_old_files(self, days=7):
        """Clean up old processed/failed files based on staging date"""
        cutoff = datetime.now() - timedelta(days=days)
        
        for directory in [self.processed_dir, self.failed_dir]:
            if not directory.exists():
                continue
            
            # Check metadata files for actual processing time
            for meta_file in directory.glob("*.meta"):
                try:
                    with open(meta_file, 'r') as f:
                        meta = json.load(f)
                    
                    # Parse staged_at date
                    staged_at_str = meta.get('staged_at', '')
                    if staged_at_str:
                        staged_at = datetime.fromisoformat(staged_at_str)
                        
                        # If older than cutoff, remove associated files
                        if staged_at < cutoff:
                            base_name = meta_file.stem
                            for ext in ['.meta', '.jsonl']:
                                file_to_remove = directory / f"{base_name}{ext}"
                                if file_to_remove.exists():
                                    file_to_remove.unlink()
                                    logger.debug(f"Cleaned up old file: {file_to_remove.name}")
                                    
                except (json.JSONDecodeError, ValueError, OSError) as e:
                    logger.debug(f"Error processing {meta_file}: {e}")
                    # Fall back to mtime for corrupted metadata
                    if meta_file.stat().st_mtime < cutoff.timestamp():
                        try:
                            meta_file.unlink()
                        except:
                            pass

def main():
    """Main entry point for standalone execution"""
    processor = StagedImportProcessor()
    
    # Process all staged files
    count = processor.process_all()
    
    # Clean up old files
    processor.cleanup_old_files()
    
    if count > 0:
        print(f"✅ Processed {count} staged conversations")
    else:
        print("No staged conversations found")
    
    return 0 if count >= 0 else 1

if __name__ == "__main__":
    sys.exit(main())