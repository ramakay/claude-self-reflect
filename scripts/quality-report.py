#!/usr/bin/env python3
"""
Generate detailed code quality report with actionable improvements.
Shows how to get from current grade to A+.
"""

import json
import sys
from pathlib import Path
from datetime import datetime
from typing import Dict, List, Any

def load_quality_data() -> Dict[str, Any]:
    """Load the latest quality analysis data."""
    cache_file = Path.home() / ".claude-self-reflect" / "session_quality.json"

    if not cache_file.exists():
        print("❌ No quality data found. Run session quality tracker first.")
        sys.exit(1)

    with open(cache_file, 'r') as f:
        return json.load(f)

def calculate_improvement_impact(data: Dict[str, Any]) -> Dict[str, int]:
    """Calculate how many issues need to be fixed to reach each grade."""
    current_issues = data['summary']['total_issues']

    thresholds = {
        'A+': 10,   # Less than 10 issues for A+
        'A': 25,    # Less than 25 for A
        'B': 50,    # Less than 50 for B
        'C': 100,   # Less than 100 for C
    }

    improvements = {}
    for grade, threshold in thresholds.items():
        if current_issues > threshold:
            improvements[grade] = current_issues - threshold

    return improvements

def generate_fix_commands(file_reports: Dict) -> List[str]:
    """Generate specific fix commands for common issues."""
    commands = []

    for file_path, report in file_reports.items():
        if report['issues'] > 0:
            file_name = Path(file_path).name

            for issue in report.get('top_issues', []):
                if issue['id'] == 'print-call' and issue['count'] > 0:
                    commands.append(
                        f"# Fix {issue['count']} print statements in {file_name}:\n"
                        f"sed -i '' 's/print(/logger.info(/g' {file_path}"
                    )
                elif issue['id'] == 'sync-open' and issue['count'] > 0:
                    commands.append(
                        f"# Convert {issue['count']} sync file operations in {file_name} to async"
                    )
                elif issue['id'] == 'broad-except' and issue['count'] > 0:
                    commands.append(
                        f"# Fix {issue['count']} bare except clauses in {file_name}"
                    )

    return commands

def generate_report(data: Dict[str, Any]) -> str:
    """Generate comprehensive quality improvement report."""
    summary = data['summary']
    current_grade = summary['quality_grade']
    current_issues = summary['total_issues']

    # Adjust grade based on issues (same logic as statusline)
    if current_issues > 50 and current_grade in ['A+', 'A']:
        current_grade = 'B'
    elif current_issues > 100:
        current_grade = 'C'

    report = []
    report.append("=" * 70)
    report.append("CODE QUALITY IMPROVEMENT REPORT")
    report.append("=" * 70)
    report.append(f"Generated: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    report.append("")

    # Current Status
    report.append("📊 CURRENT STATUS")
    report.append("-" * 40)
    report.append(f"Grade: {current_grade} ({summary['avg_quality_score']:.1%} quality score)")
    report.append(f"Total Issues: {current_issues}")
    report.append(f"Files Analyzed: {summary['files_analyzed']}")
    report.append(f"Good Patterns Found: {summary['total_good_patterns']}")
    report.append("")

    # Path to A+
    improvements = calculate_improvement_impact(data)
    if 'A+' in improvements:
        report.append("🎯 PATH TO A+ GRADE")
        report.append("-" * 40)
        report.append(f"Issues to fix: {improvements['A+']}")
        report.append(f"Target: < 10 issues (currently {current_issues})")
        report.append("")

        if 'A' in improvements:
            report.append(f"Milestone 1 → Grade A: Fix {improvements['A']} issues (get below 25)")
        if 'B' in improvements:
            report.append(f"Milestone 2 → Grade B: Fix {improvements['B']} issues (get below 50)")
        report.append("")
    else:
        report.append("✅ Already at A+ grade! Maintain < 10 issues.")
        report.append("")

    # Top Issues by Impact
    report.append("🔧 TOP ISSUES TO FIX (by file)")
    report.append("-" * 40)

    # Sort files by issue count
    sorted_files = sorted(
        data['file_reports'].items(),
        key=lambda x: x[1]['issues'],
        reverse=True
    )

    for file_path, file_report in sorted_files[:5]:
        if file_report['issues'] > 0:
            file_name = Path(file_path).name
            report.append(f"\n📁 {file_name}: {file_report['issues']} issues")

            for issue in file_report.get('top_issues', [])[:3]:
                severity = "🔴" if issue.get('severity') == 'high' else "🟡"
                report.append(f"   {severity} {issue['description']}: {issue['count']} instances")

    report.append("")

    # Actionable Commands
    report.append("💻 QUICK FIX COMMANDS")
    report.append("-" * 40)

    # Most impactful fixes
    if data.get('actionable_items'):
        for action in data['actionable_items'][:3]:
            report.append(f"• {action}")

    report.append("")

    # Specific fix commands
    fix_commands = generate_fix_commands(data['file_reports'])
    if fix_commands:
        report.append("Run these commands to fix common issues:")
        report.append("")
        for cmd in fix_commands[:3]:
            report.append(cmd)
            report.append("")

    # Progress Tracking
    report.append("📈 PROGRESS TRACKING")
    report.append("-" * 40)
    report.append("After making fixes:")
    report.append("1. Run: python scripts/session_quality_tracker.py")
    report.append("2. Check new grade: csr-status --compact")
    report.append("3. Commit when grade improves")
    report.append("")

    # Best Practices
    report.append("✨ BEST PRACTICES FOR A+ CODE")
    report.append("-" * 40)
    report.append("• Use logging instead of print statements")
    report.append("• Handle specific exceptions, not bare except")
    report.append("• Add type hints to function signatures")
    report.append("• Write docstrings for all functions/classes")
    report.append("• Use async file operations where possible")
    report.append("• Avoid global variables")
    report.append("• Keep functions under 50 lines")
    report.append("• Use list comprehensions over loops where readable")
    report.append("")

    report.append("=" * 70)

    return '\n'.join(report)

def save_report(report: str):
    """Save report to file and display location."""
    report_dir = Path.home() / ".claude-self-reflect" / "quality-reports"
    report_dir.mkdir(exist_ok=True)

    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    report_file = report_dir / f"quality_report_{timestamp}.md"

    with open(report_file, 'w') as f:
        f.write(report)

    return report_file

def main():
    """Generate and display quality improvement report."""
    print("🔍 Analyzing code quality...\n")

    data = load_quality_data()
    report = generate_report(data)

    # Display report
    print(report)

    # Save to file
    report_file = save_report(report)
    print(f"\n📄 Full report saved to: {report_file}")

    # Show how to monitor in real-time
    print("\n🔄 For real-time monitoring:")
    print("   watch -n 60 'csr-status --compact'")
    print("\n📊 To regenerate this report:")
    print("   python scripts/quality-report.py")

if __name__ == "__main__":
    main()