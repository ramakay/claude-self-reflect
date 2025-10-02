# AST-GREP Pattern Analysis Report

**File**: scripts/metadata_extractor.py
**Language**: python
**Timestamp**: 2025-10-01T21:48:03.272399
**Engine**: ast-grep-py + unified registry

## Quality Overview
- **Quality Score**: 🟡 60.8%
- **Good Practices**: 2187
- **Issues Found**: 4
- **Unique Patterns Matched**: 8

## Recommendations
- 🟢 Good: Code follows most best practices
- Fix 4 anti-patterns in python_antipatterns

## Pattern Matches by Category

### python_logging (1 patterns, 10 matches)
- ✅ **logger-call**: 10 instances
  - Logger usage
  - Example (line 65): `logger.warning(f"Error reading file {file_path}: {...`

### python_typing (1 patterns, 11 matches)
- ✅ **typed-function**: 11 instances
  - Function with return type
  - Example (line 32): `def extract_metadata_from_file(self, file_path: st...`

### python_antipatterns (2 patterns, 4 matches)
- ❌ **thread-join-async**: 3 instances
  - Thread join blocking async context
  - Example (line 167): `"\n".join(text_parts)...`
- ❌ **sync-open**: 1 instances
  - Sync file open (should use aiofiles)
  - Example (line 43): `open(file_path, 'r', encoding='utf-8')...`

### python_runtime_modification (1 patterns, 1 matches)
- ⚪ **singleton-state-change**: 1 instances
  - Runtime singleton state modification
  - Example (line 30): `self.processor_factory = MessageProcessorFactory()...`

### python_catalog (3 patterns, 2166 matches)
- ✅ **prefer-generator-expressions**: 2138 instances
  - List comprehensions like `[x for x in range(10)]` are a concise way to create lists in Python. However, we can achieve better memory efficiency by using generator expressions like `(x for x in range(10))` instead. List comprehensions create the entire list in memory, while generator expressions generate each element one at a time. We can make the change by replacing the square brackets with parentheses.
  - Example (line 1): `"""
Metadata extractor using message processors to...`
- ✅ **use-walrus-operator**: 24 instances
  - The walrus operator (`:=`) introduced in Python 3.8 allows you to assign values to variables as part of an expression. This rule aims to simplify code by using the walrus operator in `if` statements.

This first part of the rule identifies cases where a variable is assigned a value and then immediately used in an `if` statement to control flow.
  - Example (line 45): `if not line.strip():
                        conti...`
- ✅ **optional-to-none-union**: 4 instances
  - [PEP 604](https://peps.python.org/pep-0604/) recommends that `Type | None` is preferred over `Optional[Type]` for Python 3.10+.

This rule performs such rewriting. Note `Optional[$T]` alone is interpreted as subscripting expression instead of generic type, we need to use [pattern object](/guide/rule-config/atomic-rule.html#pattern-object) to disambiguate it with more context code.

<!-- Use YAML in the example. Delete this section if use pattern. -->
  - Example (line 103): `Optional[Tuple[str, bool]]...`

## Pattern Registry Statistics
- **Patterns Available**: 39
- **Patterns Matched**: 8
- **Categories Found**: python_logging, python_typing, python_antipatterns, python_runtime_modification, python_catalog

## Compliance
✅ Using unified AST-GREP registry (custom + catalog)
✅ Using ast-grep-py for AST matching
✅ NO regex patterns or fallbacks
✅ Production-ready pattern analysis