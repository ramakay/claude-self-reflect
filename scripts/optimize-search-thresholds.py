#!/usr/bin/env python3
"""
Search Threshold Optimization Script

Finds optimal similarity thresholds for different embedding models
by testing various thresholds against known relevant content.
"""

import os
import sys
from pathlib import Path
from typing import List, Dict, Any, Tuple

# Add the project root to Python path for imports
project_root = Path(__file__).parent.parent
sys.path.insert(0, str(project_root / "mcp-server" / "src"))

from qdrant_client import QdrantClient

# Optimization configuration
OPTIMIZATION_CONFIG = {
    "target_project": "conv_7f6df0fc",  # claude-self-reflect
    "test_queries": [
        "cererbras",
        "Cerebras",
        "openrouter", 
        "claude code router",
        "Qwen models",
        "using other LLMs"
    ],
    "embedding_models": {
        "voyage": {
            "dimensions": 1024,
            "threshold_range": [0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9],
            "optimal_baseline": 0.7
        },
        "local": {
            "dimensions": 384,
            "threshold_range": [0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8],
            "optimal_baseline": 0.5  # Local embeddings typically need lower thresholds
        }
    },
    "search_limits": [5, 10, 20, 50]
}

class ThresholdOptimizer:
    def __init__(self):
        self.client = QdrantClient(url="http://localhost:6333")
        self.voyage_client = None
        self.fastembed_model = None
        
    def setup_embedding_clients(self):
        """Initialize embedding clients"""
        try:
            import voyageai
            voyage_key = os.getenv('VOYAGE_KEY')
            if voyage_key:
                self.voyage_client = voyageai.Client(api_key=voyage_key)
                print(" Voyage AI client initialized")
            else:
                print("  VOYAGE_KEY not found - Voyage optimization will be skipped")
        except ImportError:
            print("  voyageai not installed - Voyage optimization will be skipped")
        
        try:
            from fastembed import TextEmbedding
            self.fastembed_model = TextEmbedding(model_name="all-MiniLM-L6-v2")
            print(" FastEmbed client initialized")
        except ImportError:
            print("  fastembed not installed - Local optimization will be skipped")
    
    def get_embedding(self, text: str, model_type: str) -> List[float]:
        """Get embedding for text using specified model"""
        if model_type == "voyage" and self.voyage_client:
            response = self.voyage_client.embed([text], model="voyage-3-large")
            return response.embeddings[0]
        elif model_type == "local" and self.fastembed_model:
            embeddings = list(self.fastembed_model.embed([text]))
            return embeddings[0].tolist()
        else:
            raise ValueError(f"Embedding model {model_type} not available")
    
    def test_threshold_performance(self, model_type: str, threshold: float, limit: int) -> Dict[str, Any]:
        """Test performance of a specific threshold"""
        collection_name = f"{OPTIMIZATION_CONFIG['target_project']}_{model_type}"
        
        # Check if collection exists
        try:
            collection_info = self.client.get_collection(collection_name)
            if collection_info.points_count == 0:
                return {"error": "Collection empty"}
        except:
            return {"error": "Collection not found"}
        
        results = {
            "threshold": threshold,
            "limit": limit,
            "queries": {},
            "total_results": 0,
            "avg_score": 0,
            "score_variance": 0
        }
        
        all_scores = []
        
        for query in OPTIMIZATION_CONFIG["test_queries"]:
            try:
                # Get embedding
                query_vector = self.get_embedding(query, model_type)
                
                # Search
                search_results = self.client.search(
                    collection_name=collection_name,
                    query_vector=query_vector,
                    limit=limit,
                    score_threshold=threshold,
                    with_payload=True
                )
                
                query_scores = [r.score for r in search_results]
                all_scores.extend(query_scores)
                
                results["queries"][query] = {
                    "result_count": len(search_results),
                    "top_score": max(query_scores) if query_scores else 0,
                    "avg_score": sum(query_scores) / len(query_scores) if query_scores else 0,
                    "has_results": len(search_results) > 0
                }
                
            except Exception as e:
                results["queries"][query] = {"error": str(e)}
        
        # Calculate overall metrics
        results["total_results"] = len(all_scores)
        results["avg_score"] = sum(all_scores) / len(all_scores) if all_scores else 0
        
        if all_scores:
            mean = results["avg_score"]
            variance = sum((x - mean) ** 2 for x in all_scores) / len(all_scores)
            results["score_variance"] = variance
        
        # Calculate success metrics
        successful_queries = sum(1 for q in results["queries"].values() 
                               if isinstance(q, dict) and q.get("has_results", False))
        results["query_success_rate"] = successful_queries / len(OPTIMIZATION_CONFIG["test_queries"])
        
        return results
    
    def optimize_model_thresholds(self, model_type: str) -> Dict[str, Any]:
        """Find optimal thresholds for a specific model"""
        print(f"\n=' Optimizing thresholds for {model_type.upper()} model...")
        
        model_config = OPTIMIZATION_CONFIG["embedding_models"][model_type]
        optimization_results = {
            "model": model_type,
            "dimensions": model_config["dimensions"],
            "baseline_threshold": model_config["optimal_baseline"],
            "threshold_tests": {},
            "recommendations": {}
        }
        
        best_overall = {"threshold": None, "limit": None, "score": 0, "metrics": None}
        
        for threshold in model_config["threshold_range"]:
            optimization_results["threshold_tests"][threshold] = {}
            
            for limit in OPTIMIZATION_CONFIG["search_limits"]:
                print(f"   Testing threshold {threshold} with limit {limit}...")
                
                test_result = self.test_threshold_performance(model_type, threshold, limit)
                optimization_results["threshold_tests"][threshold][limit] = test_result
                
                if "error" not in test_result:
                    # Calculate combined score (success rate + average score + result count)
                    combined_score = (
                        test_result["query_success_rate"] * 0.5 +  # 50% weight on success rate
                        (test_result["avg_score"] if test_result["avg_score"] else 0) * 0.3 +  # 30% weight on score quality
                        min(test_result["total_results"] / 100, 1.0) * 0.2  # 20% weight on result quantity (capped)
                    )
                    
                    if combined_score > best_overall["score"]:
                        best_overall.update({
                            "threshold": threshold,
                            "limit": limit, 
                            "score": combined_score,
                            "metrics": test_result
                        })
        
        # Generate recommendations
        if best_overall["threshold"]:
            optimization_results["recommendations"] = {
                "optimal_threshold": best_overall["threshold"],
                "optimal_limit": best_overall["limit"],
                "combined_score": best_overall["score"],
                "expected_success_rate": best_overall["metrics"]["query_success_rate"],
                "expected_avg_score": best_overall["metrics"]["avg_score"],
                "vs_baseline": {
                    "threshold_change": best_overall["threshold"] - model_config["optimal_baseline"],
                    "improvement": "higher" if best_overall["threshold"] > model_config["optimal_baseline"] else "lower"
                }
            }
        else:
            optimization_results["recommendations"] = {"error": "No successful configurations found"}
        
        return optimization_results
    
    def run_optimization(self) -> Dict[str, Any]:
        """Run complete threshold optimization"""
        print("=€ SEARCH THRESHOLD OPTIMIZATION")
        print("=" * 50)
        
        self.setup_embedding_clients()
        
        results = {
            "config": OPTIMIZATION_CONFIG,
            "models": {},
            "summary": {}
        }
        
        # Test each model
        for model_type in OPTIMIZATION_CONFIG["embedding_models"].keys():
            # Skip if client not available
            if (model_type == "voyage" and not self.voyage_client) or \
               (model_type == "local" and not self.fastembed_model):
                print(f"  Skipping {model_type} - client not available")
                continue
            
            try:
                model_results = self.optimize_model_thresholds(model_type)
                results["models"][model_type] = model_results
            except Exception as e:
                print(f"L Error optimizing {model_type}: {e}")
                results["models"][model_type] = {"error": str(e)}
        
        # Generate summary
        results["summary"] = self.generate_summary(results["models"])
        
        return results
    
    def generate_summary(self, model_results: Dict[str, Any]) -> Dict[str, Any]:
        """Generate optimization summary"""
        summary = {
            "tested_models": list(model_results.keys()),
            "recommendations_by_model": {},
            "comparison": {}
        }
        
        for model_type, model_data in model_results.items():
            if "error" in model_data:
                summary["recommendations_by_model"][model_type] = {"error": model_data["error"]}
                continue
                
            recommendations = model_data.get("recommendations", {})
            if "error" not in recommendations:
                summary["recommendations_by_model"][model_type] = {
                    "threshold": recommendations["optimal_threshold"],
                    "limit": recommendations["optimal_limit"],
                    "success_rate": f"{recommendations['expected_success_rate']:.1%}",
                    "avg_score": f"{recommendations['expected_avg_score']:.3f}",
                    "vs_baseline": recommendations["vs_baseline"]["improvement"]
                }
            else:
                summary["recommendations_by_model"][model_type] = recommendations
        
        return summary
    
    def print_report(self, results: Dict[str, Any]):
        """Print optimization report"""
        print("\n" + "=" * 60)
        print("THRESHOLD OPTIMIZATION REPORT")
        print("=" * 60)
        
        # Summary
        summary = results["summary"]
        print(f"\n=Ê TESTED MODELS: {', '.join(summary['tested_models'])}")
        
        print(f"\n<¯ RECOMMENDATIONS:")
        for model_type, rec in summary["recommendations_by_model"].items():
            if "error" in rec:
                print(f"   {model_type.upper()}: L {rec['error']}")
            else:
                threshold = rec["threshold"]
                limit = rec["limit"]
                success_rate = rec["success_rate"]
                avg_score = rec["avg_score"]
                vs_baseline = rec["vs_baseline"]
                
                print(f"   {model_type.upper()}:")
                print(f"     Optimal threshold: {threshold}")
                print(f"     Optimal limit: {limit}")
                print(f"     Success rate: {success_rate}")
                print(f"     Avg score: {avg_score}")
                print(f"     Vs baseline: {vs_baseline}")
        
        print(f"\n=¡ IMPLEMENTATION:")
        print("   Update these settings in your configuration:")
        for model_type, rec in summary["recommendations_by_model"].items():
            if "error" not in rec:
                print(f"   - {model_type.upper()}_SIMILARITY_THRESHOLD={rec['threshold']}")
                print(f"   - {model_type.upper()}_SEARCH_LIMIT={rec['limit']}")


def main():
    """Main optimization execution"""
    if len(sys.argv) > 1 and sys.argv[1] == "--help":
        print(__doc__)
        print("\nUsage:")
        print("  python optimize-search-thresholds.py           # Run optimization")
        return
    
    optimizer = ThresholdOptimizer()
    results = optimizer.run_optimization()
    
    # Print report
    optimizer.print_report(results)
    
    # Save results
    with open("threshold_optimization_results.json", "w") as f:
        import json
        json.dump(results, f, indent=2)
    
    print(f"\n=Ä Full results saved to: threshold_optimization_results.json")


if __name__ == "__main__":
    main()