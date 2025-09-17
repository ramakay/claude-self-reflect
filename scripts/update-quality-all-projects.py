#!/usr/bin/env python3
"""
Update quality metrics for all projects with conversations.
This ensures the statusline shows quality grades for all projects.
"""

import json
import os
import sys
import re
import shutil
from pathlib import Path
from datetime import datetime
import logging
from typing import Dict, List, Any, Optional

# Add parent directory to path for shared modules
sys.path.insert(0, str(Path(__file__).parent.parent))
from shared.normalization import normalize_project_name

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

def categorize_issues_from_file_reports(file_reports):
    """
    Categorize issues from AST analysis into critical/medium/low.
    """
    critical = 0
    medium = 0
    low = 0

    for file_path, report in file_reports.items():
        for rec in report.get('recommendations', []):
            if 'print statements' in rec or 'console.log' in rec.lower():
                # Extract count from "Replace N print statements"
                import re
                match = re.search(r'(\d+)', rec)
                if match:
                    low += int(match.group(1))
            elif 'anti-patterns' in rec:
                # Extract count from "Fix N anti-patterns"
                import re
                match = re.search(r'Fix (\d+)', rec)
                if match:
                    medium += int(match.group(1))

        # Check top_issues for severity classification
        for issue in report.get('top_issues', []):
            severity = issue.get('severity', 'medium')
            count = issue.get('count', 0)

            if severity == 'high':
                critical += count
            elif severity == 'medium':
                if 'print' in issue.get('id', '') or 'console' in issue.get('id', ''):
                    low += count
                else:
                    medium += count
            else:
                low += count

    return critical, medium, low


def get_quality_icon(critical=0, medium=0, low=0):
    """
    Determine quality icon based on issue severity counts.
    """
    # Icon selection based on highest severity present
    if critical > 0:
        if critical >= 10:
            return "🔴"  # Red circle - Critical issues need immediate attention
        else:
            return "🟠"  # Orange circle - Some critical issues
    elif medium > 0:
        if medium >= 50:
            return "🟡"  # Yellow circle - Many medium issues
        else:
            return "🟢"  # Green circle - Few medium issues
    elif low > 0:
        if low >= 100:
            return "⚪"  # White circle - Many minor issues (prints)
        else:
            return "✅"  # Check mark - Only minor issues
    else:
        return "✨"  # Sparkles - Perfect, no issues


def format_quality_display(critical=0, medium=0, low=0):
    """
    Format quality display with colored dot and colored numbers.
    """
    icon = get_quality_icon(critical, medium, low)

    # Build count display - just colored numbers
    counts = []
    if critical > 0:
        counts.append(f"\033[31m{critical}\033[0m")  # Standard red for critical
    if medium > 0:
        counts.append(f"\033[33m{medium}\033[0m")    # Standard yellow for medium
    if low > 0:
        counts.append(f"\033[37m{low}\033[0m")       # Light gray for low

    if counts:
        return f"{icon} {' '.join(counts)}"
    else:
        return f"{icon}"  # Perfect - no counts needed

