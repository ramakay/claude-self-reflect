#!/usr/bin/env python3
"""
AST-GREP Catalog Parser
Parses official AST-GREP catalog patterns from GitHub
and converts them for use with ast-grep-py
"""

import re
import yaml
from typing import Dict, List, Any, Optional
from pathlib import Path
import json

class ASTGrepCatalogParser:
    """
    Parser for AST-GREP official catalog patterns.
    Extracts YAML rules from markdown files and converts them
    to a format usable by ast-grep-py.
    """

    def __init__(self):
        self.parsed_patterns = {}

    def parse_markdown_pattern(self, content: str) -> Optional[Dict[str, Any]]:
        """
        Extract YAML pattern from markdown catalog file.
        Returns parsed pattern or None if not found.
        """
        # Look for YAML code block
        yaml_match = re.search(r'```yaml\n(.*?)\n```', content, re.DOTALL)
        if not yaml_match:
            return None

        yaml_content = yaml_match.group(1)

        try:
            pattern_data = yaml.safe_load(yaml_content)
            return pattern_data
        except yaml.YAMLError as e:
            print(f"Failed to parse YAML: {e}")
            return None

    def extract_metadata(self, content: str) -> Dict[str, str]:
        """Extract metadata from markdown content."""
        metadata = {}

        # Extract title (first H2)
        title_match = re.search(r'^## (.+?)(?:\s*<Badge.*?>)?$', content, re.MULTILINE)
        if title_match:
            metadata['title'] = title_match.group(1).strip()

        # Extract description
        desc_match = re.search(r'### Description\n\n(.+?)(?=\n###|\n```|\Z)', content, re.DOTALL)
        if desc_match:
            metadata['description'] = desc_match.group(1).strip()

        # Check if has fix
        metadata['has_fix'] = 'Has Fix' in content

        return metadata

    def convert_to_ast_grep_py(self, catalog_pattern: Dict[str, Any], metadata: Dict[str, str]) -> Dict[str, Any]:
        """
        Convert catalog pattern to ast-grep-py compatible format.
        """
        converted = {
            'id': catalog_pattern.get('id', 'unknown'),
            'title': metadata.get('title', ''),
            'description': metadata.get('description', ''),
            'severity': catalog_pattern.get('severity', 'warning'),
            'language': catalog_pattern.get('language', 'typescript'),
            'has_fix': metadata.get('has_fix', False)
        }

        # Extract the main pattern
        rule = catalog_pattern.get('rule', {})

        # Handle different rule structures
        if isinstance(rule, dict):
            if 'pattern' in rule:
                converted['pattern'] = rule['pattern']
            elif 'any' in rule:
                converted['patterns'] = rule['any']
                converted['match_type'] = 'any'
            elif 'all' in rule:
                converted['patterns'] = rule['all']
                converted['match_type'] = 'all'

            # Handle constraints
            if 'inside' in rule:
                converted['inside'] = rule['inside']
            if 'not' in rule:
                converted['not'] = rule['not']

        # Handle fix
        if 'fix' in catalog_pattern:
            converted['fix'] = catalog_pattern['fix']

        # Handle constraints
        if 'constraints' in catalog_pattern:
            converted['constraints'] = catalog_pattern['constraints']

        # Assign quality based on pattern type
        converted['quality'] = self._determine_quality(converted)
        converted['weight'] = self._calculate_weight(converted)

        return converted

    def _determine_quality(self, pattern: Dict[str, Any]) -> str:
        """Determine pattern quality based on its characteristics."""
        # Patterns with fixes are generally good
        if pattern.get('has_fix'):
            return 'good'

        # Error severity patterns are important
        if pattern.get('severity') == 'error':
            return 'bad'  # These detect bad code

        # Warning patterns are neutral
        if pattern.get('severity') == 'warning':
            return 'neutral'

        return 'neutral'

    def _calculate_weight(self, pattern: Dict[str, Any]) -> int:
        """Calculate pattern weight for quality scoring."""
        quality = pattern.get('quality', 'neutral')
        severity = pattern.get('severity', 'warning')

        # Base weights by quality
        weights = {
            'good': 3,
            'neutral': 1,
            'bad': -3
        }

        base_weight = weights.get(quality, 1)

        # Adjust for severity
        if severity == 'error':
            base_weight = abs(base_weight) * 2 if base_weight < 0 else base_weight * 2

        return base_weight

    def parse_catalog_patterns(self, patterns_data: Dict[str, str]) -> Dict[str, List[Dict[str, Any]]]:
        """
        Parse multiple catalog patterns from markdown content.
        patterns_data: dict mapping pattern names to markdown content
        """
        parsed_by_category = {}

        for name, content in patterns_data.items():
            pattern = self.parse_markdown_pattern(content)
            if pattern:
                metadata = self.extract_metadata(content)
                converted = self.convert_to_ast_grep_py(pattern, metadata)

                # Categorize pattern
                category = self._categorize_pattern(converted)
                if category not in parsed_by_category:
                    parsed_by_category[category] = []

                parsed_by_category[category].append(converted)

        return parsed_by_category

    def _categorize_pattern(self, pattern: Dict[str, Any]) -> str:
        """Categorize pattern based on its characteristics."""
        pattern_id = pattern.get('id', '').lower()
        title = pattern.get('title', '').lower()

        # Async patterns
        if 'await' in pattern_id or 'async' in pattern_id or 'promise' in pattern_id:
            return 'async_patterns'

        # Console/logging patterns
        if 'console' in pattern_id or 'log' in pattern_id:
            return 'logging_patterns'

        # Import patterns
        if 'import' in pattern_id:
            return 'import_patterns'

        # Error handling
        if 'catch' in pattern_id or 'error' in pattern_id:
            return 'error_handling'

        # Testing patterns
        if 'test' in pattern_id or 'expect' in pattern_id or 'should' in pattern_id:
            return 'testing_patterns'

        # Component patterns
        if 'component' in pattern_id or 'decorator' in pattern_id:
            return 'component_patterns'

        return 'general_patterns'


