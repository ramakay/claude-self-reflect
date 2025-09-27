#!/usr/bin/env python3
"""
File Pattern Watcher for Claude Self-Reflect
Watches for file modifications in conversations and extracts patterns using AST-GREP approach
"""

import os
import json
import time
import subprocess
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Any, Optional
import logging

# Add parent directory to path for imports
import sys
sys.path.append(str(Path(__file__).parent))
from pattern_registry import extract_semantic_patterns

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class FilePatternWatcher:
    """Watches files mentioned in conversations and extracts their patterns."""

    def __init__(self):
        self.pattern_cache = {}
        self.file_timeline = {}  # Track pattern evolution

    def extract_file_modifications(self, conversation_data: Dict) -> List[Dict]:
        """Extract file modifications from conversation."""
        modifications = []

        if 'message' in conversation_data and conversation_data['message']:
            msg = conversation_data['message']
            content = msg.get('content', [])

            # Check for tool uses
            if isinstance(content, list):
                for item in content:
                    if item.get('type') == 'tool_use':
                        tool_name = item.get('name', '')
                        tool_input = item.get('input', {})

                        # Track file modifications
                        if tool_name in ['Edit', 'Write', 'MultiEdit', 'NotebookEdit']:
                            file_path = tool_input.get('file_path', '')
                            if file_path:
                                modifications.append({
                                    'file': self.normalize_path(file_path),
                                    'action': tool_name,
                                    'timestamp': conversation_data.get('timestamp', ''),
                                    'conversation_id': conversation_data.get('sessionId', '')
                                })

                        # Track file reads (for context)
                        elif tool_name == 'Read':
                            file_path = tool_input.get('file_path', '')
                            if file_path:
                                modifications.append({
                                    'file': self.normalize_path(file_path),
                                    'action': 'Read',
                                    'timestamp': conversation_data.get('timestamp', ''),
                                    'conversation_id': conversation_data.get('sessionId', '')
                                })

        return modifications

    def normalize_path(self, path: str) -> str:
        """Normalize file path for consistent tracking."""
        # Remove user-specific parts
        path = path.replace('/Users/', '~/')
        path = path.replace('\\Users\\', '~\\')

        # Convert to forward slashes
        path = path.replace('\\', '/')

        # Get just the relative path from project root if possible
        if '/projects/' in path:
            parts = path.split('/projects/')
            if len(parts) > 1:
                return 'projects/' + parts[1]

        return path

    def analyze_file_patterns(self, file_path: str) -> Dict[str, Any]:
        """Analyze patterns in a specific file."""
        try:
            # Expand path
            expanded_path = os.path.expanduser(file_path)

            if not os.path.exists(expanded_path):
                return {'error': f'File not found: {file_path}'}

            # Read file content
            with open(expanded_path, 'r', encoding='utf-8') as f:
                content = f.read()

            # Extract patterns using pattern registry
            patterns = extract_semantic_patterns(content)

            # Try AST-GREP if available
            ast_grep_patterns = self.run_ast_grep(expanded_path)
            if ast_grep_patterns:
                patterns['ast_grep_patterns'] = ast_grep_patterns

            return patterns

        except Exception as e:
            logger.error(f"Error analyzing {file_path}: {e}")
            return {'error': str(e)}

    def run_ast_grep(self, file_path: str) -> Optional[List[str]]:
        """Run ast-grep on file to find patterns."""
        try:
            # Check if ast-grep is installed
            result = subprocess.run(['which', 'sg'], capture_output=True, text=True)
            if result.returncode != 0:
                return None

            # Example patterns to search for
            patterns_to_check = [
                ('async-await', 'async function $FUNC($$$) { await $$$ }'),
                ('try-catch', 'try { $$$ } catch ($ERR) { $$$ }'),
                ('useState', 'const [$VAR, $SETTER] = useState($$$)'),
            ]

            found_patterns = []
            for pattern_id, pattern in patterns_to_check:
                try:
                    # Run ast-grep
                    result = subprocess.run(
                        ['sg', '--pattern', pattern, file_path],
                        capture_output=True,
                        text=True,
                        timeout=5
                    )
                    if result.stdout.strip():
                        found_patterns.append(pattern_id)
                except:
                    pass

            return found_patterns if found_patterns else None

        except Exception as e:
            logger.debug(f"ast-grep not available: {e}")
            return None

    def update_file_timeline(self, file_path: str, patterns: Dict, timestamp: str):
        """Track how patterns evolve in a file over time."""
        if file_path not in self.file_timeline:
            self.file_timeline[file_path] = []

        self.file_timeline[file_path].append({
            'timestamp': timestamp,
            'patterns': patterns.get('patterns', []),
            'categories': patterns.get('pattern_categories', [])
        })

        # Keep only last 10 entries per file
        if len(self.file_timeline[file_path]) > 10:
            self.file_timeline[file_path] = self.file_timeline[file_path][-10:]

    def process_conversation(self, jsonl_path: str) -> Dict[str, Any]:
        """Process a conversation file and extract file patterns."""
        results = {
            'conversation_id': Path(jsonl_path).stem,
            'files_analyzed': {},
            'pattern_summary': {},
            'timeline': []
        }

        try:
            with open(jsonl_path, 'r', encoding='utf-8') as f:
                for line in f:
                    if not line.strip():
                        continue

                    try:
                        data = json.loads(line)
                        modifications = self.extract_file_modifications(data)

                        for mod in modifications:
                            file_path = mod['file']

                            # Skip if recently analyzed (cache for 5 minutes)
                            cache_key = f"{file_path}:{mod['timestamp'][:10]}"
                            if cache_key not in self.pattern_cache:
                                patterns = self.analyze_file_patterns(file_path)
                                self.pattern_cache[cache_key] = patterns

                                # Update timeline
                                self.update_file_timeline(
                                    file_path,
                                    patterns,
                                    mod['timestamp']
                                )
                            else:
                                patterns = self.pattern_cache[cache_key]

                            # Store results
                            if file_path not in results['files_analyzed']:
                                results['files_analyzed'][file_path] = {
                                    'patterns': patterns,
                                    'actions': []
                                }

                            results['files_analyzed'][file_path]['actions'].append({
                                'action': mod['action'],
                                'timestamp': mod['timestamp']
                            })

                            # Update summary
                            for pattern in patterns.get('patterns', []):
                                if pattern not in results['pattern_summary']:
                                    results['pattern_summary'][pattern] = 0
                                results['pattern_summary'][pattern] += 1

                    except json.JSONDecodeError:
                        continue
                    except Exception as e:
                        logger.debug(f"Error processing line: {e}")

        except Exception as e:
            logger.error(f"Error processing conversation {jsonl_path}: {e}")

        return results


