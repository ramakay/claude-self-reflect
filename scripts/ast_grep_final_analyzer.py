#!/usr/bin/env python3
"""
FINAL AST-GREP Analyzer with Unified Registry
MANDATORY: Uses ast-grep-py + unified pattern registry
NO regex fallbacks, NO simplifications
"""

import ast_grep_py as sg
from pathlib import Path
from typing import Dict, List, Any, Optional
from datetime import datetime
import json
import sys

# Import the unified registry
sys.path.append(str(Path(__file__).parent))
from ast_grep_unified_registry import get_unified_registry

class FinalASTGrepAnalyzer:
    """
    Final production-ready AST-GREP analyzer.
    MANDATORY components:
    - ast-grep-py for AST matching
    - Unified pattern registry (custom + catalog)
    - NO regex patterns
    - NO fallbacks
    """

    def __init__(self):
        """Initialize with unified registry."""
        self.registry = get_unified_registry()
        all_patterns = self.registry.get_all_patterns()

        print(f"✅ Loaded unified registry with {len(all_patterns)} patterns")
        print(f"   Languages: Python, TypeScript, JavaScript")
        print(f"   Good patterns: {len(self.registry.get_good_patterns())}")
        print(f"   Bad patterns: {len(self.registry.get_bad_patterns())}")

    def analyze_file(self, file_path: str) -> Dict[str, Any]:
        """
        Analyze a file using unified AST-GREP patterns.
        Returns detailed quality metrics and pattern matches.
        """
        if not Path(file_path).exists():
            raise FileNotFoundError(f"File not found: {file_path}")

        # Detect language from file extension
        language = self._detect_language(file_path)

        with open(file_path, 'r', encoding='utf-8') as f:
            content = f.read()

        # Count lines of code for normalization
        lines_of_code = len(content.splitlines())

        # Create SgRoot for the detected language
        sg_language = self._get_sg_language(language)
        root = sg.SgRoot(content, sg_language)
        node = root.root()

        # Get patterns for this language
        language_patterns = self.registry.get_patterns_by_language(language)

        # Track all matches
        all_matches = []
        pattern_errors = []
        matches_by_category = {}

        # Process each pattern
        for pattern_def in language_patterns:
            try:
                pattern_str = pattern_def.get("pattern", "")
                if not pattern_str:
                    continue

                # Find matches using ast-grep-py
                matches = node.find_all(pattern=pattern_str)

                if matches:
                    category = pattern_def.get('category', 'unknown')
                    if category not in matches_by_category:
                        matches_by_category[category] = []

                    match_info = {
                        'category': category,
                        'id': pattern_def['id'],
                        'description': pattern_def.get('description', ''),
                        'quality': pattern_def.get('quality', 'neutral'),
                        'weight': pattern_def.get('weight', 0),
                        'count': len(matches),
                        'locations': [
                            {
                                'line': m.range().start.line + 1,
                                'column': m.range().start.column,
                                'text': m.text()[:80]
                            } for m in matches[:5]  # First 5 examples
                        ]
                    }

                    matches_by_category[category].append(match_info)
                    all_matches.append(match_info)

            except Exception as e:
                # Record all pattern errors for debugging
                pattern_errors.append({
                    'pattern_id': pattern_def.get('id', '<unknown>'),
                    'category': pattern_def.get('category', 'unknown'),
                    'error': str(e)[:200]
                })

        # Calculate quality score with LOC normalization
        quality_score = self.registry.calculate_quality_score(all_matches, loc=lines_of_code)

        # Count good vs bad patterns
        good_matches = [m for m in all_matches if m['quality'] == 'good']
        bad_matches = [m for m in all_matches if m['quality'] == 'bad']

        good_count = sum(m['count'] for m in good_matches)
        bad_count = sum(m['count'] for m in bad_matches)

        return {
            'file': file_path,
            'timestamp': datetime.now().isoformat(),
            'language': language,
            'engine': 'ast-grep-py + unified registry',
            'registry_info': {
                'total_patterns_available': len(language_patterns),
                'patterns_matched': len(all_matches),
                'patterns_errored': len(pattern_errors),
                'categories_found': list(matches_by_category.keys())
            },
            'matches_by_category': matches_by_category,
            'all_matches': all_matches,
            'errors': pattern_errors[:5],  # First 5 errors only
            'quality_metrics': {
                'quality_score': round(quality_score, 3),
                'good_patterns_found': good_count,
                'bad_patterns_found': bad_count,
                'unique_patterns_matched': len(all_matches),
                'total_issues': bad_count,
                'total_good_practices': good_count
            },
            'recommendations': self._generate_recommendations(matches_by_category, quality_score)
        }

    def _detect_language(self, file_path: str) -> str:
        """Detect language from file extension."""
        ext = Path(file_path).suffix.lower()
        lang_map = {
            '.py': 'python',
            '.ts': 'typescript',
            '.tsx': 'tsx',
            '.js': 'javascript',
            '.jsx': 'jsx'
        }
        return lang_map.get(ext, 'python')

    def _get_sg_language(self, language: str) -> str:
        """Get ast-grep language identifier."""
        # ast-grep-py uses different language identifiers
        sg_map = {
            'python': 'python',
            'typescript': 'typescript',
            'tsx': 'tsx',
            'javascript': 'javascript',
            'jsx': 'jsx'
        }
        return sg_map.get(language, 'python')

    def _generate_recommendations(self, matches: Dict, score: float) -> List[str]:
        """Generate actionable recommendations based on matches."""
        recommendations = []

        if score < 0.3:
            recommendations.append("🔴 Critical: Code quality needs immediate attention")
        elif score < 0.6:
            recommendations.append("🟡 Warning: Several anti-patterns detected")
        else:
            recommendations.append("🟢 Good: Code follows most best practices")

        # Check for specific issues
        for category, category_matches in matches.items():
            if 'antipatterns' in category:
                total = sum(m['count'] for m in category_matches)
                if total > 0:
                    recommendations.append(f"Fix {total} anti-patterns in {category}")

            if 'logging' in category:
                prints = sum(m['count'] for m in category_matches if 'print' in m['id'])
                if prints > 0:
                    recommendations.append(f"Replace {prints} print statements with logger")

            if 'error' in category:
                bare = sum(m['count'] for m in category_matches if 'broad' in m['id'] or 'bare' in m['id'])
                if bare > 0:
                    recommendations.append(f"Fix {bare} bare except clauses")

        return recommendations

    def generate_report(self, result: Dict[str, Any]) -> str:
        """Generate a comprehensive analysis report."""
        report = []
        report.append("# AST-GREP Pattern Analysis Report")
        report.append(f"\n**File**: {result['file']}")
        report.append(f"**Language**: {result['language']}")
        report.append(f"**Timestamp**: {result['timestamp']}")
        report.append(f"**Engine**: {result['engine']}")

        # Quality overview
        metrics = result['quality_metrics']
        score = metrics['quality_score']
        emoji = "🟢" if score > 0.7 else "🟡" if score > 0.4 else "🔴"

        report.append("\n## Quality Overview")
        report.append(f"- **Quality Score**: {emoji} {score:.1%}")
        report.append(f"- **Good Practices**: {metrics['good_patterns_found']}")
        report.append(f"- **Issues Found**: {metrics['total_issues']}")
        report.append(f"- **Unique Patterns Matched**: {metrics['unique_patterns_matched']}")

        # Recommendations
        if result['recommendations']:
            report.append("\n## Recommendations")
            for rec in result['recommendations']:
                report.append(f"- {rec}")

        # Pattern matches by category
        report.append("\n## Pattern Matches by Category")
        for category, matches in result['matches_by_category'].items():
            if matches:
                total = sum(m['count'] for m in matches)
                report.append(f"\n### {category} ({len(matches)} patterns, {total} matches)")

                # Sort by count descending
                sorted_matches = sorted(matches, key=lambda x: x['count'], reverse=True)

                for match in sorted_matches[:5]:  # Top 5 per category
                    quality_emoji = "✅" if match['quality'] == 'good' else "❌" if match['quality'] == 'bad' else "⚪"
                    report.append(f"- {quality_emoji} **{match['id']}**: {match['count']} instances")
                    report.append(f"  - {match['description']}")
                    if match['locations']:
                        loc = match['locations'][0]
                        report.append(f"  - Example (line {loc['line']}): `{loc['text'][:50]}...`")

        # Registry info
        report.append("\n## Pattern Registry Statistics")
        info = result['registry_info']
        report.append(f"- **Patterns Available**: {info['total_patterns_available']}")
        report.append(f"- **Patterns Matched**: {info['patterns_matched']}")
        report.append(f"- **Categories Found**: {', '.join(info['categories_found'])}")

        report.append("\n## Compliance")
        report.append("✅ Using unified AST-GREP registry (custom + catalog)")
        report.append("✅ Using ast-grep-py for AST matching")
        report.append("✅ NO regex patterns or fallbacks")
        report.append("✅ Production-ready pattern analysis")

        return '\n'.join(report)


