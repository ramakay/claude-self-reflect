"""
AST-GREP Patterns Identified by CodeRabbit in PR #69 Review
These patterns should be added to our unified_registry.json
"""

SECURITY_PATTERNS = {
    "subprocess-shell-true": {
        "pattern": "subprocess.$METHOD($$$, shell=True, $$$)",
        "description": "Shell injection vulnerability - using shell=True",
        "quality": "bad",
        "weight": -10,
        "severity": "critical",
        "fix": "Use list args without shell=True"
    },
    "subprocess-run-check": {
        "pattern": "subprocess.run($$$)",
        "description": "Potential shell invocation - check for shell=True",
        "quality": "warning",
        "weight": -3,
        "severity": "high"
    },
    "subprocess-popen-check": {
        "pattern": "subprocess.Popen($$$)",
        "description": "Potential shell invocation - check for shell=True",
        "quality": "warning",
        "weight": -3,
        "severity": "high"
    },
    "os-system": {
        "pattern": "os.system($$$)",
        "description": "Direct shell command execution - security risk",
        "quality": "bad",
        "weight": -10,
        "severity": "critical"
    },
    "eval-usage": {
        "pattern": "eval($$$)",
        "description": "Dangerous eval usage - code injection risk",
        "quality": "bad",
        "weight": -10,
        "severity": "critical"
    },
    "exec-usage": {
        "pattern": "exec($$$)",
        "description": "Dangerous exec usage - code injection risk",
        "quality": "bad",
        "weight": -10,
        "severity": "critical"
    }
}

EXCEPTION_HANDLING_PATTERNS = {
    "bare-except": {
        "pattern": "except:\n    $$$",
        "description": "Bare except clause without exception type",
        "quality": "bad",
        "weight": -5,
        "severity": "high",
        "fix": "Use specific exception types"
    },
    "broad-exception": {
        "pattern": "except Exception:\n    $$$",
        "description": "Too broad exception catching",
        "quality": "bad",
        "weight": -3,
        "severity": "medium",
        "fix": "Catch specific exception types"
    },
    "broad-exception-as-e": {
        "pattern": "except Exception as $VAR:\n    $$$",
        "description": "Broad exception with variable",
        "quality": "warning",
        "weight": -2,
        "severity": "medium"
    },
    "multiple-specific-exceptions": {
        "pattern": "except ($EX1, $EX2) as $VAR:\n    $$$",
        "description": "Multiple specific exceptions (good)",
        "quality": "good",
        "weight": 3
    },
    "suppress-exception": {
        "pattern": "except $$$:\n    pass",
        "description": "Silently suppressing exceptions",
        "quality": "bad",
        "weight": -4,
        "severity": "high",
        "fix": "Log exceptions or handle properly"
    }
}

IMPORT_PATTERNS = {
    "missing-import-usage": {
        "pattern": "$MODULE.$METHOD($$$)",
        "description": "Using module without import",
        "quality": "bad",
        "weight": -5,
        "severity": "critical",
        "context": "Check if $MODULE is imported"
    },
    "unused-import": {
        "pattern": "import $MODULE",
        "description": "Import that may be unused",
        "quality": "warning",
        "weight": -1,
        "context": "Check if $MODULE is used in file"
    }
}

TYPE_SAFETY_PATTERNS = {
    "mutable-class-attribute": {
        "pattern": "class $CLASS:\n    $ATTR = []",
        "description": "Mutable class attribute without ClassVar",
        "quality": "bad",
        "weight": -4,
        "severity": "high",
        "fix": "Use ClassVar[List[...]]"
    },
    "mutable-class-dict": {
        "pattern": "class $CLASS:\n    $ATTR = {}",
        "description": "Mutable dict class attribute without ClassVar",
        "quality": "bad",
        "weight": -4,
        "severity": "high",
        "fix": "Use ClassVar[Dict[...]]"
    }
}

STRING_PATTERNS = {
    "fstring-no-placeholder": {
        "pattern": "f\"$STRING\"",
        "description": "f-string without placeholders",
        "quality": "warning",
        "weight": -1,
        "severity": "low",
        "fix": "Remove f prefix",
        "context": "Check if $STRING contains {}"
    },
    "redundant-dict-check": {
        "pattern": "if '$KEY' in $DICT and $DICT['$KEY']:",
        "description": "Redundant dictionary key check",
        "quality": "warning",
        "weight": -2,
        "fix": "Use dict.get('key')"
    }
}

