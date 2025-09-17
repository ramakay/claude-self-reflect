#!/usr/bin/env python3
"""
Release Gate System for Claude Self-Reflect
Ensures releases meet quality standards before proceeding.
"""

import json
import subprocess
import sys
from pathlib import Path
from datetime import datetime
from typing import Dict, List, Tuple, Optional

class ReleaseGate:
    """Stringent release gate with multiple checks."""

    def __init__(self):
        self.checks_passed = []
        self.checks_failed = []
        self.warnings = []
        self.release_version = None

    def check_version_bump(self) -> bool:
        """Ensure version has been properly bumped."""
        try:
            package_json = Path("package.json")
            if package_json.exists():
                with open(package_json) as f:
                    data = json.load(f)
                    self.release_version = data.get("version")

                # Check if release notes exist for this version
                release_notes = Path(f"docs/RELEASE_NOTES_v{self.release_version}.md")
                if release_notes.exists():
                    self.checks_passed.append(f"✅ Version {self.release_version} properly bumped")
                    self.checks_passed.append(f"✅ Release notes exist for v{self.release_version}")
                    return True
                else:
                    self.checks_failed.append(f"❌ No release notes for v{self.release_version}")
                    return False
            else:
                self.checks_failed.append("❌ package.json not found")
                return False
        except Exception as e:
            self.checks_failed.append(f"❌ Version check failed: {e}")
            return False

    def check_no_transient_files(self) -> bool:
        """Ensure no test/temporary files in root."""
        transient_patterns = [
            "test*.py",
            "*_report.json",
            "*_report.md",
            "*.bak",
            ".env.test",
            "test-*.js",
            "*-test.py",
            "enhanced_pattern_report.json",
            "pattern_value_report.json",
            "simplified_extractor_report.json"
        ]

        root = Path(".")
        found_transient = []

        for pattern in transient_patterns:
            matches = list(root.glob(pattern))
            found_transient.extend(matches)

        if found_transient:
            self.checks_failed.append("❌ Transient files found in root:")
            for f in found_transient[:5]:  # Show first 5
                self.checks_failed.append(f"   - {f}")
            if len(found_transient) > 5:
                self.checks_failed.append(f"   ... and {len(found_transient) - 5} more")
            return False
        else:
            self.checks_passed.append("✅ No transient files in root")
            return True

    def check_no_personal_data(self) -> bool:
        """Check for personal/sensitive data."""
        sensitive_patterns = [
            "/Users/ramakrishnanannaswamy",
            "ramakrishnanannaswamy",
            "metafora",
            "procsolve",
            "anukruti"
        ]

        files_to_check = [
            "README.md",
            "docs/*.md",
            "scripts/*.py",
            "mcp-server/src/*.py"
        ]

        found_sensitive = []

        for pattern in files_to_check:
            for file_path in Path(".").glob(pattern):
                if file_path.is_file():
                    try:
                        content = file_path.read_text()
                        for sensitive in sensitive_patterns:
                            if sensitive.lower() in content.lower():
                                # Skip if it's in a comment or example
                                if not any(skip in str(file_path) for skip in ["CLAUDE.md", "release-gate.py"]):
                                    found_sensitive.append((file_path, sensitive))
                                    break
                    except:
                        pass

        if found_sensitive:
            self.warnings.append("⚠️  Potential sensitive data found:")
            for file_path, pattern in found_sensitive[:3]:
                self.warnings.append(f"   - {file_path}: contains '{pattern}'")
            return True  # Warning only, don't block
        else:
            self.checks_passed.append("✅ No obvious personal data found")
            return True

    def check_tests_pass(self) -> bool:
        """Ensure tests pass (if they exist)."""
        test_commands = [
            ("pytest tests/", "Python tests"),
            ("npm test", "JavaScript tests"),
        ]

        any_tests_found = False
        all_pass = True

        for cmd, name in test_commands:
            try:
                result = subprocess.run(
                    cmd.split(),
                    capture_output=True,
                    text=True,
                    timeout=30
                )
                if result.returncode == 0:
                    self.checks_passed.append(f"✅ {name} pass")
                    any_tests_found = True
                else:
                    # Check if tests don't exist vs failed
                    if "no tests" in result.stderr.lower() or "not found" in result.stderr.lower():
                        continue
                    else:
                        self.checks_failed.append(f"❌ {name} failed")
                        all_pass = False
                        any_tests_found = True
            except:
                continue

        if not any_tests_found:
            self.warnings.append("⚠️  No automated tests found")

        return all_pass

    def check_code_quality(self) -> bool:
        """Check code quality metrics."""
        try:
            # Run quality check on CSR itself
            result = subprocess.run(
                ["python", "scripts/session_quality_tracker.py",
                 "--project-path", ".",
                 "--project-name", "claude-self-reflect"],
                capture_output=True,
                text=True,
                timeout=30
            )

            if result.returncode == 0:
                # Parse output for grade
                output = result.stdout
                if "Overall Grade:" in output:
                    for line in output.split('\n'):
                        if "Overall Grade:" in line:
                            grade = line.split(':')[1].strip().split()[0]
                            if grade in ['A+', 'A', 'B', 'C+']:
                                self.checks_passed.append(f"✅ Code quality: {grade}")
                                return True
                            else:
                                self.checks_failed.append(f"❌ Code quality too low: {grade}")
                                return False

            self.warnings.append("⚠️  Could not assess code quality")
            return True
        except:
            self.warnings.append("⚠️  Code quality check failed")
            return True

    def check_documentation(self) -> bool:
        """Ensure critical documentation exists."""
        required_docs = [
            "README.md",
            "LICENSE",
            f"docs/RELEASE_NOTES_v{self.release_version}.md" if self.release_version else None,
        ]

        missing = []
        for doc in required_docs:
            if doc and not Path(doc).exists():
                missing.append(doc)

        if missing:
            self.checks_failed.append("❌ Missing documentation:")
            for doc in missing:
                self.checks_failed.append(f"   - {doc}")
            return False
        else:
            self.checks_passed.append("✅ Required documentation present")
            return True

    def run_all_checks(self) -> Tuple[bool, str]:
        """Run all release gate checks."""
        print("🚦 RELEASE GATE SYSTEM")
        print("=" * 60)

        # Critical checks (must pass)
        critical_checks = [
            ("Version Bump", self.check_version_bump),
            ("Documentation", self.check_documentation),
            ("No Transient Files", self.check_no_transient_files),
            ("Tests", self.check_tests_pass),
            ("Code Quality", self.check_code_quality),
        ]

        # Warning checks (don't block release)
        warning_checks = [
            ("Personal Data", self.check_no_personal_data),
        ]

        all_critical_pass = True

        print("\n🔍 Running Critical Checks...")
        for name, check_func in critical_checks:
            print(f"   Checking {name}...", end="")
            result = check_func()
            print(" ✅" if result else " ❌")
            if not result:
                all_critical_pass = False

        print("\n🔍 Running Warning Checks...")
        for name, check_func in warning_checks:
            print(f"   Checking {name}...", end="")
            check_func()
            print(" ✓")

        # Generate report
        report = []
        report.append("\n" + "=" * 60)
        report.append("📊 RELEASE GATE REPORT")
        report.append("=" * 60)

        if self.release_version:
            report.append(f"Version: {self.release_version}")

        report.append(f"\n✅ Passed Checks ({len(self.checks_passed)}):")
        for check in self.checks_passed:
            report.append(f"   {check}")

        if self.checks_failed:
            report.append(f"\n❌ Failed Checks ({len(self.checks_failed)}):")
            for check in self.checks_failed:
                report.append(f"   {check}")

        if self.warnings:
            report.append(f"\n⚠️  Warnings ({len(self.warnings)}):")
            for warning in self.warnings:
                report.append(f"   {warning}")

        report.append("\n" + "=" * 60)

        if all_critical_pass:
            report.append("✅ RELEASE GATE: PASSED")
            report.append("Ready to proceed with release!")
        else:
            report.append("❌ RELEASE GATE: FAILED")
            report.append("Fix the issues above before releasing.")

        report.append("=" * 60)

        report_text = "\n".join(report)
        return all_critical_pass, report_text

    def create_release_checklist(self) -> str:
        """Create a release checklist for manual verification."""
        checklist = """
# RELEASE CHECKLIST v3.3.2

## Pre-Release Checks
- [ ] Version bumped in package.json
- [ ] Release notes created (docs/RELEASE_NOTES_vX.X.X.md)
- [ ] All tests pass
- [ ] Code quality grade is acceptable (B or higher)
- [ ] No transient/test files in root
- [ ] No personal/sensitive data in code
- [ ] README updated if needed
- [ ] CHANGELOG updated

## Code Review Requirements
- [ ] Self-review completed
- [ ] No hardcoded paths or credentials
- [ ] No debug console.log/print statements (except in tests)
- [ ] Error handling is appropriate
- [ ] Documentation is clear

## Release Process
1. [ ] Run release gate: `python scripts/release-gate.py`
2. [ ] Fix any issues identified
3. [ ] Create git commit with message: "chore: release vX.X.X"
4. [ ] Create git tag: `git tag vX.X.X`
5. [ ] Push to GitHub: `git push origin main --tags`
6. [ ] Create GitHub release with release notes
7. [ ] Publish to npm if applicable

## Post-Release
- [ ] Verify GitHub release is created
- [ ] Test installation from npm/pip
- [ ] Update project boards/issues
- [ ] Announce in relevant channels

## THINK HARD TODO (Release Judge)
Before releasing, answer these questions:
1. Does this release solve real user problems?
2. Are breaking changes properly documented?
3. Have we tested the upgrade path?
4. Is the code maintainable by others?
5. Are we proud of this release?

Only proceed if you can answer YES to all questions.
"""
        return checklist


def main():
    """Main entry point."""
    gate = ReleaseGate()

    # Run all checks
    passed, report = gate.run_all_checks()

    # Print report
    print(report)

    if not passed:
        print("\n⚠️  Release blocked due to failed checks.")
        print("Fix the issues and run again.")
        sys.exit(1)
    else:
        # Save checklist for manual review
        checklist = gate.create_release_checklist()
        checklist_path = Path("RELEASE_CHECKLIST.md")
        checklist_path.write_text(checklist)
        print(f"\n📋 Release checklist saved to {checklist_path}")
        print("Review and complete the checklist before releasing.")

        print("\n🎯 THINK HARD: Is this release truly ready?")
        print("Only proceed if you've thoroughly reviewed everything.")
        sys.exit(0)


if __name__ == "__main__":
    main()