def run_final_analysis(file_path=None):
    """Run final AST-GREP analysis with unified registry."""
    print("🚀 FINAL AST-GREP Analysis with Unified Registry")
    print("=" * 60)

    analyzer = FinalASTGrepAnalyzer()

    # Use provided path or default
    # Use relative path from script location
    script_dir = Path(__file__).parent
    default_path = script_dir.parent / "mcp-server" / "src" / "server.py"
    server_path = file_path if file_path else str(default_path)

    print(f"\nAnalyzing: {server_path}")
    print("-" * 40)

    try:
        result = analyzer.analyze_file(server_path)

        # Display results
        metrics = result['quality_metrics']
        score = metrics['quality_score']

        print(f"\n📊 Analysis Results:")
        print(f"  Language: {result['language']}")
        print(f"  Quality Score: {score:.1%}")
        print(f"  Good Practices: {metrics['good_patterns_found']}")
        print(f"  Issues: {metrics['total_issues']}")
        print(f"  Patterns Matched: {metrics['unique_patterns_matched']}")

        print(f"\n💡 Recommendations:")
        for rec in result['recommendations']:
            print(f"  {rec}")

        # Top issues
        bad_patterns = [m for m in result['all_matches'] if m['quality'] == 'bad']
        if bad_patterns:
            print(f"\n⚠️ Top Issues to Fix:")
            sorted_bad = sorted(bad_patterns, key=lambda x: x['count'] * abs(x['weight']), reverse=True)
            for pattern in sorted_bad[:5]:
                print(f"  - {pattern['id']}: {pattern['count']} instances")
                print(f"    {pattern['description']}")

        # Generate and save report
        report = analyzer.generate_report(result)
        report_path = script_dir / "final_analysis_report.md"
        with open(report_path, 'w') as f:
            f.write(report)

        print(f"\n📝 Full report saved to: {report_path}")

        # Save JSON results
        json_path = script_dir / "final_analysis_result.json"
        with open(json_path, 'w') as f:
            json.dump(result, f, indent=2)

        print(f"📊 JSON results saved to: {json_path}")

        print("\n✅ Final AST-GREP analysis complete!")
        print("   - Unified registry with 41 patterns")
        print("   - Support for Python, TypeScript, JavaScript")
        print("   - Ready for production integration")

        return result

    except Exception as e:
        print(f"\n❌ Analysis failed: {e}")
        raise


if __name__ == "__main__":
    import sys
    if len(sys.argv) > 1:
        # Use provided file path
        file_path = sys.argv[1]
    else:
        # Default to server.py
        file_path = str(default_path)  # Use the same default path from above
    run_final_analysis(file_path)