SECURITY_HASHING_PATTERNS = {
    "md5-usage": {
        "pattern": "hashlib.md5($$$)",
        "description": "MD5 is cryptographically broken",
        "quality": "bad",
        "weight": -8,
        "severity": "high",
        "fix": "Use SHA-256 or SHA-3"
    },
    "sha1-usage": {
        "pattern": "hashlib.sha1($$$)",
        "description": "SHA-1 is deprecated for security",
        "quality": "bad",
        "weight": -6,
        "severity": "medium",
        "fix": "Use SHA-256 or SHA-3"
    }
}

PATH_PATTERNS = {
    "hardcoded-tmp": {
        "pattern": "\"/tmp/$$$\"",
        "description": "Hardcoded /tmp path - security risk",
        "quality": "bad",
        "weight": -5,
        "severity": "medium",
        "fix": "Use tempfile module"
    },
    "hardcoded-user-path": {
        "pattern": "\"/Users/$USER/$$$\"",
        "description": "Hardcoded user-specific path",
        "quality": "bad",
        "weight": -3,
        "severity": "low",
        "fix": "Use Path.home() or environment variables"
    },
    "tilde-path": {
        "pattern": "\"~/$$$\"",
        "description": "Using tilde in path",
        "quality": "warning",
        "weight": -2,
        "fix": "Use os.path.expanduser() or Path.home()"
    }
}

UNUSED_CODE_PATTERNS = {
    "unused-variable": {
        "pattern": "$VAR = $VALUE\n$$$",
        "description": "Variable assigned but never used",
        "quality": "warning",
        "weight": -2,
        "severity": "low",
        "context": "Check if $VAR is used after assignment"
    },
    "unused-loop-variable": {
        "pattern": "for $VAR in $ITER:\n    $$$",
        "description": "Loop variable not used in loop body",
        "quality": "warning",
        "weight": -2,
        "fix": "Use _ for unused variables",
        "context": "Check if $VAR is used in loop body"
    }
}

PSUTIL_PATTERNS = {
    "psutil-without-import": {
        "pattern": "psutil.$METHOD($$$)",
        "description": "Using psutil without import",
        "quality": "bad",
        "weight": -5,
        "severity": "critical"
    },
    "psutil-exception-handling": {
        "pattern": "except psutil.$ERROR:",
        "description": "Handling psutil exception without import",
        "quality": "bad",
        "weight": -5,
        "severity": "critical"
    }
}

# Patterns that are GOOD practices we should encourage
GOOD_PATTERNS = {
    "specific-exception-multi": {
        "pattern": "except (IOError, OSError) as e:",
        "description": "Specific exception handling for I/O",
        "quality": "good",
        "weight": 5
    },
    "json-decode-error": {
        "pattern": "except json.JSONDecodeError:",
        "description": "Specific JSON error handling",
        "quality": "good",
        "weight": 4
    },
    "value-error-handling": {
        "pattern": "except (ValueError, TypeError):",
        "description": "Value/Type error handling",
        "quality": "good",
        "weight": 4
    },
    "subprocess-list-args": {
        "pattern": "subprocess.run([$$$], $$$)",
        "description": "Subprocess with list args (safe)",
        "quality": "good",
        "weight": 5
    },
    "pathlib-usage": {
        "pattern": "Path($$$)",
        "description": "Using pathlib for paths",
        "quality": "good",
        "weight": 3
    },
    "context-manager": {
        "pattern": "with $RESOURCE as $VAR:",
        "description": "Using context manager",
        "quality": "good",
        "weight": 4
    },
    "logger-usage": {
        "pattern": "logger.$LEVEL($$$)",
        "description": "Using logger instead of print",
        "quality": "good",
        "weight": 3
    }
}

if __name__ == "__main__":
    import json

    # Combine all patterns
    all_patterns = {
        "security": SECURITY_PATTERNS,
        "exception_handling": EXCEPTION_HANDLING_PATTERNS,
        "imports": IMPORT_PATTERNS,
        "type_safety": TYPE_SAFETY_PATTERNS,
        "strings": STRING_PATTERNS,
        "hashing": SECURITY_HASHING_PATTERNS,
        "paths": PATH_PATTERNS,
        "unused_code": UNUSED_CODE_PATTERNS,
        "psutil": PSUTIL_PATTERNS,
        "good_practices": GOOD_PATTERNS
    }

    print("CodeRabbit Identified Patterns Summary:")
    print("=" * 50)

    total = 0
    for category, patterns in all_patterns.items():
        count = len(patterns)
        total += count
        print(f"{category:20s}: {count:3d} patterns")

    print("-" * 50)
    print(f"{'TOTAL':20s}: {total:3d} patterns")

    # Export for integration
    with open("coderabbit_patterns.json", "w") as f:
        json.dump(all_patterns, f, indent=2)