# Sample TypeScript catalog patterns
SAMPLE_TS_PATTERNS = {
    'no-await-in-promise-all': """## No `await` in `Promise.all` array <Badge type="tip" text="Has Fix" />

### Description

Using `await` inside an inline `Promise.all` array is usually a mistake, as it defeats the purpose of running the promises in parallel.

### YAML
```yaml
id: no-await-in-promise-all
language: typescript
severity: error
message: No await in Promise.all
rule:
  pattern: await $A
  inside:
    pattern: Promise.all($_)
    stopBy:
      not: { any: [{kind: array}, {kind: arguments}] }
fix: $A
```
""",
    'no-console-except-catch': """## No `console` except in `catch` block <Badge type="tip" text="Has Fix" />

### Description

Using `console` methods is usually for debugging purposes and therefore not suitable to ship to the client.

### YAML
```yaml
id: no-console-except-error
language: typescript
severity: warning
rule:
  any:
    - pattern: console.error($$$)
      not:
        inside:
          kind: catch_clause
          stopBy: end
    - pattern: console.$METHOD($$$)
constraints:
  METHOD:
    regex: 'log|debug|warn'
fix: ''
```
"""
}


def create_catalog_based_registry():
    """Create a comprehensive catalog-based registry."""
    parser = ASTGrepCatalogParser()

    # Parse sample patterns
    patterns_by_category = parser.parse_catalog_patterns(SAMPLE_TS_PATTERNS)

    # Create registry structure
    registry = {
        'source': 'ast-grep-catalog',
        'version': '1.0.0',
        'patterns': patterns_by_category,
        'stats': {
            'total_patterns': sum(len(p) for p in patterns_by_category.values()),
            'categories': list(patterns_by_category.keys())
        }
    }

    return registry


if __name__ == "__main__":
    print("AST-GREP Catalog Parser")
    print("=" * 60)

    registry = create_catalog_based_registry()

    print(f"\nParsed {registry['stats']['total_patterns']} patterns")
    print(f"Categories: {', '.join(registry['stats']['categories'])}")

    # Display patterns
    for category, patterns in registry['patterns'].items():
        print(f"\n{category}:")
        for pattern in patterns:
            print(f"  - {pattern['id']}: {pattern.get('title', 'N/A')}")
            print(f"    Quality: {pattern['quality']}, Weight: {pattern['weight']}")
            if 'pattern' in pattern:
                print(f"    Pattern: {pattern['pattern'][:50]}...")

    # Save to JSON
    # Use relative path from script location
    script_dir = Path(__file__).parent
    output_path = script_dir / "catalog_registry.json"
    with open(output_path, 'w') as f:
        json.dump(registry, f, indent=2)

    print(f"\n✅ Saved catalog registry to {output_path}")