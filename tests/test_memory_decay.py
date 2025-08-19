#!/usr/bin/env python3
"""
Memory Decay Feature Tests
Tests time-based decay calculations and score adjustments.
"""

import os
import sys
import math
import json
import asyncio
from pathlib import Path
from datetime import datetime, timedelta
from typing import Dict, List, Any

# Add parent directory to path
sys.path.insert(0, str(Path(__file__).parent.parent))

class TestMemoryDecay:
    """Test suite for memory decay functionality."""
    
    def __init__(self):
        self.test_results = []
        self.half_life_days = 90  # Default half-life
    
    def calculate_decay(self, age_days: float, half_life: float = 90) -> float:
        """Calculate decay factor for a given age."""
        if age_days <= 0:
            return 1.0
        return math.exp(-0.693 * age_days / half_life)
    
    def test_decay_calculations(self):
        """Test decay calculations with various time periods."""
        test_name = "decay_calculations"
        print(f"\n📝 Testing {test_name}...")
        
        test_cases = [
            # (age_days, expected_decay_range)
            (0, (0.99, 1.0)),      # Today - no decay
            (1, (0.99, 1.0)),      # 1 day - minimal decay
            (7, (0.94, 0.96)),     # 1 week
            (30, (0.78, 0.80)),    # 1 month
            (90, (0.49, 0.51)),    # Half-life - should be ~0.5
            (180, (0.24, 0.26)),   # 2x half-life - should be ~0.25
            (365, (0.05, 0.07)),   # 1 year - significant decay
            (730, (0.002, 0.004)), # 2 years - almost zero
        ]
        
        for age_days, (min_expected, max_expected) in test_cases:
            try:
                decay = self.calculate_decay(age_days, self.half_life_days)
                
                assert min_expected <= decay <= max_expected, \
                    f"Decay for {age_days} days should be between {min_expected} and {max_expected}, got {decay}"
                
                self.test_results.append({
                    "test": f"{test_name}_{age_days}d",
                    "status": "✅ PASS",
                    "details": f"Age: {age_days}d, Decay: {decay:.3f}"
                })
                print(f"  ✅ {age_days} days: decay = {decay:.3f}")
                
            except AssertionError as e:
                self.test_results.append({
                    "test": f"{test_name}_{age_days}d",
                    "status": "❌ FAIL",
                    "error": str(e)
                })
                print(f"  ❌ {age_days} days: {e}")
    
    def test_half_life_variations(self):
        """Test different half-life settings."""
        test_name = "half_life_variations"
        print(f"\n📝 Testing {test_name}...")
        
        age_days = 30  # Test with 30-day old content
        half_lives = [
            (7, "aggressive"),     # Very fast decay
            (30, "fast"),          # Fast decay
            (90, "default"),       # Default
            (180, "slow"),         # Slow decay
            (365, "very_slow"),    # Very slow decay
        ]
        
        for half_life, description in half_lives:
            try:
                decay = self.calculate_decay(age_days, half_life)
                
                # Verify decay is within valid range
                assert 0 <= decay <= 1, f"Decay must be between 0 and 1"
                
                self.test_results.append({
                    "test": f"{test_name}_{description}",
                    "status": "✅ PASS",
                    "details": f"Half-life: {half_life}d, 30d decay: {decay:.3f}"
                })
                print(f"  ✅ {description} (HL={half_life}d): 30-day decay = {decay:.3f}")
                
            except AssertionError as e:
                self.test_results.append({
                    "test": f"{test_name}_{description}",
                    "status": "❌ FAIL",
                    "error": str(e)
                })
    
    def test_score_adjustment(self):
        """Test score adjustment with decay."""
        test_name = "score_adjustment"
        print(f"\n📝 Testing {test_name}...")
        
        test_cases = [
            # (original_score, age_days, use_decay, expected_range)
            (0.9, 0, True, (0.89, 0.91)),      # Fresh content, high score
            (0.9, 90, True, (0.44, 0.46)),     # Half-life decay
            (0.9, 0, False, (0.89, 0.91)),     # Decay disabled
            (0.9, 90, False, (0.89, 0.91)),    # Decay disabled, no change
            (0.5, 30, True, (0.39, 0.41)),     # Medium score with decay
            (0.3, 365, True, (0.01, 0.03)),    # Low score, old content
        ]
        
        for original_score, age_days, use_decay, expected_range in test_cases:
            try:
                if use_decay:
                    decay = self.calculate_decay(age_days)
                    adjusted_score = original_score * decay
                else:
                    adjusted_score = original_score
                
                assert expected_range[0] <= adjusted_score <= expected_range[1], \
                    f"Adjusted score should be in range {expected_range}, got {adjusted_score}"
                
                self.test_results.append({
                    "test": f"{test_name}_{original_score}_{age_days}d_{use_decay}",
                    "status": "✅ PASS",
                    "details": f"Original: {original_score}, Adjusted: {adjusted_score:.3f}"
                })
                
            except AssertionError as e:
                self.test_results.append({
                    "test": f"{test_name}_{original_score}_{age_days}d_{use_decay}",
                    "status": "❌ FAIL",
                    "error": str(e)
                })
    
    def test_time_boundaries(self):
        """Test edge cases for time calculations."""
        test_name = "time_boundaries"
        print(f"\n📝 Testing {test_name}...")
        
        edge_cases = [
            ("future", -1),      # Future date
            ("zero", 0),         # Exactly now
            ("microsecond", 0.00001),  # Very recent
            ("max_age", 10000),  # Very old
        ]
        
        for description, age_days in edge_cases:
            try:
                decay = self.calculate_decay(age_days)
                
                # Future dates should have no decay
                if age_days < 0:
                    assert decay == 1.0, "Future dates should have no decay"
                
                # All decay values should be valid
                assert 0 <= decay <= 1, "Decay must be between 0 and 1"
                
                self.test_results.append({
                    "test": f"{test_name}_{description}",
                    "status": "✅ PASS",
                    "details": f"Age: {age_days}d, Decay: {decay:.6f}"
                })
                
            except AssertionError as e:
                self.test_results.append({
                    "test": f"{test_name}_{description}",
                    "status": "❌ FAIL",
                    "error": str(e)
                })
    
    def test_ranking_with_decay(self):
        """Test how decay affects result ranking."""
        test_name = "ranking_with_decay"
        print(f"\n📝 Testing {test_name}...")
        
        # Simulate search results with different ages and scores
        results = [
            {"id": 1, "score": 0.95, "age_days": 1, "content": "Very recent, high relevance"},
            {"id": 2, "score": 0.85, "age_days": 30, "content": "Month old, good relevance"},
            {"id": 3, "score": 0.80, "age_days": 7, "content": "Week old, good relevance"},
            {"id": 4, "score": 0.99, "age_days": 180, "content": "Old but perfect match"},
            {"id": 5, "score": 0.70, "age_days": 0, "content": "Today, medium relevance"},
        ]
        
        # Test with decay enabled
        for r in results:
            r["decay"] = self.calculate_decay(r["age_days"])
            r["adjusted_score"] = r["score"] * r["decay"]
        
        # Sort by adjusted score
        ranked_with_decay = sorted(results, key=lambda x: x["adjusted_score"], reverse=True)
        
        # Without decay (sort by original score)
        ranked_without_decay = sorted(results, key=lambda x: x["score"], reverse=True)
        
        try:
            # Verify decay changes ranking
            with_decay_order = [r["id"] for r in ranked_with_decay]
            without_decay_order = [r["id"] for r in ranked_without_decay]
            
            assert with_decay_order != without_decay_order, \
                "Decay should change result ranking"
            
            # Recent content should rank higher with decay
            assert ranked_with_decay[0]["age_days"] < 30, \
                "Recent content should rank higher with decay"
            
            self.test_results.append({
                "test": test_name,
                "status": "✅ PASS",
                "details": f"With decay: {with_decay_order}, Without: {without_decay_order}"
            })
            
            print(f"  ✅ Ranking with decay: {with_decay_order}")
            print(f"  ✅ Ranking without decay: {without_decay_order}")
            
        except AssertionError as e:
            self.test_results.append({
                "test": test_name,
                "status": "❌ FAIL",
                "error": str(e)
            })
    
    def test_performance_impact(self):
        """Test performance impact of decay calculations."""
        test_name = "performance_impact"
        print(f"\n📝 Testing {test_name}...")
        
        import time
        
        # Simulate processing many results
        num_results = 1000
        results = [
            {"score": 0.5 + i/2000, "age_days": i % 365}
            for i in range(num_results)
        ]
        
        # Time without decay
        start = time.time()
        for r in results:
            _ = r["score"]  # Just access score
        time_without_decay = time.time() - start
        
        # Time with decay
        start = time.time()
        for r in results:
            decay = self.calculate_decay(r["age_days"])
            _ = r["score"] * decay
        time_with_decay = time.time() - start
        
        try:
            # Decay should add minimal overhead (< 100ms for 1000 items)
            overhead = time_with_decay - time_without_decay
            assert overhead < 0.1, f"Decay overhead too high: {overhead:.3f}s"
            
            self.test_results.append({
                "test": test_name,
                "status": "✅ PASS",
                "details": f"Overhead for {num_results} items: {overhead*1000:.1f}ms"
            })
            
            print(f"  ✅ Performance overhead: {overhead*1000:.1f}ms for {num_results} items")
            
        except AssertionError as e:
            self.test_results.append({
                "test": test_name,
                "status": "❌ FAIL",
                "error": str(e)
            })
    
    def test_configuration_persistence(self):
        """Test decay configuration persistence."""
        test_name = "config_persistence"
        print(f"\n📝 Testing {test_name}...")
        
        import tempfile
        
        config = {
            "use_decay": True,
            "half_life_days": 60,
            "min_score_threshold": 0.1
        }
        
        try:
            # Save config
            with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
                json.dump(config, f)
                config_file = f.name
            
            # Load config
            with open(config_file, 'r') as f:
                loaded_config = json.load(f)
            
            assert loaded_config == config, "Config should persist correctly"
            
            # Test with environment variable override
            os.environ["USE_DECAY"] = "0"
            use_decay = os.environ.get("USE_DECAY", "1") != "0"
            assert not use_decay, "Environment variable should override config"
            
            self.test_results.append({
                "test": test_name,
                "status": "✅ PASS"
            })
            
            # Cleanup
            os.unlink(config_file)
            if "USE_DECAY" in os.environ:
                del os.environ["USE_DECAY"]
            
        except Exception as e:
            self.test_results.append({
                "test": test_name,
                "status": "❌ FAIL",
                "error": str(e)
            })
    
    def print_results(self):
        """Print test results summary."""
        print("\n" + "="*60)
        print("📊 MEMORY DECAY TEST RESULTS")
        print("="*60)
        
        passed = sum(1 for r in self.test_results if "PASS" in r["status"])
        failed = sum(1 for r in self.test_results if "FAIL" in r["status"])
        
        print(f"\n✅ Passed: {passed}")
        print(f"❌ Failed: {failed}")
        print(f"📝 Total: {len(self.test_results)}")
        
        if failed > 0:
            print("\n❌ FAILED TESTS:")
            for result in self.test_results:
                if "FAIL" in result["status"]:
                    print(f"  - {result['test']}: {result.get('error', 'Unknown error')}")
        
        print("\n" + "="*60)
        return failed == 0
    
    def run_all_tests(self):
        """Run all memory decay tests."""
        print("🚀 Starting Memory Decay Feature Tests")
        print("="*60)
        
        self.test_decay_calculations()
        self.test_half_life_variations()
        self.test_score_adjustment()
        self.test_time_boundaries()
        self.test_ranking_with_decay()
        self.test_performance_impact()
        self.test_configuration_persistence()
        
        return self.print_results()

def main():
    """Main test runner."""
    tester = TestMemoryDecay()
    success = tester.run_all_tests()
    sys.exit(0 if success else 1)

if __name__ == "__main__":
    main()