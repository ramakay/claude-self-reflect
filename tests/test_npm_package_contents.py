#!/usr/bin/env python3
"""
Regression test to ensure all required files are included in npm package.
Prevents issues like #71 where refactored modules were missing from package.

This test:
1. Runs `npm pack --dry-run` to get the list of files that would be packaged
2. Verifies all critical Python modules are included
3. Fails the build if any required files are missing
"""

import subprocess
import sys
import re
from pathlib import Path


# Required Python modules that MUST be in the npm package
REQUIRED_MODULES = [
    # Refactored import modules (PR #69)
    "scripts/metadata_extractor.py",
    "scripts/message_processors.py",
    "scripts/import_strategies.py",
    "scripts/embedding_service.py",

    # Core utilities
    "scripts/doctor.py",
    "scripts/unified_state_manager.py",
    "scripts/utils.py",

    # Import scripts
    "scripts/import-conversations-unified.py",

    # Shell scripts (required for Docker services)
    "scripts/watcher-loop.sh",
    "scripts/precompact-hook.sh",

    # MCP server
    "mcp-server/src/server.py",
    "mcp-server/src/embedding_manager.py",
]

# Critical directories that should have files packaged
REQUIRED_DIRS = [
    "scripts/",
    "mcp-server/src/",
    "shared/",
    "installer/",
]


def run_npm_pack_dry_run():
    """Run npm pack --dry-run and return the combined output."""
    try:
        result = subprocess.run(
            ["npm", "pack", "--dry-run"],
            capture_output=True,
            text=True,
            check=True,
            cwd=Path(__file__).parent.parent
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


def check_required_modules(packaged_files):
    """Verify all required modules are in the package."""
    missing_files = []

    for required_file in REQUIRED_MODULES:
        # Check if the file or pattern is in packaged files
        found = any(required_file in pkg_file for pkg_file in packaged_files)

        if not found:
            missing_files.append(required_file)
            print(f"❌ Missing required file: {required_file}")
        else:
            print(f"✅ Found: {required_file}")

    return missing_files


def check_required_directories(packaged_files):
    """Verify files from required directories are included."""
    missing_dirs = []

    for required_dir in REQUIRED_DIRS:
        # Check if any files from this directory are packaged
        found = any(required_dir in pkg_file for pkg_file in packaged_files)

        if not found:
            missing_dirs.append(required_dir)
            print(f"⚠️  No files from directory: {required_dir}")
        else:
            dir_files = [f for f in packaged_files if required_dir in f]
            print(f"✅ {required_dir} has {len(dir_files)} files")

    return missing_dirs


def main():
    """Main test function."""
    print("=" * 60)
    print("NPM Package Contents Regression Test")
    print("=" * 60)
    print("")

    # Run npm pack --dry-run
    print("Running npm pack --dry-run...")
    npm_output = run_npm_pack_dry_run()

    # Parse the output
    packaged_files = parse_packaged_files(npm_output)
    print(f"\n📦 Total files to be packaged: {len(packaged_files)}")
    print("")

    # Check required modules
    print("Checking required Python modules...")
    print("-" * 60)
    missing_modules = check_required_modules(packaged_files)
    print("")

    # Check required directories
    print("Checking required directories...")
    print("-" * 60)
    missing_dirs = check_required_directories(packaged_files)
    print("")

    # Summary
    print("=" * 60)
    print("Test Summary")
    print("=" * 60)

    if missing_modules:
        print(f"❌ FAILED: {len(missing_modules)} required modules missing!")
        print("\nMissing modules:")
        for module in missing_modules:
            print(f"  - {module}")
        print("\n💡 Fix: Add missing files to package.json 'files' array")
        sys.exit(1)

    if missing_dirs:
        print(f"⚠️  WARNING: {len(missing_dirs)} directories have no files")
        print("This may be intentional, but verify:")
        for dir_path in missing_dirs:
            print(f"  - {dir_path}")

    print("✅ PASSED: All required modules are included in npm package")
    print("\nThis test prevents issues like:")
    print("  - Issue #71: ModuleNotFoundError for refactored modules")
    print("  - Discussion #70: Users unable to run setup")
    print("")
    print("=" * 60)

    return 0


if __name__ == "__main__":
    sys.exit(main())
