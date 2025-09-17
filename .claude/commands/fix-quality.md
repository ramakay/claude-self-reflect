---
allowed-tools: Task
argument-hint: [--file path | --project] [--dry-run] [--severity level]
description: Fix code quality issues with regression testing
---

Please use the `quality-fixer` subagent to fix code quality issues in this project.

Arguments provided: $ARGUMENTS

The quality-fixer subagent will:
1. Run tests to establish a baseline
2. Detect fixable issues using AST-GREP
3. Apply fixes one at a time with regression testing
4. Revert any changes that break tests
5. Provide a detailed report

If no arguments provided, analyze the current directory.
If --dry-run is specified, only show what would be fixed without making changes.
If --file is specified, only fix issues in that specific file.
If --severity is specified, only fix issues of that severity level or higher.

IMPORTANT: The subagent will refuse to proceed if no test suite is found, ensuring safety.