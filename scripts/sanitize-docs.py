#!/usr/bin/env python3
"""Sanitize personal data from documentation files."""

import re
from pathlib import Path
import sys

def sanitize_file(file_path):
    """Sanitize personal data in a single file."""
    with open(file_path, 'r') as f:
        content = f.read()

    original = content

    # Replace username with generic placeholder
    content = re.sub(r'/Users/ramakrishnan[a-z]*/projects/', '/Users/YOUR_USERNAME/projects/', content)
    content = re.sub(r'/Users/ramakrishnan[a-z]*/', '/Users/YOUR_USERNAME/', content)
    content = re.sub(r'ramakrishnan[a-z]*', 'YOUR_USERNAME', content)

    # Replace specific project names that might be personal
    # Keep claude-self-reflect as it's the main project
    # But remove references to other personal projects
    content = re.sub(r'/cc-enhance\b', '/your-project', content)

    # Replace any email addresses
    content = re.sub(r'[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}', 'user@example.com', content)

    if content != original:
        with open(file_path, 'w') as f:
            f.write(content)
        return True
    return False

def main():
    """Sanitize all markdown files in docs directory."""
    docs_dir = Path(__file__).parent.parent / 'docs'

    if not docs_dir.exists():
        print("❌ Docs directory not found")
        return 1

    # Find all markdown files
    md_files = list(docs_dir.rglob('*.md'))

    print(f"🔍 Checking {len(md_files)} documentation files...")

    sanitized_count = 0
    for file_path in md_files:
        if sanitize_file(file_path):
            print(f"  ✅ Sanitized: {file_path.relative_to(docs_dir)}")
            sanitized_count += 1

    if sanitized_count > 0:
        print(f"\n✨ Sanitized {sanitized_count} files")
    else:
        print("\n✅ No personal data found in documentation")

    return 0

if __name__ == "__main__":
    sys.exit(main())