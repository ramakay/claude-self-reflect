#!/usr/bin/env python3
"""
Fast Pre-Commit Quality Gate
Only analyzes staged files with caching for performance
"""

import sys
import subprocess
import json
import hashlib
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
import time
from typing import ClassVar

# Try to import AST-GREP, fall back to simpler analysis if not available
try:
    import ast_grep_py as sg
    HAS_AST_GREP = True
except ImportError:
    HAS_AST_GREP = False

class FastQualityGate:
    """Fast quality gate for pre-commit hooks"""

    CACHE_DIR: ClassVar[Path] = Path('.git/quality-cache')
    CACHE_FILE: ClassVar[Path] = Path('.git/quality-cache') / 'analysis.json'
    MIN_QUALITY_SCORE: ClassVar[float] = 0.60
    MAX_WORKERS: ClassVar[int] = 4
    CACHE_TTL_SECONDS: ClassVar[int] = 86400  # 24 hours

    # Critical patterns to check (even without AST-GREP)
    CRITICAL_PATTERNS: ClassVar[list] = [
        ('eval(', 'Direct eval usage - security risk'),
        ('exec(', 'Direct exec usage - security risk'),
        ('__import__(', 'Dynamic import - security risk'),
        ('os.system(', 'Shell command execution - security risk'),
        ('subprocess.call(shell=True', 'Shell injection vulnerability'),
        ('subprocess.run(shell=True', 'Shell injection vulnerability - shell=True'),
        ('subprocess.Popen(shell=True', 'Shell injection vulnerability - shell=True'),
        ('pickle.loads(', 'Unsafe deserialization'),
        ('yaml.load(', 'Unsafe YAML loading - use safe_load'),
    ]

    # Common bad patterns (for simple analysis)
    BAD_PATTERNS: ClassVar[list] = [
        ('except:', 'Bare except clause'),
        ('import *', 'Wildcard import'),
        ('global ', 'Global variable usage'),
        ('TODO:', 'Unfinished TODO'),
        ('FIXME:', 'Unresolved FIXME'),
        ('XXX:', 'Code marked as problematic'),
        ('print(', 'Debug print statement'),
        ('console.log(', 'Debug console.log'),
        ('debugger;', 'Debugger statement'),
    ]
    
    def __init__(self):
        self.CACHE_DIR.mkdir(exist_ok=True)
        if HAS_AST_GREP:
            try:
                sys.path.append(str(Path(__file__).parent))
                from ast_grep_unified_registry import get_unified_registry
                self.registry = get_unified_registry()
            except:
                self.registry = None
        else:
            self.registry = None
        
    # Files that define patterns or are test scripts (intentional print/debug statements)
    SKIP_FILES: ClassVar[set] = {
        'scripts/quality-gate-staged.py',  # Don't analyze pattern definitions (self-flagging)
        'tests/test_npm_package_contents.py',  # Test script with intentional print statements
        'scripts/ast_grep_final_analyzer.py',  # Analysis tool with intentional print
        # Batch automation scripts - standalone tools with intentional print for user feedback
        'docs/design/batch_import_all_projects.py',
        'docs/design/batch_import_v3.py',
        'docs/design/batch_ground_truth_generator.py',
        'docs/design/import_existing_batch.py',
        'docs/design/recover_all_batches.py',
        'docs/design/recover_batch_results.py',
        'docs/design/extract_events_v3.py',
        # CLI installer scripts - user-facing tools that REQUIRE console.log for UX
        'installer/setup-wizard.js',
        'installer/setup-wizard-docker.js',
        'installer/cli.js',
        'installer/postinstall.js',
        'installer/fastembed-fallback.js',
        'installer/statusline-setup.js',
        'installer/update-manager.js',
    }

    def get_staged_files(self):
        """Get list of staged files that need analysis"""
        try:
            result = subprocess.run(
                ['git', 'diff', '--cached', '--name-only', '--diff-filter=ACM'],
                capture_output=True, text=True, check=True
            )
            files = result.stdout.strip().split('\n') if result.stdout.strip() else []
            # Filter for analyzable files, exclude skip list
            return [f for f in files
                   if f and f.endswith(('.py', '.ts', '.tsx', '.js', '.jsx'))
                   and f not in self.SKIP_FILES]
        except subprocess.CalledProcessError:
            return []
    
    def get_file_hash(self, filepath):
        """Quick SHA-256 hash for cache key"""
        try:
            with open(filepath, 'rb') as f:
                return hashlib.sha256(f.read()).hexdigest()
        except FileNotFoundError:
            return None
    
    def load_cache(self):
        """Load analysis cache with TTL check"""
        if not self.CACHE_FILE.exists():
            return {}
        try:
            with open(self.CACHE_FILE) as f:
                cache = json.load(f)
            # Clean expired entries
            now = time.time()
            cache = {k: v for k, v in cache.items() 
                    if now - v.get('timestamp', 0) < self.CACHE_TTL_SECONDS}
            return cache
        except:
            return {}
    
    def save_cache(self, cache):
        """Save cache atomically"""
        temp = self.CACHE_FILE.with_suffix('.tmp')
        try:
            with open(temp, 'w') as f:
                json.dump(cache, f, indent=2)
            temp.replace(self.CACHE_FILE)
        except:
            pass  # Don't fail on cache write errors
    
    def pattern_check(self, filepath, content=None):
        """Simple pattern-based check when AST-GREP not available"""
        if content is None:
            try:
                with open(filepath, 'r', encoding='utf-8') as f:
                    content = f.read()
            except:
                return True, [], []
        
        critical_found = []
        bad_found = []
        
        # Check critical patterns
        for pattern, description in self.CRITICAL_PATTERNS:
            if pattern in content:
                critical_found.append(f"{description}: {pattern}")
        
        # If critical issues found, return immediately
        if critical_found:
            return False, critical_found, []
        
        # Check bad patterns
        for pattern, description in self.BAD_PATTERNS:
            count = content.count(pattern)
            if count > 0:
                bad_found.append({'pattern': pattern, 'description': description, 'count': count})
        
        return True, [], bad_found
    
    def analyze_file(self, filepath):
        """Analyze a single file with caching"""
        # Check cache first
        cache = self.load_cache()
        file_hash = self.get_file_hash(filepath)
        
        if not file_hash:
            return {'file': filepath, 'score': 1.0, 'issues': [], 'cached': False}
        
        cache_key = filepath
        if cache_key in cache and cache[cache_key].get('hash') == file_hash:
            result = cache[cache_key].get('result', {})
            result['cached'] = True
            return result
        
        # Perform analysis
        try:
            with open(filepath, 'r', encoding='utf-8') as f:
                content = f.read()
            
            # Always do pattern check
            secure, critical_issues, bad_patterns = self.pattern_check(filepath, content)
            
            if not secure:
                result = {
                    'file': filepath,
                    'score': 0.0,
                    'critical': True,
                    'issues': critical_issues,
                    'cached': False
                }
            else:
                # Calculate score based on bad patterns
                lines = len(content.splitlines())
                if lines > 0:
                    total_bad = sum(p['count'] for p in bad_patterns)
                    # Scale penalty by file size
                    penalty_per_issue = min(0.02, 5.0 / lines)
                    score = max(0.0, 1.0 - (total_bad * penalty_per_issue))
                else:
                    score = 1.0
                
                result = {
                    'file': filepath,
                    'score': score,
                    'issues': bad_patterns[:5],  # Limit to 5 issues
                    'bad_count': len(bad_patterns),
                    'cached': False
                }
            
            # If AST-GREP available and no critical issues, try deeper analysis
            if HAS_AST_GREP and self.registry and not critical_issues:
                try:
                    ast_result = self._run_ast_grep_analysis(filepath, content)
                    # Use AST-GREP score if it found more issues
                    if ast_result.get('bad_count', 0) > len(bad_patterns):
                        result = ast_result
                        result['cached'] = False
                except:
                    pass  # Keep simple analysis result
            
        except Exception as e:
            # Don't block commits on analysis errors
            result = {
                'file': filepath,
                'score': 1.0,
                'issues': [],
                'error': str(e),
                'cached': False
            }
        
        # Update cache
        cache[cache_key] = {
            'hash': file_hash,
            'result': result,
            'timestamp': time.time()
        }
        self.save_cache(cache)
        
        return result
    
    def _detect_language(self, filepath):
        """Detect language from file extension"""
        ext = Path(filepath).suffix.lower()
        if ext == '.py':
            return 'python'
        elif ext in ['.ts', '.tsx']:
            return 'typescript'
        elif ext in ['.js', '.jsx']:
            return 'javascript'
        return None
    
    def _run_ast_grep_analysis(self, filepath, content):
        """Run AST-GREP analysis on a single file"""
        language = self._detect_language(filepath)
        if not language:
            return {'file': filepath, 'score': 1.0, 'issues': []}
        
        # Map language to ast-grep-py language
        sg_lang_map = {
            'python': sg.Python,
            'typescript': sg.TypeScript,
            'javascript': sg.JavaScript
        }
        
        sg_language = sg_lang_map.get(language)
        if not sg_language:
            return {'file': filepath, 'score': 1.0, 'issues': []}
        
        # Create AST root
        root = sg.SgRoot(content, sg_language)
        node = root.root()
        
        # Get patterns for this language
        patterns = self.registry.get_patterns_by_language(language)
        bad_patterns = [p for p in patterns if p.get('quality') == 'bad']
        
        issues = []
        bad_count = 0
        
        # Check each bad pattern (limited for speed)
        for pattern_def in bad_patterns[:10]:
            pattern_str = pattern_def.get('pattern', '')
            if not pattern_str:
                continue
            
            try:
                matches = node.find_all(pattern=pattern_str)
                if matches:
                    bad_count += len(matches)
                    issues.append({
                        'pattern': pattern_def['id'],
                        'description': pattern_def.get('description', ''),
                        'count': len(matches)
                    })
            except Exception:
                continue
        
        # Calculate quality score
        lines = len(content.splitlines())
        if lines > 0:
            penalty_per_issue = min(0.05, 10.0 / lines)
            score = max(0.0, 1.0 - (bad_count * penalty_per_issue))
        else:
            score = 1.0
        
        return {
            'file': filepath,
            'score': score,
            'issues': issues[:5],
            'bad_count': bad_count
        }
    
    def run(self):
        """Main entry point for pre-commit hook"""
        start_time = time.time()
        
        # Get staged files
        staged_files = self.get_staged_files()
        if not staged_files:
            print("✅ No files to check")
            return 0
        
        print(f"🔍 Checking quality of {len(staged_files)} files...")
        if not HAS_AST_GREP:
            print("   (Using simple pattern analysis - ast-grep-py not installed)")
        
        # Analyze files in parallel
        with ThreadPoolExecutor(max_workers=self.MAX_WORKERS) as executor:
            results = list(executor.map(self.analyze_file, staged_files))
        
        # Check for critical issues first
        critical_issues = [r for r in results if r.get('critical')]
        if critical_issues:
            print("\n❌ CRITICAL SECURITY ISSUES FOUND:")
            for r in critical_issues:
                print(f"  {r['file']}:")
                for issue in r['issues']:
                    print(f"    - {issue}")
            print("\nCommit blocked. Fix these issues before committing.")
            return 1
        
        # Calculate overall score
        valid_results = [r for r in results if 'score' in r]
        if not valid_results:
            print("✅ No analyzable files")
            return 0
        
        total_score = sum(r['score'] for r in valid_results) / len(valid_results)
        
        # Show results
        cached_count = sum(1 for r in results if r.get('cached'))
        elapsed = time.time() - start_time
        
        print(f"\n📊 Quality Score: {total_score:.1%}")
        print(f"⏱️  Analysis time: {elapsed:.2f}s ({cached_count}/{len(results)} from cache)")
        
        # Check threshold
        if total_score < self.MIN_QUALITY_SCORE:
            print(f"\n❌ Quality gate failed: {total_score:.1%} < {self.MIN_QUALITY_SCORE:.0%}")
            print("\nIssues found:")
            
            # Show worst files
            worst_files = sorted(valid_results, key=lambda x: x['score'])[:5]
            for r in worst_files:
                if r['issues']:
                    print(f"\n  {r['file']} (score: {r['score']:.1%}):")
                    for issue in r['issues'][:3]:
                        if isinstance(issue, dict):
                            print(f"    - {issue['description']} ({issue['count']} occurrences)")
                        else:
                            print(f"    - {issue}")
            
            print("\n💡 To bypass: git commit --no-verify")
            print("📝 To see full report: python scripts/ast_grep_final_analyzer.py")
            return 1
        
        # Success - show summary
        if total_score >= 0.90:
            print("🌟 Excellent code quality!")
        elif total_score >= 0.75:
            print("✅ Good code quality")
        else:
            print("✅ Quality gate passed (consider improvements)")
        
        # Show files with issues as warnings
        files_with_issues = [r for r in valid_results if r.get('issues')]
        if files_with_issues and total_score >= self.MIN_QUALITY_SCORE:
            print("\n⚠️  Files with minor issues (not blocking):")
            for r in files_with_issues[:3]:
                print(f"  - {r['file']} ({len(r.get('issues', []))} patterns)")
        
        return 0

if __name__ == '__main__':
    gate = FastQualityGate()
    sys.exit(gate.run())
