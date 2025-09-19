#!/usr/bin/env python3
"""
Session Quality Tracker
Analyzes code quality for files edited in the current Claude session.
MANDATORY AST-GREP analysis for real-time quality feedback.
"""

import json
import os
from pathlib import Path
from datetime import datetime, timedelta
from typing import Dict, List, Any, Optional, Set
import logging
import sys

# Setup logger
logger = logging.getLogger(__name__)
logging.basicConfig(level=logging.INFO, format='%(message)s')

# Add scripts directory to path
sys.path.append(str(Path(__file__).parent))

from ast_grep_final_analyzer import FinalASTGrepAnalyzer
from update_patterns import check_and_update_patterns

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class SessionQualityTracker:
    """
    Tracks code quality for the current Claude session.
    Provides real-time feedback on code quality issues.
    """

    def __init__(self):
        self.analyzer = FinalASTGrepAnalyzer()
        self.session_files = set()
        self.quality_history = {}
        self.current_session_id = None

    def find_active_session(self, project_path: Optional[str] = None) -> Optional[Path]:
        """Find the most recently modified JSONL file (active session)."""
        claude_projects = Path.home() / ".claude" / "projects"

        if project_path:
            # Look for specific project
            project_dir = claude_projects / project_path.replace('/', '-')
        else:
            # Look for claude-self-reflect project
            project_dir = claude_projects / "-Users-ramakrishnanannaswamy-projects-claude-self-reflect"

        if not project_dir.exists():
            logger.warning(f"Project directory not found: {project_dir}")
            return None

        # Find most recent JSONL file (modified in last 2 hours)
        recent_files = []
        cutoff_time = datetime.now() - timedelta(hours=2)

        for jsonl_file in project_dir.glob("*.jsonl"):
            if jsonl_file.stat().st_mtime > cutoff_time.timestamp():
                recent_files.append((jsonl_file, jsonl_file.stat().st_mtime))

        if not recent_files:
            return None

        # Return most recent file
        recent_files.sort(key=lambda x: x[1], reverse=True)
        return recent_files[0][0]

    def extract_edited_files(self, session_file: Path) -> Set[str]:
        """Extract files edited in the current session."""
        edited_files = set()

        try:
            with open(session_file, 'r') as f:
                for line in f:
                    try:
                        entry = json.loads(line.strip())

                        # Look for messages with tool usage
                        if 'messages' in entry:
                            for msg in entry['messages']:
                                if msg.get('role') == 'assistant' and 'content' in msg:
                                    content = msg['content']

                                    # Handle different content formats
                                    if isinstance(content, list):
                                        for item in content:
                                            if isinstance(item, dict) and item.get('type') == 'tool_use':
                                                tool_name = item.get('name', '')
                                                if tool_name in ['Edit', 'Write', 'MultiEdit', 'NotebookEdit']:
                                                    input_data = item.get('input', {})
                                                    if 'file_path' in input_data:
                                                        file_path = input_data['file_path']
                                                        if file_path and os.path.exists(file_path):
                                                            edited_files.add(file_path)

                    except json.JSONDecodeError:
                        continue
                    except Exception as e:
                        logger.debug(f"Error processing line: {e}")

        except Exception as e:
            logger.error(f"Error reading session file: {e}")

        return edited_files

    def analyze_session_quality(self, session_file: Optional[Path] = None, use_tracker: bool = False) -> Dict[str, Any]:
        """
        Analyze code quality for all files edited in current session.
        Returns quality report with actionable insights.
        """
        # Update patterns (uses cache, <100ms)
        check_and_update_patterns()

        # Check for session edit tracker first (priority mode)
        if use_tracker or (not session_file):
            tracker_file = Path.home() / ".claude-self-reflect" / "current_session_edits.json"
            if tracker_file.exists():
                try:
                    with open(tracker_file, 'r') as f:
                        tracker_data = json.load(f)
                        edited_files = set(tracker_data.get('edited_files', []))
                        if edited_files:
                            logger.info(f"Using session tracker: {len(edited_files)} files edited in session")
                            self.current_session_id = 'active_session'
                            # Use Session scope label for tracked edits
                            return self._analyze_files_with_scope(edited_files, scope_label='Session')
                except Exception as e:
                    logger.debug(f"Error reading tracker file: {e}")

        # Find active session if not provided
        if not session_file:
            session_file = self.find_active_session()
            if not session_file:
                # Fallback: analyze recently modified files in project
                return self.analyze_recent_files()

        self.current_session_id = session_file.stem

        # Extract edited files
        edited_files = self.extract_edited_files(session_file)

        if not edited_files:
            # Fallback to analyzing recent files when no edits in session
            logger.info("No files edited in current session, analyzing recent project files...")
            return self.analyze_recent_files()

        # Analyze each edited file
        file_reports = {}
        total_issues = 0
        total_good_patterns = 0
        quality_scores = []

        for file_path in edited_files:
            # Only analyze code files
            if any(file_path.endswith(ext) for ext in ['.py', '.ts', '.js', '.tsx', '.jsx']):
                try:
                    result = self.analyzer.analyze_file(file_path)
                    metrics = result['quality_metrics']

                    file_reports[file_path] = {
                        'quality_score': metrics['quality_score'],
                        'good_patterns': metrics['good_patterns_found'],
                        'issues': metrics['total_issues'],
                        'recommendations': result.get('recommendations', [])[:3],  # Top 3
                        'top_issues': self._get_top_issues(result)
                    }

                    total_issues += metrics['total_issues']
                    total_good_patterns += metrics['good_patterns_found']
                    quality_scores.append(metrics['quality_score'])

                    # Track quality history
                    if file_path not in self.quality_history:
                        self.quality_history[file_path] = []
                    self.quality_history[file_path].append({
                        'timestamp': datetime.now().isoformat(),
                        'score': metrics['quality_score']
                    })

                except Exception as e:
                    logger.error(f"Failed to analyze {file_path}: {e}")

        if not file_reports:
            return {
                'status': 'no_code_files',
                'session_id': self.current_session_id,
                'message': 'No analyzable code files in session'
            }

        # Calculate session average
        avg_quality = sum(quality_scores) / len(quality_scores) if quality_scores else 0

        # Generate session report
        return {
            'status': 'success',
            'session_id': self.current_session_id,
            'scope_label': 'Sess',  # Session scope - files edited in current session
            'timestamp': datetime.now().isoformat(),
            'summary': {
                'files_analyzed': len(file_reports),
                'avg_quality_score': round(avg_quality, 3),
                'total_issues': total_issues,
                'total_good_patterns': total_good_patterns,
                'quality_grade': self._get_quality_grade(avg_quality, total_issues)
            },
            'file_reports': file_reports,
            'actionable_items': self._generate_actionable_items(file_reports),
            'quality_trend': self._calculate_quality_trend()
        }

    def _get_top_issues(self, analysis_result: Dict) -> List[Dict]:
        """Extract top issues from analysis result."""
        top_issues = []

        for match in analysis_result.get('all_matches', []):
            if match['quality'] == 'bad' and match['count'] > 0:
                top_issues.append({
                    'id': match['id'],
                    'description': match['description'],
                    'count': match['count'],
                    'severity': 'high' if match['weight'] <= -3 else 'medium'
                })

        # Sort by count * weight (impact)
        top_issues.sort(key=lambda x: x['count'], reverse=True)
        return top_issues[:5]  # Top 5 issues

    def _get_quality_grade(self, score: float, total_issues: int = 0) -> str:
        """
        Convert quality score to letter grade.
        Based on consensus: issues should dominate grading.

        Grade boundaries (adjusted for issue count):
        - A+: score >= 0.97 AND issues <= 5
        - A:  score >= 0.93 AND issues <= 20
        - B:  score >= 0.83 AND issues <= 50
        - C:  score >= 0.73 AND issues <= 100
        - D:  score >= 0.60
        - F:  score < 0.60
        """
        # Hard caps based on issue count (industry standard)
        if total_issues > 200:
            return 'F'
        elif total_issues > 100:
            # Many issues - max grade is C
            if score >= 0.77:
                return 'C+'
            elif score >= 0.73:
                return 'C'
            elif score >= 0.70:
                return 'C-'
            elif score >= 0.60:
                return 'D'
            else:
                return 'F'
        elif total_issues > 50:
            # Moderate issues - max grade is B
            if score >= 0.87:
                return 'B+'
            elif score >= 0.83:
                return 'B'
            elif score >= 0.80:
                return 'B-'
            elif score >= 0.73:
                return 'C'
            elif score >= 0.60:
                return 'D'
            else:
                return 'F'
        elif total_issues > 20:
            # Some issues - max grade is A-
            if score >= 0.90:
                return 'A-'
            elif score >= 0.87:
                return 'B+'
            elif score >= 0.83:
                return 'B'
            elif score >= 0.73:
                return 'C'
            elif score >= 0.60:
                return 'D'
            else:
                return 'F'
        elif total_issues > 5:
            # Few issues - max grade is A
            if score >= 0.93:
                return 'A'
            elif score >= 0.90:
                return 'A-'
            elif score >= 0.83:
                return 'B'
            elif score >= 0.73:
                return 'C'
            elif score >= 0.60:
                return 'D'
            else:
                return 'F'
        else:
            # Very few issues (0-5) - can achieve A+
            if score >= 0.97:
                return 'A+'
            elif score >= 0.93:
                return 'A'
            elif score >= 0.90:
                return 'A-'
            elif score >= 0.83:
                return 'B'
            elif score >= 0.73:
                return 'C'
            elif score >= 0.60:
                return 'D'
            else:
                return 'F'

    def _generate_actionable_items(self, file_reports: Dict) -> List[str]:
        """Generate actionable recommendations for the user."""
        actions = []

        # Collect all issues
        all_issues = {}
        for file_path, report in file_reports.items():
            for issue in report.get('top_issues', []):
                issue_id = issue['id']
                if issue_id not in all_issues:
                    all_issues[issue_id] = {
                        'description': issue['description'],
                        'total_count': 0,
                        'files': []
                    }
                all_issues[issue_id]['total_count'] += issue['count']
                all_issues[issue_id]['files'].append(Path(file_path).name)

        # Sort by total impact
        sorted_issues = sorted(all_issues.items(), key=lambda x: x[1]['total_count'], reverse=True)

        # Generate actions
        for issue_id, issue_data in sorted_issues[:3]:  # Top 3 issues
            if issue_id == 'print-call':
                actions.append(f"🔧 Replace {issue_data['total_count']} print statements with logger in {', '.join(issue_data['files'][:2])}")
            elif issue_id == 'sync-open':
                actions.append(f"⚡ Use async file operations ({issue_data['total_count']} sync opens found)")
            elif issue_id == 'broad-except':
                actions.append(f"🎯 Fix {issue_data['total_count']} bare except clauses for better error handling")
            elif issue_id == 'no-console-log':
                actions.append(f"🚫 Remove {issue_data['total_count']} console.log statements")
            else:
                actions.append(f"📝 Fix {issue_data['total_count']} instances of {issue_data['description']}")

        return actions

    def _analyze_files_with_scope(self, edited_files: set, scope_label: str = 'Session') -> Dict[str, Any]:
        """
        Analyze specific files with a given scope label.
        Used for both session tracking and fallback modes.
        """
        # Analyze each edited file
        file_reports = {}
        total_issues = 0
        total_good_patterns = 0
        quality_scores = []

        for file_path in edited_files:
            # Only analyze code files
            if any(str(file_path).endswith(ext) for ext in ['.py', '.ts', '.js', '.tsx', '.jsx']):
                try:
                    result = self.analyzer.analyze_file(file_path)
                    metrics = result['quality_metrics']

                    file_reports[file_path] = {
                        'quality_score': metrics['quality_score'],
                        'good_patterns': metrics['good_patterns_found'],
                        'issues': metrics['total_issues'],
                        'recommendations': result.get('recommendations', [])[:3],  # Top 3
                        'top_issues': self._get_top_issues(result)
                    }

                    total_issues += metrics['total_issues']
                    total_good_patterns += metrics['good_patterns_found']
                    quality_scores.append(metrics['quality_score'])

                    # Track quality history
                    if file_path not in self.quality_history:
                        self.quality_history[file_path] = []
                    self.quality_history[file_path].append({
                        'timestamp': datetime.now().isoformat(),
                        'score': metrics['quality_score']
                    })

                except Exception as e:
                    logger.error(f"Failed to analyze {file_path}: {e}")

        if not file_reports:
            return {
                'status': 'no_code_files',
                'session_id': self.current_session_id,
                'scope_label': scope_label,
                'message': 'No analyzable code files in session'
            }

        # Calculate session average
        avg_quality = sum(quality_scores) / len(quality_scores) if quality_scores else 0

        # Generate session report
        return {
            'status': 'success',
            'session_id': self.current_session_id,
            'scope_label': scope_label,  # Use provided scope label
            'timestamp': datetime.now().isoformat(),
            'summary': {
                'files_analyzed': len(file_reports),
                'avg_quality_score': round(avg_quality, 3),
                'total_issues': total_issues,
                'total_good_patterns': total_good_patterns,
                'quality_grade': self._get_quality_grade(avg_quality, total_issues)
            },
            'file_reports': file_reports,
            'actionable_items': self._generate_actionable_items(file_reports),
            'quality_trend': self._calculate_quality_trend()
        }

    def analyze_recent_files(self) -> Dict[str, Any]:
        """Analyze core project files when no session is found."""
        # Use current working directory as project root
        project_root = Path.cwd()

        # Find code files in the project dynamically
        code_extensions = {'.py', '.js', '.ts', '.jsx', '.tsx', '.java', '.cpp', '.c',
                          '.h', '.hpp', '.rs', '.go', '.rb', '.php'}

        core_files = []
        # Look for code files in the project (limit to avoid too many files)
        for ext in code_extensions:
            files = list(project_root.rglob(f'*{ext}'))
            # Filter out common non-source directories
            files = [f for f in files if not any(
                skip in f.parts for skip in ['venv', '.venv', 'node_modules', '.git',
                                            '__pycache__', '.pytest_cache', 'dist',
                                            'build', 'target', '.idea', '.vscode']
            )]
            core_files.extend(files[:20])  # Take up to 20 files per extension
            if len(core_files) >= 50:  # Increased limit to 50 files for better coverage
                break

        # Convert to relative paths
        core_files = [str(f.relative_to(project_root)) for f in core_files[:50]]

        edited_files = set()
        for file_path in core_files:
            full_path = project_root / file_path
            if full_path.exists():
                edited_files.add(str(full_path))

        # Also check for recently modified files (last 30 minutes) to catch actual work
        try:
            # No need to validate project_root - we can analyze any project

            # Use pathlib instead of subprocess for safer file discovery
            scripts_dir = project_root / "scripts"
            if scripts_dir.exists():
                from datetime import datetime, timedelta
                cutoff_time = datetime.now() - timedelta(minutes=30)
                for py_file in scripts_dir.glob("*.py"):
                    if py_file.stat().st_mtime > cutoff_time.timestamp():
                        # Skip test files and temporary files
                        if "test_" not in py_file.name and "verify_" not in py_file.name:
                            edited_files.add(str(py_file))
        except Exception as e:
            logger.debug(f"Error checking recent files: {e}")

        if not edited_files:
            return {
                'status': 'no_edits',
                'session_id': 'recent_files',
                'message': 'No recently modified code files'
            }

        # Analyze the files
        self.current_session_id = "recent_files"
        file_reports = {}
        total_issues = 0
        total_good_patterns = 0
        quality_scores = []

        for file_path in list(edited_files)[:50]:  # Analyze up to 50 files for better coverage
            try:
                result = self.analyzer.analyze_file(file_path)
                metrics = result['quality_metrics']

                file_reports[file_path] = {
                    'quality_score': metrics['quality_score'],
                    'good_patterns': metrics['good_patterns_found'],
                    'issues': metrics['total_issues'],
                    'recommendations': result.get('recommendations', [])[:3],
                    'top_issues': self._get_top_issues(result)
                }

                total_issues += metrics['total_issues']
                total_good_patterns += metrics['good_patterns_found']
                quality_scores.append(metrics['quality_score'])

            except Exception as e:
                logger.error(f"Failed to analyze {file_path}: {e}")

        if not file_reports:
            return {
                'status': 'no_code_files',
                'session_id': 'recent_files',
                'message': 'No analyzable code files'
            }

        avg_quality = sum(quality_scores) / len(quality_scores) if quality_scores else 0

        # Determine scope label based on what files we're analyzing
        scope_label = 'Core'  # Default to core project files
        if any('session_quality_tracker' in str(f) for f in edited_files):
            scope_label = 'Fix'  # Files we just fixed
        elif any(Path(f).stat().st_mtime > (datetime.now().timestamp() - 1800) for f in edited_files if Path(f).exists()):
            scope_label = 'Recent'  # Recently modified

        return {
            'status': 'success',
            'session_id': 'recent_files',
            'scope_label': scope_label,
            'timestamp': datetime.now().isoformat(),
            'summary': {
                'files_analyzed': len(file_reports),
                'avg_quality_score': round(avg_quality, 3),
                'total_issues': total_issues,
                'total_good_patterns': total_good_patterns,
                'quality_grade': self._get_quality_grade(avg_quality, total_issues)
            },
            'file_reports': file_reports,
            'actionable_items': self._generate_actionable_items(file_reports),
            'quality_trend': self._calculate_quality_trend()
        }

    def _calculate_quality_trend(self) -> str:
        """Calculate quality trend across session."""
        if not self.quality_history:
            return "No trend data"

        # Look at average change
        improvements = 0
        degradations = 0

        for file_path, history in self.quality_history.items():
            if len(history) >= 2:
                delta = history[-1]['score'] - history[0]['score']
                if delta > 0.05:
                    improvements += 1
                elif delta < -0.05:
                    degradations += 1

        if improvements > degradations:
            return f"📈 Improving ({improvements} files better)"
        elif degradations > improvements:
            return f"📉 Degrading ({degradations} files worse)"
        else:
            return "➡️ Stable"

    def generate_report(self, analysis: Dict) -> str:
        """Generate human-readable report."""
        if analysis['status'] != 'success':
            return f"❌ {analysis['message']}"

        summary = analysis['summary']
        grade = summary['quality_grade']
        emoji = '🟢' if grade in ['A+', 'A'] else '🟡' if grade in ['B', 'C'] else '🔴'

        report = []
        report.append("=" * 60)
        report.append("SESSION CODE QUALITY REPORT")
        report.append("=" * 60)
        report.append(f"Session: {analysis['session_id'][:8]}...")
        report.append(f"Time: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
        report.append("")
        report.append(f"{emoji} Overall Grade: {grade} ({summary['avg_quality_score']:.1%})")
        report.append(f"Files Analyzed: {summary['files_analyzed']}")
        report.append(f"Total Issues: {summary['total_issues']}")
        report.append(f"Good Patterns: {summary['total_good_patterns']}")
        report.append(f"Trend: {analysis['quality_trend']}")
        report.append("")

        if analysis['actionable_items']:
            report.append("ACTIONS NEEDED:")
            for action in analysis['actionable_items']:
                report.append(f"  {action}")
            report.append("")

        report.append("FILE DETAILS:")
        for file_path, file_report in analysis['file_reports'].items():
            file_name = Path(file_path).name
            score = file_report['quality_score']
            emoji = '✅' if score > 0.7 else '⚠️' if score > 0.5 else '❌'
            report.append(f"  {emoji} {file_name}: {score:.1%} ({file_report['issues']} issues)")

        report.append("")
        report.append("💡 Ask Claude: 'Please fix the code quality issues in this session'")
        report.append("=" * 60)

        return '\n'.join(report)


def main(use_tracker=False):
    """Run session quality analysis."""
    tracker = SessionQualityTracker()

    logger.info("🔍 Analyzing current session code quality...")
    logger.info("")

    analysis = tracker.analyze_session_quality(use_tracker=use_tracker)
    report = tracker.generate_report(analysis)

    logger.info(report)

    # Save report for watcher integration - PER PROJECT
    # Always save cache, even with fallback analysis
    if analysis.get('status') in ['success', 'fallback']:
        # Get project name from environment or current directory
        project_name = os.environ.get('QUALITY_PROJECT_NAME', os.path.basename(os.getcwd()))
        # Secure sanitization with whitelist approach
        import re
        safe_project_name = re.sub(r'[^a-zA-Z0-9_-]', '_', project_name)[:100]

        # Save to per-project cache directory
        cache_dir = Path.home() / ".claude-self-reflect" / "quality_cache"
        cache_dir.mkdir(exist_ok=True, parents=True)
        report_path = cache_dir / f"{safe_project_name}.json"

        # Validate the report path stays within cache directory
        if not str(report_path.resolve()).startswith(str(cache_dir.resolve())):
            logger.error(f"Security: Invalid cache path for {project_name}")
            return

        with open(report_path, 'w') as f:
            json.dump(analysis, f, indent=2)
        logger.info(f"\n📊 Full report saved to: {report_path}")


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description='Analyze code quality for projects')
    parser.add_argument('--project-path', help='Path to the project to analyze')
    parser.add_argument('--project-name', help='Name of the project for cache file')
    parser.add_argument('--use-tracker', action='store_true',
                        help='Use session edit tracker for analysis')
    parser.add_argument('--update-cache-only', action='store_true',
                        help='Only update cache without printing report')
    args = parser.parse_args()

    # If external project specified, change to that directory
    if args.project_path:
        os.chdir(args.project_path)

    # Override project name if specified
    if args.project_name:
        # This will be used in the main() function for cache naming
        os.environ['QUALITY_PROJECT_NAME'] = args.project_name

    # For cache-only mode, suppress output
    if args.update_cache_only:
        # Redirect logger to null
        import os
        import sys
        sys.stdout = open(os.devnull, 'w')
        sys.stderr = open(os.devnull, 'w')

    main(use_tracker=args.use_tracker)