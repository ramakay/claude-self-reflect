#!/usr/bin/env python3
"""
Batch Monitor Service - Monitors Anthropic Batch API jobs and triggers downstream processes.

This service:
1. Monitors active batch jobs (narratives and evaluations)
2. Retrieves completed results
3. Triggers ground truth generation after narratives complete
4. Manages batch lifecycle
"""

import os
import sys
import time
import json
import logging
import fcntl
import tempfile
from pathlib import Path
from typing import Dict, List, Optional
from datetime import datetime
from dotenv import load_dotenv

# Add project root to path
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

load_dotenv()

import anthropic
from qdrant_client import QdrantClient

# Import centralized config and utilities
sys.path.insert(0, str(Path(__file__).parent))
from config import (
    CSR_BATCH_STATE_DIR,
    QDRANT_URL,
    QDRANT_API_KEY
)
from qdrant_connection import connect_to_qdrant_with_retry

logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


class BatchMonitor:
    """Monitor and manage Anthropic Batch API jobs."""

    def __init__(self):
        """Initialize batch monitor."""
        # Validate API key is configured
        api_key = os.getenv("ANTHROPIC_API_KEY")
        if not api_key:
            raise ValueError(
                "ANTHROPIC_API_KEY environment variable required for batch automation. "
                "Get your key at: https://console.anthropic.com/settings/keys"
            )
        self.client = anthropic.Anthropic(api_key=api_key)

        # Initialize Qdrant with retry logic
        self.qdrant = connect_to_qdrant_with_retry(
            url=QDRANT_URL,
            api_key=QDRANT_API_KEY if QDRANT_API_KEY else None
        )

        # Use centralized config for state directory
        self.state_dir = CSR_BATCH_STATE_DIR

        self.narrative_state = self.state_dir / "narrative_batches.json"
        self.eval_state = self.state_dir / "eval_batches.json"

    def load_batch_state(self, state_file: Path) -> Dict:
        """Load batch state from file with shared lock."""
        if not state_file.exists():
            return {"active": [], "completed": [], "failed": []}

        with open(state_file, 'r', encoding='utf-8') as f:
            # Acquire shared lock for reading
            fcntl.flock(f.fileno(), fcntl.LOCK_SH)
            try:
                return json.load(f)
            finally:
                fcntl.flock(f.fileno(), fcntl.LOCK_UN)

    def save_batch_state(self, state_file: Path, state: Dict):
        """Save batch state to file atomically with exclusive lock."""
        state_file.parent.mkdir(parents=True, exist_ok=True)

        # Write to temp file with exclusive lock, then atomically replace
        with tempfile.NamedTemporaryFile('w', delete=False, dir=state_file.parent, encoding='utf-8') as tmp:
            fcntl.flock(tmp.fileno(), fcntl.LOCK_EX)
            try:
                json.dump(state, tmp, indent=2)
                tmp.flush()
                os.fsync(tmp.fileno())
            finally:
                fcntl.flock(tmp.fileno(), fcntl.LOCK_UN)
            temp_name = tmp.name

        # Atomic replace
        os.replace(temp_name, state_file)

    def register_narrative_batch(self, batch_id: str, metadata: Dict):
        """Register a new narrative generation batch."""
        state = self.load_batch_state(self.narrative_state)

        state["active"].append({
            "batch_id": batch_id,
            "type": "narrative",
            "submitted_at": datetime.utcnow().isoformat(),
            "metadata": metadata
        })

        self.save_batch_state(self.narrative_state, state)
        logger.info(f"Registered narrative batch: {batch_id}")

    def register_eval_batch(self, batch_id: str, metadata: Dict):
        """Register a new evaluation batch."""
        state = self.load_batch_state(self.eval_state)

        state["active"].append({
            "batch_id": batch_id,
            "type": "evaluation",
            "submitted_at": datetime.utcnow().isoformat(),
            "metadata": metadata
        })

        self.save_batch_state(self.eval_state, state)
        logger.info(f"Registered evaluation batch: {batch_id}")

    def check_active_batches(self):
        """Check status of all active batches."""
        # Check narrative batches
        narrative_state = self.load_batch_state(self.narrative_state)
        self._process_batches(narrative_state, self.narrative_state, "narrative")

        # Check evaluation batches
        eval_state = self.load_batch_state(self.eval_state)
        self._process_batches(eval_state, self.eval_state, "evaluation")

    def _process_batches(self, state: Dict, state_file: Path, batch_type: str):
        """Process batches of a given type."""
        still_active = []

        for batch_info in state["active"]:
            batch_id = batch_info["batch_id"]

            try:
                # Check batch status
                batch = self.client.messages.batches.retrieve(batch_id)

                if batch.processing_status == "ended":
                    # Batch completed
                    if batch.request_counts.succeeded > 0:
                        logger.info(f"✅ {batch_type.title()} batch completed: {batch_id} ({batch.request_counts.succeeded} succeeded)")

                        # Process completed batch
                        if batch_type == "narrative":
                            self._handle_completed_narrative_batch(batch_id, batch_info)
                        else:
                            self._handle_completed_eval_batch(batch_id, batch_info)

                        # Move to completed
                        batch_info["completed_at"] = datetime.utcnow().isoformat()
                        batch_info["succeeded"] = batch.request_counts.succeeded
                        batch_info["failed"] = batch.request_counts.errored
                        state["completed"].append(batch_info)
                    else:
                        # All failed
                        logger.error(f"❌ {batch_type.title()} batch failed: {batch_id}")
                        batch_info["failed_at"] = datetime.utcnow().isoformat()
                        state["failed"].append(batch_info)
                else:
                    # Still processing
                    logger.info(f"⏳ {batch_type.title()} batch in progress: {batch_id} ({batch.request_counts.processing} processing)")
                    still_active.append(batch_info)

            except Exception as e:
                logger.error(f"Error checking batch {batch_id}: {e}")
                still_active.append(batch_info)  # Keep in active list for retry

        # Update state
        state["active"] = still_active
        self.save_batch_state(state_file, state)

    def _handle_completed_narrative_batch(self, batch_id: str, batch_info: Dict):
        """Handle completed narrative batch - trigger evaluation generation."""
        logger.info(f"📝 Processing completed narrative batch: {batch_id}")

        try:
            # Retrieve results (already handled by batch_import_all_projects.py)
            # Just trigger evaluation generation for these new narratives

            conversation_ids = batch_info.get("metadata", {}).get("conversation_ids", [])

            if conversation_ids:
                logger.info(f"🎯 Triggering evaluation generation for {len(conversation_ids)} conversations")

                # Import and run evaluation generator
                sys.path.insert(0, str(Path(__file__).parent.parent.parent / "docs" / "design"))
                from batch_ground_truth_generator import BatchGroundTruthGenerator

                generator = BatchGroundTruthGenerator()

                # Fetch these specific narratives
                import requests
                url = f"{generator.qdrant_url}/collections/{generator.collection_name}/points/scroll"
                payload = {
                    'limit': len(conversation_ids),
                    'with_payload': True,
                    'with_vector': False,
                    'filter': {
                        'must': [
                            {
                                'key': 'conversation_id',
                                'match': {
                                    'any': conversation_ids
                                }
                            }
                        ]
                    }
                }

                response = requests.post(url, json=payload, timeout=10)
                response.raise_for_status()
                data = response.json()
                points = data.get('result', {}).get('points', [])

                if points:
                    # Create and submit evaluation batch
                    requests_file = generator.create_batch_requests(
                        points,
                        output_file=f'auto_eval_{batch_id[:8]}.jsonl'
                    )

                    eval_batch_id = generator.submit_batch(requests_file)

                    # Register the evaluation batch
                    self.register_eval_batch(eval_batch_id, {
                        "triggered_by": batch_id,
                        "conversation_ids": conversation_ids,
                        "auto_generated": True
                    })

                    logger.info(f"✅ Evaluation batch submitted: {eval_batch_id}")

        except Exception as e:
            logger.error(f"Error handling narrative batch completion: {e}", exc_info=True)

    def _handle_completed_eval_batch(self, batch_id: str, batch_info: Dict):
        """Handle completed evaluation batch - results already in Qdrant."""
        logger.info(f"📊 Evaluation batch completed: {batch_id}")

        # Results are already pushed to Qdrant by batch_ground_truth_generator.py
        # Just log completion
        logger.info(f"✅ {batch_info.get('metadata', {}).get('conversation_count', 'N/A')} evaluations stored in Qdrant")

    def run_once(self):
        """Run one monitoring cycle."""
        logger.info("🔍 Checking active batches...")
        self.check_active_batches()

    def run_forever(self, interval: int = 60):
        """Run monitoring loop forever."""
        logger.info(f"🚀 Batch monitor started (checking every {interval}s)")

        while True:
            try:
                self.run_once()
                time.sleep(interval)
            except KeyboardInterrupt:
                logger.info("👋 Batch monitor stopped")
                break
            except Exception as e:
                logger.error(f"Error in monitoring loop: {e}", exc_info=True)
                time.sleep(interval)


def main():
    """Main entry point."""
    import argparse

    parser = argparse.ArgumentParser(description="Batch Monitor Service")
    parser.add_argument("--once", action="store_true", help="Run once and exit")
    parser.add_argument("--interval", type=int, default=60, help="Check interval in seconds")

    args = parser.parse_args()

    monitor = BatchMonitor()

    if args.once:
        monitor.run_once()
    else:
        monitor.run_forever(interval=args.interval)


if __name__ == "__main__":
    main()