def detect_project_type(project_path) -> str:
    """Detect if project contains actual code or is documentation-only.

    Returns:
        'code' - Project contains source code files
        'non-code' - Project is documentation/data only
        'mixed' - Project has both code and documentation
    """
    code_extensions = {'.py', '.js', '.ts', '.jsx', '.tsx', '.java', '.cpp', '.c',
                      '.h', '.hpp', '.rs', '.go', '.rb', '.php', '.swift', '.kt',
                      '.scala', '.r', '.m', '.mm', '.cs', '.vb', '.fs', '.lua'}

    has_code = False
    has_docs = False
    code_files = 0
    doc_files = 0

    # Convert to string if Path object
    project_path_str = str(project_path)

    # First check if this is a conversation path in ~/.claude/projects/
    # These paths look like: /Users/x/.claude/projects/-Users-x-projects-foo
    if '/.claude/projects/' in project_path_str:
        # This is already a conversation directory, check the original project
        # Extract the original path from the encoded name
        parts = project_path_str.split('/.claude/projects/')
        if len(parts) == 2:
            encoded_name = parts[1]
            # Convert back: -Users-x-projects-foo -> /Users/x/projects/foo
            # This is tricky because project names may contain dashes
            # Strategy: split on '-' and rebuild the path intelligently
            if encoded_name.startswith('-'):
                encoded_name = encoded_name[1:]  # Remove leading dash

            # For common patterns like Users-username-projects-projectname
            # We need to identify the path separators vs project name dashes
            parts = encoded_name.split('-')

            # Try to reconstruct the path
            # Common pattern: Users/username/projects/project-name
            if len(parts) >= 3 and parts[0] == 'Users':
                # Likely /Users/username/...
                # Keep combining parts until we hit 'projects' or similar
                path_parts = []
                i = 0
                while i < len(parts):
                    if i <= 2:  # Users, username, projects
                        path_parts.append(parts[i])
                        i += 1
                    else:
                        # Rest is the project name, may contain dashes
                        path_parts.append('-'.join(parts[i:]))
                        break
                original_path = '/' + '/'.join(path_parts)
            else:
                # Fallback: just replace all dashes
                original_path = '/' + encoded_name.replace('-', '/')

            # Ensure path is absolute
            if not original_path.startswith('/'):
                original_path = '/' + original_path

            # Check if the original project exists
            if Path(original_path).exists():
                project_path_str = original_path
            else:
                # Project doesn't exist on disk, likely documentation only
                return 'non-code'

    # Now check the actual project directory for code files
    project_dir = Path(project_path_str)
    if project_dir.exists():
        # Use pathlib to recursively search for code files
        try:
            for file_path in project_dir.rglob('*'):
                if file_path.is_file():
                    # Skip common non-source directories
                    parts = file_path.parts
                    if any(skip in parts for skip in ['venv', '.venv', 'node_modules', '.git',
                                                       '__pycache__', '.pytest_cache', 'dist',
                                                       'build', 'target', '.idea', '.vscode']):
                        continue

                    if file_path.suffix in code_extensions:
                        has_code = True
                        code_files += 1
                        if code_files >= 5:  # Found enough code files
                            break
                    elif file_path.suffix in ['.md', '.txt', '.rst', '.pdf', '.docx', '.xlsx']:
                        has_docs = True
                        doc_files += 1
        except PermissionError:
            # Can't read directory, assume non-code
            pass

    # Determine project type
    if has_code and has_docs:
        return 'mixed'
    elif has_code:
        return 'code'
    else:
        return 'non-code'

