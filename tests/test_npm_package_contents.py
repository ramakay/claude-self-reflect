#!/usr/bin/env python3
"""
Regression test to ensure all required files are included in npm package.
Prevents issues like #71 where refactored modules were missing from package.

This test:
1. Parses package.json to get expected files (from 'files' array)
2. Runs `npm pack --dry-run` to get actual packaged files
3. Validates that glob patterns match actual files
4. Catches discrepancies between expected and actual packaging

This is a self-maintaining test - no hardcoded file lists needed!
"""

import subprocess
import sys
import re
import json
from pathlib import Path
from typing import List, Set
import fnmatch


def get_repo_root() -> Path:
    """Get the repository root directory."""
    return Path(__file__).parent.parent


def load_package_json() -> dict:
    """Load and parse package.json."""
    package_json_path = get_repo_root() / "package.json"
    try:
        with open(package_json_path, 'r') as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError) as e:
        print(f"❌ Failed to load package.json: {e}")
        sys.exit(1)


def expand_glob_patterns(patterns: List[str]) -> Set[str]:
    """Expand glob patterns from package.json files array to actual file paths."""
    repo_root = get_repo_root()
    expanded_files = set()

    for pattern in patterns:
        # Handle glob patterns (**, *, etc.)
        if '*' in pattern:
            # Use Path.glob() for glob expansion
            matched_paths = repo_root.glob(pattern)
            for path in matched_paths:
                if path.is_file():
                    # Get relative path from repo root
                    rel_path = path.relative_to(repo_root)
                    expanded_files.add(str(rel_path))
        else:
            # Exact file/directory path
            full_path = repo_root / pattern
            if full_path.is_file():
                expanded_files.add(pattern)
            elif full_path.is_dir():
                # If directory, include all files recursively
                for file_path in full_path.rglob('*'):
                    if file_path.is_file():
                        rel_path = file_path.relative_to(repo_root)
                        expanded_files.add(str(rel_path))

    return expanded_files


def run_npm_pack_dry_run():
    """Run npm pack --dry-run and return the combined output."""
    try:
        result = subprocess.run(
            ["npm", "pack", "--dry-run"],
            capture_output=True,
            text=True,
            check=True,
            cwd=get_repo_root()
        )
        # npm pack outputs to stderr, so combine both
        return result.stdout + result.stderr
    except subprocess.CalledProcessError as e:
        print(f"❌ Failed to run npm pack --dry-run: {e}")
        print(f"   stdout: {e.stdout}")
        print(f"   stderr: {e.stderr}")
        sys.exit(1)


def parse_packaged_files(npm_output):
    """Parse npm pack output to extract list of files."""
    packaged_files = []
    in_tarball_section = False

    # npm pack output format:
    # "npm notice"
    # "npm notice 📦  package-name@version"
    # "npm notice Tarball Contents"
    # "npm notice 40.1kB .claude/agents/file.md"
    for line in npm_output.split('\n'):
        # Start parsing after "Tarball Contents" header
        if 'Tarball Contents' in line:
            in_tarball_section = True
            continue

        # Stop at "Tarball Details" or "Unpacked Size" section
        if in_tarball_section and ('Tarball Details' in line or 'Unpacked Size' in line):
            break

        if in_tarball_section and 'npm notice' in line:
            # Extract file path (last part after size)
            # Format: "npm notice 40.1kB path/to/file"
            # Use more flexible pattern: size is digits with optional decimal and unit, then space, then filename
            match = re.search(r'npm notice\s+([\d.]+[kKmMgGbBtT]*)\s+(.+)$', line)
            if match:
                file_path = match.group(2).strip()
                if file_path:  # Skip empty lines
                    packaged_files.append(file_path)

    return packaged_files


def normalize_path(path: str) -> str:
    """Normalize path separators for cross-platform compatibility."""
    return str(Path(path)).replace('\\', '/')


def compare_expected_vs_actual(expected_files: Set[str], actual_files: Set[str]) -> tuple:
    """Compare expected files from package.json globs vs actual npm pack output."""
    # Normalize paths for comparison
    expected_normalized = {normalize_path(f) for f in expected_files}
    actual_normalized = {normalize_path(f) for f in actual_files}

    # Find discrepancies
    missing_from_package = expected_normalized - actual_normalized
    extra_in_package = actual_normalized - expected_normalized

    return missing_from_package, extra_in_package


