#!/usr/bin/env python3
"""
New quality icon system - replaces letter grades with meaningful icons.
Shows Critical/Medium/Low issue counts instead of alarmist grades.
"""

def get_quality_icon(critical=0, medium=0, low=0):
    """
    Determine quality icon based on issue severity counts.

    Critical: Security issues, sync operations in async, type safety violations
    Medium: Anti-patterns, code smells, complexity issues
    Low: Print statements, console.log, formatting issues
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

def format_statusline(project_name, critical=0, medium=0, low=0):
    """
    Format statusline with colored dot and colored numbers.
    Clean display without extra icons next to numbers.
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

def categorize_issues(file_reports):
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

# Example usage
if __name__ == "__main__":
    # Test different scenarios
    print("Quality Icon System Examples:")
    print("=" * 40)

    # Perfect code
    print(f"Perfect:     {format_statusline('project', 0, 0, 0)}")

    # Only prints (common in test files)
    print(f"Test files:  {format_statusline('tests', 0, 0, 150)}")

    # Mixed issues
    print(f"Typical:     {format_statusline('app', 2, 25, 80)}")

    # Critical issues
    print(f"Needs work:  {format_statusline('legacy', 15, 100, 200)}")

    print("\nReal Project Examples:")
    print("=" * 40)

    # CSR with breakdown
    csr_critical = 0  # No critical issues
    csr_medium = 189   # Anti-patterns
    csr_low = 1907     # Print statements
    print(f"claude-self-reflect: {format_statusline('csr', csr_critical, csr_medium, csr_low)}")

    # Anukruti
    anu_critical = 4   # Sync operations
    anu_medium = 0     # No medium
    anu_low = 300      # Prints and console.log
    print(f"anukruti:            {format_statusline('anu', anu_critical, anu_medium, anu_low)}")

    # Clean project
    print(f"clean-project:       {format_statusline('clean', 0, 3, 10)}")