def demo_watcher():
    """Demo the file pattern watcher."""
    watcher = FilePatternWatcher()

    # Test on current conversation
    # Use home directory for test file
    test_file = os.path.join(
        os.path.expanduser("~"),
        ".claude/projects/-Users-" + os.path.expanduser("~").replace("/", "-") + "-projects-claude-self-reflect/167aec1d-9226-48a2-9a9b-dc18c679532c.jsonl"
    )

    print("File Pattern Watcher Demo")
    print("=" * 60)

    results = watcher.process_conversation(test_file)

    print(f"Conversation: {results['conversation_id']}")
    print(f"Files analyzed: {len(results['files_analyzed'])}")

    if results['files_analyzed']:
        print("\nFiles and their patterns:")
        for file, data in list(results['files_analyzed'].items())[:5]:
            print(f"\n  {file}:")
            if 'patterns' in data['patterns']:
                patterns = data['patterns'].get('patterns', [])
                if patterns:
                    for p in patterns[:3]:
                        print(f"    - {p}")
                else:
                    print("    - No patterns found")
            print(f"    Actions: {[a['action'] for a in data['actions'][:3]]}")

    if results['pattern_summary']:
        print("\nPattern frequency:")
        for pattern, count in sorted(results['pattern_summary'].items(),
                                    key=lambda x: x[1], reverse=True)[:10]:
            print(f"  {pattern}: {count} occurrences")


if __name__ == "__main__":
    demo_watcher()