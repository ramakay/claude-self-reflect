#!/usr/bin/env python3
"""
Fix the CSR project detection issue.
"""
import json
from pathlib import Path

def fix_project_detection():
    """Update the cache for CSR to show it as a code project."""

    # Check that CSR has code files
    project_path = Path.cwd()  # Use current working directory

    code_extensions = {'.py', '.js', '.ts', '.jsx', '.tsx', '.java', '.cpp', '.c',
                      '.h', '.hpp', '.rs', '.go', '.rb', '.php', '.swift', '.kt',
                      '.scala', '.r', '.m', '.mm', '.cs', '.vb', '.fs', '.lua'}

    code_files = []
    for file_path in project_path.rglob('*'):
        if file_path.is_file():
            # Skip common non-source directories
            parts = file_path.parts
            if any(skip in parts for skip in ['venv', '.venv', 'node_modules', '.git',
                                               '__pycache__', '.pytest_cache', 'dist',
                                               'build', 'target', '.idea', '.vscode']):
                continue

            if file_path.suffix in code_extensions:
                code_files.append(str(file_path.relative_to(project_path)))
                if len(code_files) >= 5:
                    break

    print(f"Found {len(code_files)} code files in CSR:")
    for f in code_files[:5]:
        print(f"  {f}")

    # Now run the AST analyzer on CSR
    import sys
    sys.path.append(str(project_path / 'scripts'))

    from ast_grep_final_analyzer import FinalASTGrepAnalyzer
    from session_quality_tracker import SessionQualityTracker

    analyzer = FinalASTGrepAnalyzer()
    tracker = SessionQualityTracker()

    # Analyze the project files
    print("\nAnalyzing CSR project...")

    all_file_reports = {}
    total_issues = 0
    total_good_patterns = 0
    total_loc = 0

    # Analyze Python files
    for py_file in project_path.rglob('*.py'):
        # Skip venv and other non-source
        parts = py_file.parts
        if any(skip in parts for skip in ['venv', '.venv', '__pycache__', '.pytest_cache']):
            continue

        rel_path = str(py_file.relative_to(project_path))
        print(f"  Analyzing {rel_path}...")

        file_result = analyzer.analyze_file(str(py_file))

        if file_result and file_result.get('quality_metrics'):
            metrics = file_result['quality_metrics']
            file_report = {
                'quality_score': metrics.get('quality_score', 1.0),
                'good_patterns': metrics.get('total_good_practices', 0),
                'issues': metrics.get('total_issues', 0),
                'recommendations': file_result.get('recommendations', []),
                'top_issues': []
            }

            # Add to reports if there's any activity
            if file_report['issues'] > 0 or file_report['good_patterns'] > 0:
                all_file_reports[str(py_file)] = file_report
                total_issues += file_report['issues']
                total_good_patterns += file_report['good_patterns']

            # Get LOC from the file
            with open(py_file) as f:
                total_loc += len(f.readlines())

    # Calculate overall score
    from ast_grep_unified_registry import UnifiedASTGrepRegistry
    registry = UnifiedASTGrepRegistry()

    all_matches = []
    for report in all_file_reports.values():
        # Create match format for scoring
        for _ in range(report['issues']):
            all_matches.append({'quality': 'bad', 'weight': -2, 'count': 1})
        for _ in range(report['good_patterns']):
            all_matches.append({'quality': 'good', 'weight': 1, 'count': 1})

    overall_score = registry.calculate_quality_score(all_matches, loc=max(1, total_loc))
    quality_grade = tracker._get_quality_grade(overall_score, total_issues)

    result = {
        'status': 'success',
        'session_id': 'fix_csr',
        'scope_label': 'Core',
        'summary': {
            'files_analyzed': len(all_file_reports),
            'quality_score': overall_score,
            'avg_quality_score': overall_score,
            'total_issues': total_issues,
            'total_good_patterns': total_good_patterns,
            'quality_grade': quality_grade
        },
        'file_reports': all_file_reports
    }

    if True:
        print(f"\nQuality Score: {result['summary']['quality_score']:.2f}")
        print(f"Grade: {result['summary']['quality_grade']}")
        print(f"Files Analyzed: {result['summary']['files_analyzed']}")
        print(f"Total Issues: {result['summary']['total_issues']}")
        print(f"Total Good Patterns: {result['summary']['total_good_patterns']}")

        # Save the result to cache
        cache_dir = Path.home() / '.claude-self-reflect' / 'quality_cache'
        cache_dir.mkdir(parents=True, exist_ok=True)

        cache_file = cache_dir / 'claude-self-reflect.json'
        with open(cache_file, 'w') as f:
            json.dump(result, f, indent=2)

        print(f"\n✅ Updated cache at {cache_file}")
    else:
        print(f"Analysis failed: {result.get('message', 'Unknown error')}")

if __name__ == '__main__':
    fix_project_detection()