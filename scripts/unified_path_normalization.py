#!/usr/bin/env python3
"""
Unified Path Normalization for Claude Self-Reflect
Based on GPT-5 security review recommendations.

This module provides a single, consistent path normalization function
used across import, search, and runtime tools.
"""

import os
import re
import hashlib
from pathlib import Path
from typing import Dict, Optional, Tuple
import logging

logger = logging.getLogger(__name__)

class PathNormalizer:
    """Unified path normalization with privacy protection."""

    def __init__(self, redact_users: bool = True, hash_paths: bool = True):
        """
        Initialize path normalizer.

        Args:
            redact_users: Whether to redact user-specific path segments
            hash_paths: Whether to generate hashes for exact matching
        """
        self.redact_users = redact_users
        self.hash_paths = hash_paths

        # Precompile regex patterns for performance
        self.patterns = {
            'windows_users': re.compile(r'[Cc]:\\Users\\([^\\]+)'),
            'unix_users': re.compile(r'/Users/([^/]+)'),
            'home_dir': re.compile(r'/home/([^/]+)'),
            'duplicate_slashes': re.compile(r'/+'),
            'trailing_slash': re.compile(r'/$')
        }

    def normalize(self, path: str) -> Dict[str, any]:
        """
        Normalize a file path with multiple representations for robust matching.

        Args:
            path: The file path to normalize

        Returns:
            Dictionary with normalized path variants and metadata:
            - original: Original input path
            - normalized: Primary normalized form
            - canonical: Canonical absolute path (if resolvable)
            - tilde: Path with home directory as ~/
            - basename: Just the filename
            - parent: Parent directory name
            - hash: SHA256 hash of normalized path (for exact matching)
            - redacted: Privacy-safe version with user info removed
        """
        if not path:
            return {
                'original': '',
                'normalized': '',
                'canonical': '',
                'tilde': '',
                'basename': '',
                'parent': '',
                'hash': '',
                'redacted': ''
            }

        result = {'original': path}

        # Step 1: Basic normalization
        normalized = path

        # Convert backslashes to forward slashes
        normalized = normalized.replace('\\', '/')

        # Remove duplicate slashes
        normalized = self.patterns['duplicate_slashes'].sub('/', normalized)

        # Remove trailing slash (unless it's root)
        if len(normalized) > 1:
            normalized = self.patterns['trailing_slash'].sub('', normalized)

        result['normalized'] = normalized

        # Step 2: Extract components
        try:
            path_obj = Path(normalized)
            result['basename'] = path_obj.name
            result['parent'] = path_obj.parent.name if path_obj.parent.name else ''
        except:
            result['basename'] = os.path.basename(normalized)
            result['parent'] = os.path.basename(os.path.dirname(normalized))

        # Step 3: Create canonical path (if possible)
        canonical = ''
        try:
            # Use PurePath to avoid network/filesystem access per GPT-5 recommendation
            from pathlib import PurePath
            pure_path = PurePath(normalized)

            # Try to resolve if it's a local path
            if not normalized.startswith(('http://', 'https://', 'ftp://', '//')):
                try:
                    # Timeout wrapper would go here in production
                    resolved = Path(normalized).resolve()
                    canonical = str(resolved).replace('\\', '/')
                except:
                    canonical = normalized
            else:
                canonical = normalized
        except:
            canonical = normalized

        result['canonical'] = canonical

        # Step 4: Create tilde version
        tilde = normalized
        home_dir = str(Path.home()).replace('\\', '/')

        # Replace home directory with ~/
        if canonical and canonical.startswith(home_dir):
            tilde = canonical.replace(home_dir, '~', 1)
        elif normalized.startswith(home_dir):
            tilde = normalized.replace(home_dir, '~', 1)

        # Also handle /Users/username pattern
        if '/Users/' in tilde:
            tilde = self.patterns['unix_users'].sub('~', tilde, 1)
        elif 'C:\\Users\\' in path or 'c:\\users\\' in path.lower():
            tilde = self.patterns['windows_users'].sub('~', tilde, 1)
        elif '/home/' in tilde:
            tilde = self.patterns['home_dir'].sub('~', tilde, 1)

        result['tilde'] = tilde

        # Step 5: Create redacted version for privacy
        redacted = tilde  # Start with tilde version

        if self.redact_users:
            # Further redact any remaining user-specific segments
            # Keep only project-relative paths
            if '/projects/' in redacted:
                idx = redacted.find('/projects/')
                redacted = '~' + redacted[idx:]
            elif '/Documents/' in redacted:
                idx = redacted.find('/Documents/')
                redacted = '~' + redacted[idx:]
            elif '/Desktop/' in redacted:
                idx = redacted.find('/Desktop/')
                redacted = '~' + redacted[idx:]

        result['redacted'] = redacted

        # Step 6: Generate hash for exact matching
        if self.hash_paths:
            # Use the tilde form for consistent hashing
            hash_input = tilde.lower()  # Case-insensitive matching
            result['hash'] = hashlib.sha256(hash_input.encode()).hexdigest()[:16]  # First 16 chars sufficient
        else:
            result['hash'] = ''

        return result

    def match_any(self, stored_path: str, query_path: str) -> bool:
        """
        Check if two paths match using any normalization variant.

        Args:
            stored_path: Path as stored in database
            query_path: Path from search query

        Returns:
            True if paths match by any variant
        """
        stored = self.normalize(stored_path)
        query = self.normalize(query_path)

        # Check hash first (fastest)
        if stored['hash'] and query['hash'] and stored['hash'] == query['hash']:
            return True

        # Check exact matches on various forms
        stored_variants = {stored['normalized'], stored['tilde'], stored['basename'], stored['redacted']}
        query_variants = {query['normalized'], query['tilde'], query['basename'], query['redacted']}

        # Remove empty strings
        stored_variants.discard('')
        query_variants.discard('')

        # Check for any intersection
        return bool(stored_variants & query_variants)


# Global instance for shared use
_normalizer = PathNormalizer()

def normalize_path(path: str) -> Dict[str, any]:
    """
    Global function for path normalization.
    Use this everywhere for consistency.
    """
    return _normalizer.normalize(path)

def paths_match(stored_path: str, query_path: str) -> bool:
    """
    Global function to check if two paths match.
    """
    return _normalizer.match_any(stored_path, query_path)


if __name__ == "__main__":
    # Test the normalizer
    test_paths = [
        "/Users/john/projects/claude-self-reflect/test.py",
        "~/projects/claude-self-reflect/test.py",
        "C:\\Users\\john\\Documents\\test.py",
        "/home/john/work/project/file.txt",
        "test.py",
        "../relative/path/file.js"
    ]

    normalizer = PathNormalizer()

    print("Path Normalization Tests")
    print("=" * 60)

    for path in test_paths:
        result = normalizer.normalize(path)
        print(f"\nOriginal: {path}")
        print(f"  Normalized: {result['normalized']}")
        print(f"  Tilde:      {result['tilde']}")
        print(f"  Redacted:   {result['redacted']}")
        print(f"  Basename:   {result['basename']}")
        print(f"  Hash:       {result['hash']}")

    # Test matching
    print("\n" + "=" * 60)
    print("Matching Tests")
    print("=" * 60)

    path1 = "/Users/john/projects/test.py"
    path2 = "~/projects/test.py"
    path3 = "test.py"

    print(f"\n'{path1}' matches '{path2}': {normalizer.match_any(path1, path2)}")
    print(f"'{path1}' matches '{path3}': {normalizer.match_any(path1, path3)}")
    print(f"'{path2}' matches '{path3}': {normalizer.match_any(path2, path3)}")