class ConversationQualityAnalyzer:
    """Analyze code quality patterns in conversations."""

    def analyze_conversation_file(self, jsonl_path: Path) -> Dict[str, Any]:
        """Analyze a conversation JSONL file for quality patterns."""

        quality_indicators = {
            'good_patterns': 0,
            'bad_patterns': 0,
            'tests_written': 0,
            'documentation_added': 0,
            'refactoring_done': 0,
            'errors_fixed': 0,
            'security_issues_addressed': 0,
            'performance_improvements': 0,
            'code_reviews': 0,
            'todos_completed': 0
        }

        # Pattern detection - more lenient for conversation analysis
        good_patterns = [
            (r'test[s]?\s+(pass|succeed|complete|ran|executed)', 'tests_passing'),
            (r'fix(ed|ing|es)?\s+(bug|issue|problem)', 'bug_fixes'),
            (r'add(ed|ing|s)?\s+(test|spec|unit test)', 'tests_written'),
            (r'refactor(ed|ing|s)?', 'refactoring_done'),
            (r'document(ed|ing|ation|s)?', 'documentation_added'),
            (r'security\s+(fix|patch|update)', 'security_issues_addressed'),
            (r'performance\s+(improv|optim|enhanc)', 'performance_improvements'),
            (r'code\s+review', 'code_reviews'),
            (r'todo.*complet|task.*done|finished', 'todos_completed'),
            (r'type.*check|typescript|typing', 'type_safety'),
            (r'lint|eslint|pylint|ruff', 'linting_clean'),
            (r'implement(ed|ing|s)?', 'implementation'),
            (r'creat(ed|ing|e)\s+(function|class|component|module)', 'new_code'),
            (r'updat(ed|ing|e)', 'updates'),
            (r'improv(ed|ing|e)', 'improvements')
        ]

        bad_patterns = [
            (r'test[s]?\s+fail|failing test', 'failing_tests'),
            (r'error:|exception:|traceback', 'errors'),
            (r'todo:|fixme:|hack:', 'technical_debt'),
            (r'console\.log|print\s*\(', 'debug_statements'),
            (r'any\s+type|typing\.Any', 'weak_typing'),
            (r'disable.*lint|noqa|type:\s*ignore', 'ignored_warnings'),
            (r'sleep\s*\(|time\.sleep', 'blocking_calls'),
            (r'except\s*:', 'broad_exceptions'),
            (r'bug|issue|problem|broken', 'issues'),
            (r'not\s+work|doesn\'t\s+work|fails', 'failures')
        ]

        try:
            with open(jsonl_path, 'r') as f:
                for line in f:
                    try:
                        entry = json.loads(line.strip())

                        # Analyze messages
                        if 'messages' in entry:
                            for msg in entry['messages']:
                                content = str(msg.get('content', ''))
                                content_lower = content.lower()

                                # Check good patterns
                                for pattern, name in good_patterns:
                                    if re.search(pattern, content_lower):
                                        quality_indicators['good_patterns'] += 1

                                # Check bad patterns
                                for pattern, name in bad_patterns:
                                    if re.search(pattern, content_lower):
                                        quality_indicators['bad_patterns'] += 1

                                # Check for tool usage (indicates active development)
                                if msg.get('role') == 'assistant' and isinstance(msg.get('content'), list):
                                    for item in msg['content']:
                                        if isinstance(item, dict) and item.get('type') == 'tool_use':
                                            tool_name = item.get('name', '')
                                            if tool_name in ['Write', 'Edit', 'MultiEdit']:
                                                quality_indicators['good_patterns'] += 2
                                            elif tool_name == 'TodoWrite':
                                                quality_indicators['todos_completed'] += 1
                                            elif tool_name in ['Bash'] and 'test' in str(item.get('input', {})):
                                                quality_indicators['tests_written'] += 1

                    except json.JSONDecodeError:
                        continue
                    except Exception as e:
                        logger.debug(f"Error processing line: {e}")

        except Exception as e:
            logger.error(f"Error reading file {jsonl_path}: {e}")
            return self._default_quality()

        # Calculate score
        total_patterns = quality_indicators['good_patterns'] + quality_indicators['bad_patterns']
        if total_patterns == 0:
            # No patterns found, return neutral score
            return {
                'quality_score': 0.7,
                'quality_grade': 'B',
                'total_issues': 0,
                'good_patterns_found': 0,
                'indicators': quality_indicators
            }

        quality_score = quality_indicators['good_patterns'] / total_patterns

        # Determine grade
        if quality_score >= 0.9:
            grade = 'A+'
        elif quality_score >= 0.8:
            grade = 'A'
        elif quality_score >= 0.7:
            grade = 'B'
        elif quality_score >= 0.6:
            grade = 'C'
        elif quality_score >= 0.5:
            grade = 'D'
        else:
            grade = 'F'

        return {
            'quality_score': quality_score,
            'quality_grade': grade,
            'total_issues': quality_indicators['bad_patterns'],
            'good_patterns_found': quality_indicators['good_patterns'],
            'indicators': quality_indicators
        }

    def _default_quality(self) -> Dict[str, Any]:
        """Return default quality when analysis fails."""
        return {
            'quality_score': 0.5,
            'quality_grade': 'D',
            'total_issues': 0,
            'good_patterns_found': 0,
            'indicators': {}
        }

    def analyze_project(self, project_path: Path, limit: int = 5) -> Dict[str, Any]:
        """Analyze multiple conversations from a project."""

        # Find recent JSONL files
        jsonl_files = sorted(
            project_path.glob("*.jsonl"),
            key=lambda x: x.stat().st_mtime,
            reverse=True
        )[:limit]

        if not jsonl_files:
            return self._default_quality()

        all_scores = []
        total_issues = 0
        total_good = 0

        for jsonl_file in jsonl_files:
            result = self.analyze_conversation_file(jsonl_file)
            all_scores.append(result['quality_score'])
            total_issues += result['total_issues']
            total_good += result['good_patterns_found']

        avg_score = sum(all_scores) / len(all_scores) if all_scores else 0.5

        # Determine grade
        if avg_score >= 0.9:
            grade = 'A+'
        elif avg_score >= 0.8:
            grade = 'A'
        elif avg_score >= 0.7:
            grade = 'B'
        elif avg_score >= 0.6:
            grade = 'C'
        elif avg_score >= 0.5:
            grade = 'D'
        else:
            grade = 'F'

        return {
            'quality_score': avg_score,
            'quality_grade': grade,
            'total_issues': total_issues,
            'good_patterns_found': total_good,
            'files_analyzed': len(jsonl_files)
        }

