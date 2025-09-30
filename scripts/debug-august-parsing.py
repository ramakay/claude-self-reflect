#!/usr/bin/env python3
"""Debug why August files aren't parsing properly."""

import json
import sys

def parse_jsonl_file(file_path):
    """Parse JSONL file and extract messages."""
    messages = []
    
    with open(file_path, 'r', encoding='utf-8') as f:
        for line_num, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            
            try:
                data = json.loads(line)
                
                # Skip summary messages
                if data.get('type') == 'summary':
                    print(f"Line {line_num}: Skipping summary")
                    continue
                
                # Handle messages with type user/assistant at root level
                if data.get('type') in ['user', 'assistant']:
                    if 'message' in data and data['message']:
                        msg = data['message']
                        if msg.get('role') and msg.get('content'):
                            content = msg['content']
                            if isinstance(content, list):
                                text_parts = []
                                for item in content:
                                    if isinstance(item, dict) and item.get('type') == 'text':
                                        text_parts.append(item.get('text', ''))
                                    elif isinstance(item, str):
                                        text_parts.append(item)
                                content = '\n'.join(text_parts)
                            
                            if content:
                                messages.append({
                                    'role': msg['role'],
                                    'content': content[:200] + '...' if len(content) > 200 else content,
                                    'line': line_num
                                })
                                print(f"Line {line_num}: Extracted {msg['role']} message ({len(content)} chars)")
                            else:
                                print(f"Line {line_num}: Empty content for {msg['role']}")
                        else:
                            print(f"Line {line_num}: Missing role or content in message field")
                    else:
                        print(f"Line {line_num}: No message field for type={data.get('type')}")
                else:
                    print(f"Line {line_num}: Unknown type={data.get('type')}")
                    
            except Exception as e:
                print(f"Line {line_num}: Parse error - {e}")
    
    return messages

if __name__ == "__main__":
    # Use home directory path
    from pathlib import Path
    home = Path.home()
    file_path = home / ".claude" / "projects" / f"-{home}-projects-claude-self-reflect" / "7b3354ed-d6d2-4eab-b328-1fced4bb63bb.jsonl"
    file_path = str(file_path)
    
    print(f"Parsing: {file_path}")
    print("=" * 60)
    
    messages = parse_jsonl_file(file_path)
    
    print("\n" + "=" * 60)
    print(f"Total messages extracted: {len(messages)}")
    
    if messages:
        print("\nFirst 5 messages:")
        for i, msg in enumerate(messages[:5]):
            print(f"\n{i+1}. Line {msg['line']}: {msg['role']}")
            print(f"   Content: {msg['content'][:100]}...")