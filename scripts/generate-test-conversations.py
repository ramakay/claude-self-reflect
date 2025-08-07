#!/usr/bin/env python3
"""Generate test conversations for stress testing the streaming importer."""

import json
import uuid
import time
import os
from datetime import datetime
from pathlib import Path

def create_conversation(index, size="small"):
    """Generate test conversation with specified size."""
    content_sizes = {
        "small": 100,
        "medium": 500, 
        "large": 2000
    }
    
    # Generate content with realistic conversation patterns
    base_content = f"Test conversation {index} for v2.5.0 streaming importer validation. "
    padding = "This tests memory usage, import speed, and search functionality. " * (content_sizes[size] // 100)
    
    return {
        "type": "conversation",
        "uuid": f"stress-test-{str(uuid.uuid4())}",
        "name": f"Stress Test {index} - {size.upper()}",
        "messages": [
            {
                "role": "human", 
                "content": f"Question {index}: How does the streaming importer handle {size} conversations? {base_content}{padding}"
            },
            {
                "role": "assistant", 
                "content": [
                    {
                        "type": "text", 
                        "text": f"Answer {index}: The streaming importer processes conversations efficiently with operational memory under 50MB. {base_content}{padding}"
                    }
                ]
            }
        ],
        "created_at": datetime.now().isoformat(),
        "updated_at": datetime.now().isoformat(),
        "model": "claude-opus-4",
        "summary": f"Stress test conversation {index}"
    }

def main():
    """Generate test conversations over time."""
    # Create test directory
    test_dir = Path.home() / ".claude" / "projects" / "claude-self-reflect-stress-test"
    test_dir.mkdir(parents=True, exist_ok=True)
    
    print(f"Starting conversation generation at {datetime.now()}")
    print(f"Target directory: {test_dir}")
    print("-" * 60)
    
    # Phase 1: Generate 10 small conversations quickly
    print("\nPhase 1: Small conversations (10 files)")
    for i in range(10):
        conv = create_conversation(i, size="small")
        path = test_dir / f"stress-test-small-{i}.json"
        path.write_text(json.dumps(conv, indent=2))
        print(f"✓ Created small conversation {i}")
        time.sleep(0.5)  # Quick generation
    
    # Phase 2: Generate 10 medium conversations
    print("\nPhase 2: Medium conversations (10 files)")
    for i in range(10):
        conv = create_conversation(i + 10, size="medium")
        path = test_dir / f"stress-test-medium-{i}.json"
        path.write_text(json.dumps(conv, indent=2))
        print(f"✓ Created medium conversation {i + 10}")
        time.sleep(1)  # Moderate pace
    
    # Phase 3: Generate 5 large conversations
    print("\nPhase 3: Large conversations (5 files)")
    for i in range(5):
        conv = create_conversation(i + 20, size="large")
        path = test_dir / f"stress-test-large-{i}.json"
        path.write_text(json.dumps(conv, indent=2))
        print(f"✓ Created large conversation {i + 20}")
        time.sleep(2)  # Slower pace for large files
    
    # Phase 4: Simulate active session updates
    print("\nPhase 4: Simulating active session updates")
    active_file = test_dir / "active-session.json"
    for i in range(5):
        conv = create_conversation(100 + i, size="medium")
        conv["name"] = f"Active Session Update {i}"
        conv["messages"].append({
            "role": "human",
            "content": f"Update {i}: Testing active session detection"
        })
        conv["messages"].append({
            "role": "assistant",
            "content": [{"type": "text", "text": f"Response {i}: Active session detected and prioritized"}]
        })
        active_file.write_text(json.dumps(conv, indent=2))
        print(f"✓ Updated active session (iteration {i})")
        time.sleep(3)
    
    print("\n" + "=" * 60)
    print(f"Conversation generation complete at {datetime.now()}")
    print(f"Total files created: 25 + 5 updates")
    print(f"Total time: ~45 seconds")
    print("=" * 60)

if __name__ == "__main__":
    main()