def get_actual_project_path_from_qdrant(project_name):
    """Query Qdrant to get the actual project path from stored metadata.

    This is more reliable than trying to reconstruct paths from conversation directory names.
    """
    try:
        from qdrant_client import QdrantClient
        client = QdrantClient(url="http://localhost:6333")

        # Get collection name for this project
        from shared.normalization import normalize_project_name
        import hashlib

        normalized = normalize_project_name(project_name)
        # Try both local and voyage collections
        for suffix in ["local", "voyage"]:
            name_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
            collection_name = f"conv_{name_hash}_{suffix}"

            try:
                # Query for a single point to get metadata
                result = client.scroll(
                    collection_name=collection_name,
                    limit=1,
                    with_payload=True
                )

                if result and result[0]:
                    for point in result[0]:
                        # Check if project_path is in payload
                        if point.payload and 'project_path' in point.payload:
                            project_path = Path(point.payload['project_path'])
                            if project_path.exists():
                                return project_path
            except:
                continue

    except Exception as e:
        logger.debug(f"Could not query Qdrant for project path: {e}")

    return None

def get_actual_project_path(conversation_dir_name):
    """Map conversation directory name to actual project path in a cross-platform way.

    Conversation dirs encode the path with dashes:
    - macOS/Linux: -Users-username-projects-projectname or -home-username-projects-projectname
    - Windows: -C-Users-username-projects-projectname

    This function reconstructs the actual path regardless of platform.
    """
    # Extract the normalized project name using shared utility
    project_name = normalize_project_name(conversation_dir_name)

    # Try to reconstruct the original path in a platform-agnostic way
    if conversation_dir_name.startswith("-"):
        path_str = conversation_dir_name[1:]
    else:
        path_str = conversation_dir_name

    # Split into components
    parts = path_str.split("-")

    # Try to reconstruct the path
    if "projects" in parts:
        try:
            projects_idx = parts.index("projects")

            # Reconstruct path based on platform conventions
            if len(parts) > 0:
                # Check for Windows drive letter (single letter component at start)
                if len(parts[0]) == 1 and parts[0].isalpha():
                    # Windows path: C-Users-username-projects-projectname
                    # Becomes: C:/Users/username/projects/projectname
                    drive = parts[0] + ":"
                    path_parts = [drive] + parts[1:projects_idx + 1]
                    base_path = Path(*path_parts)
                else:
                    # Unix-like path: Users-username-projects or home-username-projects
                    # Becomes: /Users/username/projects or /home/username/projects
                    base_path = Path("/") / Path(*parts[:projects_idx + 1])

                actual_path = base_path / project_name
                if actual_path.exists():
                    return actual_path
        except (ValueError, IndexError):
            pass

    # Cross-platform fallback: try common project locations with normalized name
    common_locations = [
        Path.home() / "projects" / project_name,
        Path.home() / "code" / project_name,
        Path.home() / "dev" / project_name,
        Path.home() / "src" / project_name,
        Path.home() / "Documents" / project_name,
        Path.home() / "Desktop" / project_name,
        Path.home() / "workspace" / project_name,
        Path.home() / "repos" / project_name,
    ]

    for loc in common_locations:
        if loc.exists():
            return loc

    # If we still can't find it, return None
    return None

