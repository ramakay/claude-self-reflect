#!/usr/bin/env python3
"""
Auto-update AST-GREP patterns from official catalog.
MANDATORY feature - runs on every import to ensure latest patterns.
Fast execution: <1 second with caching.
"""

import os
import json
import yaml
import re
import hashlib
from pathlib import Path
from datetime import datetime, timedelta
from typing import Dict, List, Any, Optional
import subprocess
import tempfile
import shutil
import logging

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

# Configuration
CACHE_DIR = Path.home() / ".claude-self-reflect" / "cache" / "patterns"
CACHE_FILE = CACHE_DIR / "pattern_cache.json"
REGISTRY_FILE = Path(__file__).parent / "unified_registry.json"
CATALOG_REPO = "https://github.com/ast-grep/ast-grep.github.io.git"
CATALOG_PATH = "website/catalog"
CACHE_HOURS = 24  # Check for updates once per day

# Ensure cache directory exists
CACHE_DIR.mkdir(parents=True, exist_ok=True)


class PatternUpdater:
    """Updates AST-GREP patterns from official catalog."""

    def __init__(self):
        self.patterns = {}
        self.stats = {
            'total_patterns': 0,
            'new_patterns': 0,
            'updated_patterns': 0,
            'languages': set()
        }

    def should_update(self) -> bool:
        """Check if patterns need updating based on cache age."""
        if not CACHE_FILE.exists():
            return True

        try:
            with open(CACHE_FILE, 'r') as f:
                cache = json.load(f)

            cached_time = datetime.fromisoformat(cache.get('timestamp', '2000-01-01'))
            if datetime.now() - cached_time > timedelta(hours=CACHE_HOURS):
                return True

            # Also check if registry file is missing
            if not REGISTRY_FILE.exists():
                return True

            return False
        except:
            return True

    def fetch_catalog_patterns(self) -> Dict[str, List[Dict]]:
        """Fetch latest patterns from AST-GREP GitHub catalog."""
        patterns_by_lang = {}

        with tempfile.TemporaryDirectory() as tmpdir:
            repo_path = Path(tmpdir) / "ast-grep-catalog"

            try:
                # Clone or pull the repository (shallow clone for speed)
                logger.info("Fetching latest AST-GREP patterns from GitHub...")
                # Use shorter timeout to avoid blocking analysis
                timeout = int(os.environ.get("AST_GREP_CATALOG_TIMEOUT", "10"))
                subprocess.run(
                    ["git", "clone", "--depth", "1", "--single-branch",
                     CATALOG_REPO, str(repo_path)],
                    check=True,
                    capture_output=True,
                    timeout=timeout
                )

                catalog_dir = repo_path / CATALOG_PATH

                # Process each language directory
                for lang_dir in catalog_dir.iterdir():
                    if lang_dir.is_dir() and not lang_dir.name.startswith('.'):
                        language = lang_dir.name
                        patterns_by_lang[language] = []
                        self.stats['languages'].add(language)

                        # Process each pattern file
                        for pattern_file in lang_dir.glob("*.md"):
                            if pattern_file.name == "index.md":
                                continue

                            pattern = self._parse_pattern_file(pattern_file, language)
                            if pattern:
                                patterns_by_lang[language].append(pattern)
                                self.stats['total_patterns'] += 1

                logger.info(f"Fetched {self.stats['total_patterns']} patterns for {len(self.stats['languages'])} languages")

            except subprocess.TimeoutExpired:
                logger.warning("GitHub fetch timed out, using cached patterns")
                return {}
            except Exception as e:
                logger.warning(f"Failed to fetch from GitHub: {e}, using cached patterns")
                return {}

        return patterns_by_lang

    def _parse_pattern_file(self, file_path: Path, language: str) -> Optional[Dict]:
        """Parse a single pattern file from the catalog."""
        try:
            content = file_path.read_text()

            # Extract YAML block
            yaml_match = re.search(r'```yaml\n(.*?)\n```', content, re.DOTALL)
            if not yaml_match:
                return None

            yaml_content = yaml_match.group(1)
            pattern_data = yaml.safe_load(yaml_content)

            # Extract metadata
            title_match = re.search(r'^## (.+?)(?:\s*<Badge.*?>)?$', content, re.MULTILINE)
            title = title_match.group(1).strip() if title_match else file_path.stem

            # Extract description
            desc_match = re.search(r'### Description\n\n(.+?)(?=\n###|\n```|\Z)', content, re.DOTALL)
            description = desc_match.group(1).strip() if desc_match else ""

            # Build pattern object
            pattern = {
                'id': pattern_data.get('id', file_path.stem),
                'title': title,
                'description': description,
                'language': pattern_data.get('language', language),
                'file': file_path.name,
                'has_fix': 'fix' in pattern_data
            }

            # Extract rule
            if 'rule' in pattern_data:
                rule = pattern_data['rule']
                if isinstance(rule, dict):
                    if 'pattern' in rule:
                        pattern['pattern'] = rule['pattern']
                    if 'any' in rule:
                        pattern['patterns'] = rule['any']
                        pattern['match_type'] = 'any'
                    if 'all' in rule:
                        pattern['patterns'] = rule['all']
                        pattern['match_type'] = 'all'
                    if 'inside' in rule:
                        pattern['inside'] = rule['inside']

            # Add fix if present
            if 'fix' in pattern_data:
                pattern['fix'] = pattern_data['fix']

            # Determine quality based on type
            pattern['quality'] = self._determine_quality(pattern)
            pattern['weight'] = self._calculate_weight(pattern)

            return pattern

        except Exception as e:
            logger.debug(f"Failed to parse {file_path}: {e}")
            return None

    def _determine_quality(self, pattern: Dict) -> str:
        """Determine pattern quality."""
        if pattern.get('has_fix'):
            return 'good'

        # Patterns that detect issues are "bad" (they find bad code)
        if any(word in pattern.get('id', '').lower()
               for word in ['no-', 'missing-', 'avoid-', 'deprecated']):
            return 'bad'

        return 'neutral'

    def _calculate_weight(self, pattern: Dict) -> int:
        """Calculate pattern weight for scoring."""
        quality = pattern.get('quality', 'neutral')
        weights = {
            'good': 3,
            'neutral': 1,
            'bad': -3
        }
        return weights.get(quality, 1)

    def merge_with_custom_patterns(self, catalog_patterns: Dict) -> Dict:
        """Merge catalog patterns with custom local patterns."""
        # Load existing registry if it exists
        existing_patterns = {}
        if REGISTRY_FILE.exists():
            try:
                with open(REGISTRY_FILE, 'r') as f:
                    registry = json.load(f)
                    existing_patterns = registry.get('patterns', {})
            except:
                pass

        # Keep custom Python patterns (our manual additions)
        custom_categories = [
            'python_async', 'python_error_handling', 'python_logging',
            'python_typing', 'python_antipatterns', 'python_qdrant', 'python_mcp'
        ]

        merged = {}
        for category in custom_categories:
            if category in existing_patterns:
                merged[category] = existing_patterns[category]

        # Add catalog patterns
        for language, patterns in catalog_patterns.items():
            category_name = f"{language}_catalog"
            merged[category_name] = patterns

        return merged

    def save_registry(self, patterns: Dict):
        """Save updated pattern registry."""
        registry = {
            'source': 'unified-ast-grep-auto-updated',
            'version': '3.0.0',
            'timestamp': datetime.now().isoformat(),
            'patterns': patterns,
            'stats': {
                'total_patterns': sum(len(p) for p in patterns.values()),
                'categories': list(patterns.keys()),
                'languages': list(self.stats['languages']),
                'last_update': datetime.now().isoformat()
            }
        }

        with open(REGISTRY_FILE, 'w') as f:
            json.dump(registry, f, indent=2)

        logger.info(f"Saved {registry['stats']['total_patterns']} patterns to {REGISTRY_FILE}")

    def update_cache(self):
        """Update cache file with timestamp."""
        cache_data = {
            'timestamp': datetime.now().isoformat(),
            'stats': {
                'total_patterns': self.stats['total_patterns'],
                'languages': list(self.stats['languages'])
            }
        }

        with open(CACHE_FILE, 'w') as f:
            json.dump(cache_data, f)

    def update_patterns(self, force: bool = False) -> bool:
        """Main update function - FAST with caching."""
        # Check if update needed (< 10ms)
        if not force and not self.should_update():
            logger.debug("Patterns are up to date (cached)")
            return False

        logger.info("Updating AST-GREP patterns...")

        # Fetch from GitHub (only when cache expired)
        catalog_patterns = self.fetch_catalog_patterns()

        if catalog_patterns:
            # Merge with custom patterns
            merged_patterns = self.merge_with_custom_patterns(catalog_patterns)

            # Save updated registry
            self.save_registry(merged_patterns)

            # Update cache timestamp
            self.update_cache()

            logger.info(f"✅ Pattern update complete: {self.stats['total_patterns']} patterns")
            return True
        else:
            logger.info("Using existing patterns (GitHub unavailable)")
            return False


def check_and_update_patterns(force: bool = False) -> bool:
    """
    Quick pattern update check - MANDATORY but FAST.
    Called on every import, uses 24-hour cache.
    """
    updater = PatternUpdater()
    return updater.update_patterns(force=force)


def install_time_update():
    """Run during package installation - forces update."""
    logger.info("Installing AST-GREP patterns...")
    updater = PatternUpdater()
    updater.update_patterns(force=True)


if __name__ == "__main__":
    import sys

    # Allow --force flag for manual updates
    force = "--force" in sys.argv

    if force:
        print("Forcing pattern update from GitHub...")
    else:
        print("Checking for pattern updates (24-hour cache)...")

    success = check_and_update_patterns(force=force)

    if success:
        print("✅ Patterns updated successfully")
    else:
        print("✅ Patterns are up to date")

    # Show stats
    if REGISTRY_FILE.exists():
        with open(REGISTRY_FILE, 'r') as f:
            registry = json.load(f)
            stats = registry.get('stats', {})
            print(f"   Total patterns: {stats.get('total_patterns', 0)}")
            print(f"   Languages: {', '.join(stats.get('languages', []))}")
            print(f"   Last update: {stats.get('last_update', 'Unknown')}")