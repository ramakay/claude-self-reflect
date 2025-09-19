# AST-GREP Pattern Analysis Report

**File**: /Users/ramakrishnanannaswamy/projects/claude-self-reflect/terrible_quality.py
**Language**: python
**Timestamp**: 2025-09-19T09:44:52.770968
**Engine**: ast-grep-py + unified registry

## Quality Overview
- **Quality Score**: 🟡 50.8%
- **Good Practices**: 261
- **Issues Found**: 23
- **Unique Patterns Matched**: 5

## Recommendations
- 🟡 Warning: Several anti-patterns detected
- Replace 18 print statements with logger

## Pattern Matches by Category

### python_logging (1 patterns, 18 matches)
- ❌ **print-call**: 18 instances
  - Print statement
  - Example (line 5): `print("Bad 1")...`

### python_catalog (2 patterns, 261 matches)
- ✅ **prefer-generator-expressions**: 255 instances
  - List comprehensions like `[x for x in range(10)]` are a concise way to create lists in Python. However, we can achieve better memory efficiency by using generator expressions like `(x for x in range(10))` instead. List comprehensions create the entire list in memory, while generator expressions generate each element one at a time. We can make the change by replacing the square brackets with parentheses.
  - Example (line 1): `#!/usr/bin/env python3
"""File with terrible quali...`
- ✅ **use-walrus-operator**: 6 instances
  - The walrus operator (`:=`) introduced in Python 3.8 allows you to assign values to variables as part of an expression. This rule aims to simplify code by using the walrus operator in `if` statements.

This first part of the rule identifies cases where a variable is assigned a value and then immediately used in an `if` statement to control flow.
  - Example (line 17): `if True:
    if True:
        if True:
           ...`

### python_complexity (2 patterns, 5 matches)
- ❌ **nested-if-depth-3**: 3 instances
  - Deeply nested if statements (3+ levels)
  - Example (line 17): `if True:
    if True:
        if True:
           ...`
- ❌ **nested-loops**: 2 instances
  - Nested loops (performance risk)
  - Example (line 29): `for i in range(10):
    for j in range(10):
      ...`

## Pattern Registry Statistics
- **Patterns Available**: 44
- **Patterns Matched**: 5
- **Categories Found**: python_logging, python_catalog, python_complexity

## Compliance
✅ Using unified AST-GREP registry (custom + catalog)
✅ Using ast-grep-py for AST matching
✅ NO regex patterns or fallbacks
✅ Production-ready pattern analysis