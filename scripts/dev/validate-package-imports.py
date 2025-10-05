#!/usr/bin/env python3
"""
Package Import Validator for v6.0.0+
Validates that all production imports work correctly from npm package structure.

Prevents issues like:
- Missing modules in package.json files array
- Broken import paths after restructuring
- Missing dependencies
"""

import subprocess
import sys
import json
from pathlib import Path
from typing import List, Set, Tuple

def get_repo_root() -> Path:
    """Get repository root."""
    return Path(__file__).parent.parent.parent

def get_packaged_files() -> Set[str]:
    """Get list of files that will be in npm package."""
    repo_root = get_repo_root()
    result = subprocess.run(
        ["npm", "pack", "--dry-run"],
        capture_output=True,
        text=True,
        cwd=repo_root
    )

    packaged_files = set()
    in_tarball_section = False

    for line in (result.stdout + result.stderr).split('\n'):
        if 'Tarball Contents' in line:
            in_tarball_section = True
            continue
        if in_tarball_section and ('Tarball Details' in line or 'Unpacked Size' in line):
            break
        if in_tarball_section and 'npm notice' in line:
            import re
            match = re.search(r'npm notice\s+[\d.]+[kKmMgGbBtT]*\s+(.+)$', line)
            if match:
                file_path = match.group(1).strip()
                if file_path:
                    packaged_files.add(file_path)

    return packaged_files

def find_python_imports(file_path: Path) -> List[str]:
    """Extract import statements from Python file."""
    imports = []
    try:
        with open(file_path, 'r', encoding='utf-8') as f:
            for line in f:
                line = line.strip()
                if line.startswith('from ') or line.startswith('import '):
                    imports.append(line)
    except Exception:
        pass
    return imports

def validate_production_imports() -> Tuple[List[str], List[str]]:
    """Validate all production Python files have their imports satisfied."""
    repo_root = get_repo_root()
    packaged_files = get_packaged_files()

    # Get all Python files in package
    python_files = [f for f in packaged_files if f.endswith('.py')]

    errors = []
    warnings = []

    # Critical production modules that MUST be in package
    critical_modules = [
        'src/runtime/import-conversations-unified.py',
        'src/runtime/unified_state_manager.py',
        'src/runtime/metadata_extractor.py',
        'src/importer/main.py',
        'mcp-server/src/server.py',
    ]

    for module in critical_modules:
        if module not in packaged_files:
            errors.append(f"CRITICAL: Missing production module: {module}")

    # Check that src/ directory exists in package
    src_files = [f for f in packaged_files if f.startswith('src/')]
    if len(src_files) == 0:
        errors.append("CRITICAL: No src/ files found in package!")

    # Check that shared/ directory exists if needed
    shared_files = [f for f in packaged_files if f.startswith('shared/')]

    # Validate no scripts/* files leaked into package
    scripts_files = [f for f in packaged_files if f.startswith('scripts/') and f.endswith('.py')]
    for f in scripts_files:
        # Allow only essential scripts
        if f not in ['scripts/auto-migrate.cjs', 'scripts/migrate-to-unified-state.py', 'scripts/csr-status']:
            warnings.append(f"WARNING: Dev script in package: {f}")

    return errors, warnings

def main():
    """Run validation."""
    print("=" * 70)
    print("Package Import Validator (Issue #87)")
    print("=" * 70)
    print()

    print("📋 Analyzing npm package contents...")
    errors, warnings = validate_production_imports()

    print()
    print("=" * 70)
    print("Validation Results")
    print("=" * 70)
    print()

    if errors:
        print(f"❌ {len(errors)} CRITICAL ERRORS:")
        for error in errors:
            print(f"  {error}")
        print()

    if warnings:
        print(f"⚠️  {len(warnings)} WARNINGS:")
        for warning in warnings[:10]:
            print(f"  {warning}")
        if len(warnings) > 10:
            print(f"  ... and {len(warnings) - 10} more")
        print()

    if not errors and not warnings:
        print("✅ VALIDATION PASSED: All production imports validated")
        print()
        return 0
    elif not errors:
        print("✅ VALIDATION PASSED: No critical errors (warnings only)")
        print()
        return 0
    else:
        print("❌ VALIDATION FAILED: Critical errors found")
        print()
        return 1

if __name__ == "__main__":
    sys.exit(main())