def find_all_projects():
    """Find all Claude projects with conversations."""
    claude_dir = Path.home() / ".claude" / "projects"
    projects = []

    if not claude_dir.exists():
        logger.error(f"Claude projects directory not found: {claude_dir}")
        return projects

    # Find all project directories with JSONL files
    for project_dir in claude_dir.iterdir():
        if project_dir.is_dir():
            jsonl_files = list(project_dir.glob("*.jsonl"))
            if jsonl_files:
                # Extract project name from directory name
                dir_name = project_dir.name

                # Use shared normalization to get project name
                project_name = normalize_project_name(dir_name)

                # Get the actual project path - try Qdrant first, then reconstruction
                actual_path = get_actual_project_path_from_qdrant(project_name)
                if not actual_path:
                    actual_path = get_actual_project_path(dir_name)

                projects.append({
                    "name": project_name,
                    "conversation_path": project_dir,  # Path to conversations
                    "actual_path": actual_path,  # Path to actual code
                    "path": actual_path if actual_path else project_dir,  # Use actual if found
                    "file_count": len(jsonl_files)
                })

    return projects

def update_quality_for_project(project_info):
    """Update quality metrics for a specific project."""
    project_name = project_info["name"]
    project_path = project_info["path"]
    conversation_path = project_info.get("conversation_path", project_path)
    actual_path = project_info.get("actual_path")

    logger.info(f"\n{'='*60}")
    logger.info(f"Processing: {project_name}")
    if actual_path:
        logger.info(f"Actual project: {actual_path}")
    logger.info(f"Conversations: {conversation_path}")
    logger.info(f"Files: {project_info['file_count']} conversations")

    # Detect project type to determine analysis method
    # Use actual_path if available, otherwise use project_path
    path_to_analyze = actual_path if actual_path else project_path
    project_type = detect_project_type(path_to_analyze)
    logger.info(f"Project type: {project_type}")

    # For non-code projects, create a special marker
    if project_type == 'non-code':
        logger.info("📚 Non-code project detected, skipping quality analysis")

        # Create a special cache entry for non-code projects
        cache_dir = Path.home() / ".claude-self-reflect" / "quality_cache"
        cache_dir.mkdir(exist_ok=True, parents=True)

        # Secure sanitization (re is already imported at top)
        import re
        safe_project_name = re.sub(r'[^a-zA-Z0-9_-]', '_', project_name)[:100]
        cache_file = cache_dir / f"{safe_project_name}.json"

        # Save a marker for non-code project
        analysis = {
            'status': 'non-code',
            'project_type': 'documentation',
            'timestamp': datetime.now().isoformat(),
            'message': 'Documentation/data project - quality metrics not applicable'
        }

        with open(cache_file, 'w') as f:
            json.dump(analysis, f, indent=2)

        logger.info("   Marked as documentation project")
        return True

    # For code projects, try to use AST-based quality tracker
    if project_type in ['code', 'mixed']:
        # Use the actual_path we already found, or fall back to reconstructing
        actual_project_path = actual_path if actual_path else None

        # Only try to reconstruct if we don't have actual_path
        if not actual_project_path and '/.claude/projects/' in str(project_path):
            parts = str(project_path).split('/.claude/projects/')
            if len(parts) == 2:
                encoded_name = parts[1]
                if encoded_name.startswith('-'):
                    encoded_name = encoded_name[1:]

                # Reconstruct the actual path
                parts = encoded_name.split('-')
                if len(parts) >= 3 and parts[0] == 'Users':
                    path_parts = []
                    i = 0
                    while i < len(parts):
                        if i <= 2:  # Users, username, projects
                            path_parts.append(parts[i])
                            i += 1
                        else:
                            # Rest is the project name
                            path_parts.append('-'.join(parts[i:]))
                            break
                    actual_project_path = '/' + '/'.join(path_parts)

        # If we found the actual project path and it exists, try AST analysis
        if actual_project_path and Path(actual_project_path).exists():
            try:
                # Run AST analyzer from claude-self-reflect on the target project
                import subprocess
                csr_path = Path(__file__).parent.parent

                # Build command to run AST analyzer on the project
                # Use the venv Python to ensure dependencies are available
                venv_python = csr_path / "venv" / "bin" / "python"
                if not venv_python.exists():
                    # Fallback to system python
                    venv_python = "python"
                else:
                    venv_python = str(venv_python)

                cmd = [
                    venv_python,
                    str(csr_path / "scripts" / "session_quality_tracker.py"),
                    "--project-path", actual_project_path,
                    "--project-name", project_name
                ]

                result = subprocess.run(
                    cmd,
                    capture_output=True,
                    text=True,
                    cwd=str(csr_path),  # Run from claude-self-reflect dir to access modules
                    timeout=30
                )

                if result.returncode == 0:
                    logger.info("✅ Quality updated using AST-based analyzer")

                    # Try to load the cached result to get issue breakdown
                    cache_dir = Path.home() / ".claude-self-reflect" / "quality_cache"
                    import re
                    safe_project_name = re.sub(r'[^a-zA-Z0-9_-]', '_', project_name)[:100]
                    cache_file = cache_dir / f"{safe_project_name}.json"

                    if cache_file.exists():
                        try:
                            with open(cache_file, 'r') as f:
                                cached_data = json.load(f)

                            if cached_data.get('status') == 'success':
                                file_reports = cached_data.get('file_reports', {})
                                critical, medium, low = categorize_issues_from_file_reports(file_reports)
                                quality_display = format_quality_display(critical, medium, low)
                                logger.info(f"   Quality: {quality_display}")

                                # Show breakdown if there are issues
                                if critical + medium + low > 0:
                                    breakdown = []
                                    if critical > 0:
                                        breakdown.append(f"{critical} critical")
                                    if medium > 0:
                                        breakdown.append(f"{medium} medium")
                                    if low > 0:
                                        breakdown.append(f"{low} low")
                                    logger.info(f"   Breakdown: {', '.join(breakdown)}")
                        except:
                            # Fallback to old output parsing
                            for line in result.stdout.split('\n'):
                                if 'Overall Grade:' in line:
                                    logger.info(f"   {line.strip()}")
                                elif 'Total Issues:' in line:
                                    logger.info(f"   {line.strip()}")
                    return True
                else:
                    logger.info(f"AST analyzer returned: {result.stderr[:200]}")
                    logger.info("Falling back to conversation analyzer")
            except Exception as e:
                logger.info(f"Could not run AST analyzer: {e}")

    # Use conversation analyzer for other projects
    analyzer = ConversationQualityAnalyzer()

    try:
        # Analyze recent conversations (up to 5)
        result = analyzer.analyze_project(project_path, limit=5)

        # Create analysis structure matching the expected format
        analysis = {
            'status': 'success',
            'session_id': f"{project_name}_conversations",
            'scope_label': 'Conv',  # Conversation analysis
            'timestamp': datetime.now().isoformat(),
            'summary': {
                'files_analyzed': result.get('files_analyzed', 0),
                'avg_quality_score': result['quality_score'],
                'total_issues': result['total_issues'],
                'total_good_patterns': result['good_patterns_found'],
                'quality_grade': result['quality_grade']
            },
            'file_reports': {},
            'actionable_items': [],
            'quality_trend': 'N/A'
        }

        # For conversation analysis, we'll categorize based on patterns
        # Since we don't have actual code severity, we'll use heuristics
        total_issues = result['total_issues']
        critical = 0  # Conversation analysis doesn't detect critical issues
        medium = min(total_issues, total_issues // 2)  # Half as medium
        low = total_issues - medium  # Rest as low

        quality_display = format_quality_display(critical, medium, low)
        logger.info(f"✅ Quality updated: {quality_display}")
        logger.info(f"   Patterns found - Issues: {result['total_issues']}, Good: {result['good_patterns_found']}")
        logger.info(f"   Analyzed {result.get('files_analyzed', 0)} recent conversations")

        # Save to per-project cache with secure path sanitization
        import re
        # Whitelist approach: only allow alphanumeric, dash, underscore
        safe_project_name = re.sub(r'[^a-zA-Z0-9_-]', '_', project_name)[:100]
        cache_dir = Path.home() / ".claude-self-reflect" / "quality_cache"
        cache_dir.mkdir(exist_ok=True, parents=True)
        cache_file = cache_dir / f"{safe_project_name}.json"

        # Validate the cache file path stays within cache directory
        if not str(cache_file.resolve()).startswith(str(cache_dir.resolve())):
            logger.error(f"Security: Invalid cache path for {project_name}")
            return False

        with open(cache_file, 'w') as f:
            json.dump(analysis, f, indent=2)

        logger.info(f"   Cache saved: {cache_file.name}")
        return True

    except Exception as e:
        logger.error(f"❌ Error analyzing {project_name}: {e}")
        return False

def main():
    """Main entry point."""
    logger.info("🔍 Finding all Claude projects...")

    projects = find_all_projects()

    if not projects:
        logger.error("No Claude projects found with conversations")
        return 1

    logger.info(f"Found {len(projects)} projects with conversations")

    # Update quality for each project
    success_count = 0
    code_projects = 0
    doc_projects = 0
    mixed_projects = 0

    for project in projects:
        # Count project types
        project_type = detect_project_type(project['path'])
        if project_type == 'code':
            code_projects += 1
        elif project_type == 'non-code':
            doc_projects += 1
        else:
            mixed_projects += 1

        if update_quality_for_project(project):
            success_count += 1

    logger.info(f"\n{'='*60}")
    logger.info(f"✅ Updated quality for {success_count}/{len(projects)} projects")
    logger.info(f"   Code projects: {code_projects}")
    logger.info(f"   Documentation projects: {doc_projects}")
    logger.info(f"   Mixed projects: {mixed_projects}")

    # Show current cache status
    cache_dir = Path.home() / ".claude-self-reflect" / "quality_cache"
    if cache_dir.exists():
        cache_files = list(cache_dir.glob("*.json"))
        logger.info(f"📊 Total cache files: {len(cache_files)}")

        for cache_file in cache_files[:5]:  # Show first 5
            try:
                with open(cache_file, 'r') as f:
                    data = json.load(f)
                if data.get('status') == 'success':
                    file_reports = data.get('file_reports', {})
                    critical, medium, low = categorize_issues_from_file_reports(file_reports)
                    quality_display = format_quality_display(critical, medium, low)
                    logger.info(f"   {cache_file.stem}: {quality_display}")
                elif data.get('status') == 'non-code':
                    logger.info(f"   {cache_file.stem}: 📚 Documentation project")
            except:
                pass

    return 0

if __name__ == "__main__":
    sys.exit(main())