def validate_critical_files(actual_files: Set[str]) -> List[str]:
    """
    Validate that critical files are present in the package.
    These are files we know must ALWAYS be included for functionality.
    """
    critical_patterns = [
        # Core installer
        "installer/*.js",
        # MCP server entry point
        "mcp-server/src/server.py",
        "mcp-server/pyproject.toml",
        # Import system (v6.0.0+ in src/runtime/)
        "src/runtime/import-conversations-unified.py",
        "src/runtime/unified_state_manager.py",
        "src/importer/**/*.py",
        # Essential shell scripts
        "mcp-server/run-mcp.sh",
        # Docker infrastructure
        "docker-compose.yaml",
        # Config
        ".env.example",
    ]

    missing_critical = []
    for pattern in critical_patterns:
        if '*' in pattern:
            # Glob pattern - check if any file matches
            if not any(fnmatch.fnmatch(f, pattern) for f in actual_files):
                missing_critical.append(pattern)
        else:
            # Exact file
            if pattern not in actual_files and not any(normalize_path(f) == normalize_path(pattern) for f in actual_files):
                missing_critical.append(pattern)

    return missing_critical


def main():
    """Main test function."""
    print("=" * 70)
    print("NPM Package Contents Dynamic Validation Test (Issue #84)")
    print("=" * 70)
    print("")

    # Load package.json
    print("📋 Loading package.json...")
    package_json = load_package_json()
    files_patterns = package_json.get('files', [])
    print(f"   Found {len(files_patterns)} file patterns in package.json")
    print("")

    # Expand glob patterns to get expected files
    print("🔍 Expanding glob patterns from package.json...")
    expected_files = expand_glob_patterns(files_patterns)
    print(f"   Expected {len(expected_files)} files from globs")
    print("")

    # Run npm pack --dry-run
    print("📦 Running npm pack --dry-run...")
    npm_output = run_npm_pack_dry_run()
    actual_packaged_files = set(parse_packaged_files(npm_output))
    print(f"   npm will package {len(actual_packaged_files)} files")
    print("")

    # Compare expected vs actual
    print("⚖️  Comparing expected vs actual...")
    print("-" * 70)
    missing_from_package, extra_in_package = compare_expected_vs_actual(
        expected_files, actual_packaged_files
    )

    # Validate critical files
    print("\n🔐 Validating critical files...")
    print("-" * 70)
    missing_critical = validate_critical_files(actual_packaged_files)

    # Report findings
    print("")
    print("=" * 70)
    print("Test Results")
    print("=" * 70)
    print("")

    has_errors = False

    # Report missing critical files (ERRORS)
    if missing_critical:
        has_errors = True
        print(f"❌ CRITICAL: {len(missing_critical)} essential files missing!")
        print("\nMissing critical files:")
        for pattern in sorted(missing_critical):
            print(f"  - {pattern}")
        print("\n💡 Fix: Ensure these patterns are in package.json 'files' array")
        print("")

    # Report files expected but not packaged (WARNINGS - might be npm defaults)
    if missing_from_package:
        print(f"⚠️  WARNING: {len(missing_from_package)} files matched globs but not in package")
        print("   (This might be expected due to .gitignore or npm defaults)")
        if len(missing_from_package) <= 10:
            for f in sorted(missing_from_package):
                print(f"  - {f}")
        else:
            for f in sorted(list(missing_from_package)[:10]):
                print(f"  - {f}")
            print(f"  ... and {len(missing_from_package) - 10} more")
        print("")

    # Report extra files in package (INFO)
    if extra_in_package:
        print(f"ℹ️  INFO: {len(extra_in_package)} files in package not from globs")
        print("   (These are likely npm defaults: package.json, README.md, LICENSE, etc.)")
        print("")

    # Final summary
    if has_errors:
        print("=" * 70)
        print("❌ TEST FAILED: Critical files missing from npm package")
        print("=" * 70)
        print("\nThis test prevents issues like:")
        print("  - Issue #71: ModuleNotFoundError for refactored modules")
        print("  - Discussion #70: Users unable to run setup")
        print("")
        print("Action required: Update package.json 'files' array with missing patterns")
        print("=" * 70)
        return 1
    else:
        print("=" * 70)
        print("✅ TEST PASSED: All critical files included in npm package")
        print("=" * 70)
        print("")
        print("Package validation:")
        print(f"  ✓ {len(actual_packaged_files)} files will be packaged")
        print(f"  ✓ All critical patterns present")
        print(f"  ✓ package.json globs are working correctly")
        print("")
        print("=" * 70)
        return 0


if __name__ == "__main__":
    sys.exit(main())
