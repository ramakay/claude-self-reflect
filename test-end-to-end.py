#!/usr/bin/env python3
"""End-to-end validation test for collection mismatch fix."""

import os
import time
import json
from datetime import datetime
import subprocess

# Test both local and cloud modes
def test_end_to_end(mode="local"):
    print(f"\n=== END-TO-END TEST: {mode.upper()} MODE ===")
    
    timestamp = int(time.time())
    test_uuid = f"e2e-test-{mode}-{timestamp}"
    
    # Create test conversation
    test_conversation = {
        "type": "conversation",
        "uuid": test_uuid,
        "name": f"End-to-End Test {mode.title()} Mode",
        "created_at": datetime.now().isoformat(),
        "messages": [
            {
                "role": "human",
                "content": f"This is an end-to-end test for {mode} mode collection fix validation"
            },
            {
                "role": "assistant", 
                "content": [{"type": "text", "text": f"Testing collection naming fix in {mode} mode with hash 7f6df0fc"}]
            }
        ]
    }
    
    # Write to correct location for streaming importer
    test_file = f"/Users/ramakrishnanannaswamy/.claude/projects/-Users-ramakrishnanannaswamy-projects-claude-self-reflect/{test_uuid}.jsonl"
    
    print(f"1. Writing test conversation: {test_uuid}")
    with open(test_file, 'w') as f:
        json.dump(test_conversation, f)
    
    write_time = time.time()
    print(f"   File written at: {datetime.fromtimestamp(write_time).strftime('%H:%M:%S')}")
    
    # Wait for streaming importer (should be within 10 seconds)
    print("2. Waiting for streaming importer to process file...")
    max_wait = 15  # seconds
    start_wait = time.time()
    
    # Check if data appears in correct collection
    expected_collection = f"conv_7f6df0fc_{mode}"
    
    print(f"   Expected collection: {expected_collection}")
    
    found = False
    for i in range(max_wait):
        # Check collection point count
        result = subprocess.run([
            "curl", "-s", f"http://localhost:6333/collections/{expected_collection}"
        ], capture_output=True, text=True)
        
        if result.returncode == 0:
            try:
                data = json.loads(result.stdout)
                if data.get("result", {}).get("points_count", 0) > 0:
                    # Search for our specific test
                    search_result = subprocess.run([
                        "curl", "-s", "-X", "POST",
                        f"http://localhost:6333/collections/{expected_collection}/points/scroll",
                        "-H", "Content-Type: application/json",
                        "-d", json.dumps({
                            "filter": {
                                "must": [
                                    {"key": "conversation_uuid", "match": {"value": test_uuid}}
                                ]
                            },
                            "limit": 10
                        })
                    ], capture_output=True, text=True)
                    
                    if search_result.returncode == 0:
                        search_data = json.loads(search_result.stdout)
                        if search_data.get("result", {}).get("points"):
                            found = True
                            found_time = time.time()
                            latency = found_time - write_time
                            print(f"   ✅ FOUND at: {datetime.fromtimestamp(found_time).strftime('%H:%M:%S')}")
                            print(f"   ✅ LATENCY: {latency:.1f} seconds")
                            break
            except:
                pass
        
        time.sleep(1)
        if i % 5 == 0:
            print(f"   Waiting... ({i+1}/{max_wait}s)")
    
    if not found:
        end_time = time.time()
        print(f"   ❌ NOT FOUND after {end_time - write_time:.1f} seconds")
        return False, None
    
    # Clean up
    try:
        os.remove(test_file)
    except:
        pass
        
    return True, latency

if __name__ == "__main__":
    # Test local mode
    success_local, latency_local = test_end_to_end("local")
    
    # Test cloud mode (if we have data there)
    success_cloud, latency_cloud = test_end_to_end("voyage")
    
    print(f"\n=== RESULTS SUMMARY ===")
    print(f"Local Mode:  {'PASS' if success_local else 'FAIL'}" + 
          (f" ({latency_local:.1f}s)" if success_local else ""))
    print(f"Cloud Mode:  {'PASS' if success_cloud else 'FAIL'}" + 
          (f" ({latency_cloud:.1f}s)" if success_cloud else ""))
    
    target_latency = 10.0  # seconds
    if success_local and latency_local <= target_latency:
        print(f"✅ Local mode meets <{target_latency}s requirement")
    elif success_local:
        print(f"⚠️  Local mode exceeds {target_latency}s target")
    
    if success_cloud and latency_cloud <= target_latency:
        print(f"✅ Cloud mode meets <{target_latency}s requirement") 
    elif success_cloud:
        print(f"⚠️  Cloud mode exceeds {target_latency}s target")