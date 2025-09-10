#!/usr/bin/env python3
"""
Debug import of a single file with summary messages
"""

import json
from pathlib import Path

# Target file
test_file = Path.home() / '.claude/projects/-Users-ramakrishnanannaswamy-projects-claude-self-reflect/c072a61e-aebb-4c85-960b-c5ffeafa7115.jsonl'

print(f"Analyzing: {test_file.name}\n")

# Read and analyze the file
all_messages = []
summary_count = 0
user_count = 0
assistant_count = 0
other_count = 0

with open(test_file, 'r') as f:
    for i, line in enumerate(f, 1):
        if line.strip():
            try:
                data = json.loads(line)
                msg_type = data.get('type', 'unknown')
                
                print(f"Line {i}: type={msg_type}", end="")
                
                # Check what would be extracted
                if msg_type == 'summary':
                    summary_count += 1
                    print(f" -> SKIPPED (summary)")
                    continue
                
                # Check for messages with type user/assistant
                if msg_type in ['user', 'assistant']:
                    if 'message' in data and data['message']:
                        msg = data['message']
                        if msg.get('role') and msg.get('content'):
                            all_messages.append(msg)
                            if msg_type == 'user':
                                user_count += 1
                            else:
                                assistant_count += 1
                            
                            # Extract a preview of content
                            content = msg.get('content', '')
                            if isinstance(content, list) and len(content) > 0:
                                first_item = content[0]
                                if isinstance(first_item, dict):
                                    preview = str(first_item.get('content', first_item.get('text', '')))[:50]
                                else:
                                    preview = str(first_item)[:50]
                            else:
                                preview = str(content)[:50]
                            
                            print(f" -> EXTRACTED (role={msg['role']}, preview: {preview}...)")
                        else:
                            print(f" -> NO role/content in message")
                    else:
                        print(f" -> NO message field")
                else:
                    other_count += 1
                    print(f" -> OTHER TYPE")
                    
            except json.JSONDecodeError as e:
                print(f"Line {i}: INVALID JSON - {e}")

print(f"\n=== SUMMARY ===")
print(f"Total lines: {i}")
print(f"Summaries (skipped): {summary_count}")
print(f"User messages: {user_count}")
print(f"Assistant messages: {assistant_count}")
print(f"Other types: {other_count}")
print(f"Total extracted messages: {len(all_messages)}")

# Check for Memento content
memento_found = False
for msg in all_messages:
    content = str(msg.get('content', ''))
    if 'memento' in content.lower():
        memento_found = True
        break

print(f"\nMemento content found in messages: {memento_found}")

if len(all_messages) > 0:
    print(f"\n✅ File SHOULD be importable with {len(all_messages)} messages")
else:
    print(f"\n❌ File would result in ZERO messages imported")