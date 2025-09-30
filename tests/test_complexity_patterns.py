#!/usr/bin/env python3
"""Test cases for AST-GREP complexity patterns."""

import unittest
import sys
from pathlib import Path

# Add scripts to path
sys.path.append(str(Path(__file__).parent.parent / "scripts"))

from ast_grep_final_analyzer import FinalASTGrepAnalyzer


class TestComplexityPatterns(unittest.TestCase):
    """Test the newly added complexity patterns detect correctly."""

    def setUp(self):
        """Initialize analyzer for testing."""
        self.analyzer = FinalASTGrepAnalyzer()

    def test_nested_if_detection(self):
        """Test detection of deeply nested if statements."""
        test_code = '''
def complex_function():
    if condition1:
        x = 1
        if condition2:
            y = 2
            if condition3:
                z = 3
                return True
    return False
'''
        # Create temp file
        test_file = Path("/tmp/test_nested_if.py")
        test_file.write_text(test_code)

        # Analyze
        result = self.analyzer.analyze_file(str(test_file))

        # Check for nested-if pattern detection
        patterns_found = [m['id'] for m in result['all_matches']]
        self.assertIn('nested-if-depth-3', patterns_found)

    def test_complex_condition_detection(self):
        """Test detection of complex conditions with 4+ parts."""
        test_code = '''
def check_multiple():
    if a > 0 and b < 10 and c == 5 and d != 0:
        return True
    return False
'''
        test_file = Path("/tmp/test_complex_condition.py")
        test_file.write_text(test_code)

        result = self.analyzer.analyze_file(str(test_file))
        patterns_found = [m['id'] for m in result['all_matches']]
        self.assertIn('complex-condition', patterns_found)

    def test_nested_loops_detection(self):
        """Test detection of nested loops."""
        test_code = '''
def process_matrix():
    for i in range(10):
        print(i)
        for j in range(10):
            print(i, j)
            process(i, j)
'''
        test_file = Path("/tmp/test_nested_loops.py")
        test_file.write_text(test_code)

        result = self.analyzer.analyze_file(str(test_file))
        patterns_found = [m['id'] for m in result['all_matches']]
        self.assertIn('nested-loops', patterns_found)

    def test_callback_hell_typescript(self):
        """Test detection of callback hell in TypeScript."""
        test_code = '''
fetchData((data) => {
    processData((result) => {
        saveData((saved) => {
            notifyUser(saved);
        })
    })
})
'''
        test_file = Path("/tmp/test_callback.ts")
        test_file.write_text(test_code)

        result = self.analyzer.analyze_file(str(test_file))
        patterns_found = [m['id'] for m in result['all_matches']]
        # Should detect callback-hell pattern
        self.assertIn('callback-hell', patterns_found)

    def test_quality_score_impact(self):
        """Test that complexity patterns correctly impact quality score."""
        # Clean code
        clean_code = '''
def simple_function():
    """A simple function with good practices."""
    result = calculate_value()
    return result
'''

        # Complex code
        complex_code = '''
def bad_function():
    print("Debug")
    if a:
        if b:
            if c:
                if d:
                    for i in range(10):
                        for j in range(10):
                            print(i, j)
    try:
        x = 1/0
    except:
        pass
'''

        # Test clean code
        test_file = Path("/tmp/test_quality.py")
        test_file.write_text(clean_code)
        clean_result = self.analyzer.analyze_file(str(test_file))
        clean_score = clean_result['quality_metrics']['quality_score']

        # Test complex code
        test_file.write_text(complex_code)
        complex_result = self.analyzer.analyze_file(str(test_file))
        complex_score = complex_result['quality_metrics']['quality_score']

        # Complex code should have significantly lower score
        self.assertGreater(clean_score, complex_score)
        self.assertGreater(clean_score - complex_score, 0.2)  # At least 20% difference


if __name__ == '__main__':
    unittest.main()