#!/usr/bin/env python3
"""
Check AST-GREP enrichment coverage in Qdrant collections.
"""

import asyncio
import os
from qdrant_client import AsyncQdrantClient
from typing import Dict, Any
import json

async def check_ast_grep_coverage():
    """Check how much data in Qdrant contains AST-GREP patterns."""

    # Connect to Qdrant
    client = AsyncQdrantClient(
        url=os.getenv("QDRANT_URL", "http://localhost:6333"),
        api_key=os.getenv("QDRANT_API_KEY")
    )

    stats = {
        "collections": [],
        "total_points": 0,
        "points_with_ast": 0,
        "points_with_patterns": 0,
        "detailed_breakdown": {}
    }

    try:
        # Get all collections
        collections = await client.get_collections()

        for collection in collections.collections:
            collection_name = collection.name
            if not collection_name.startswith("csr_"):
                continue

            print(f"\nAnalyzing collection: {collection_name}")

            # Get collection info
            info = await client.get_collection(collection_name)
            total_points = info.points_count
            stats["total_points"] += total_points

            # Sample points to check for AST-GREP enrichment
            # Check for ast_elements field
            scroll_result = await client.scroll(
                collection_name=collection_name,
                limit=100,  # Sample size
                with_payload=True,
                with_vectors=False
            )

            collection_stats = {
                "name": collection_name,
                "total": total_points,
                "sampled": len(scroll_result[0]),
                "with_ast": 0,
                "with_patterns": 0,
                "pattern_types": set()
            }

            for point in scroll_result[0]:
                payload = point.payload

                # Check for AST elements
                if "ast_elements" in payload and payload["ast_elements"]:
                    collection_stats["with_ast"] += 1
                    stats["points_with_ast"] += 1

                # Check for pattern analysis
                if "pattern_analysis" in payload:
                    collection_stats["with_patterns"] += 1
                    stats["points_with_patterns"] += 1

                    # Track pattern types found
                    if isinstance(payload["pattern_analysis"], dict):
                        for category in payload["pattern_analysis"].keys():
                            collection_stats["pattern_types"].add(category)

                # Check for quality scores
                if "quality_score" in payload:
                    if "quality_score_range" not in collection_stats:
                        collection_stats["quality_score_range"] = {
                            "min": payload["quality_score"],
                            "max": payload["quality_score"]
                        }
                    else:
                        collection_stats["quality_score_range"]["min"] = min(
                            collection_stats["quality_score_range"]["min"],
                            payload["quality_score"]
                        )
                        collection_stats["quality_score_range"]["max"] = max(
                            collection_stats["quality_score_range"]["max"],
                            payload["quality_score"]
                        )

            # Calculate percentages
            if collection_stats["sampled"] > 0:
                collection_stats["ast_percentage"] = (
                    collection_stats["with_ast"] / collection_stats["sampled"] * 100
                )
                collection_stats["pattern_percentage"] = (
                    collection_stats["with_patterns"] / collection_stats["sampled"] * 100
                )

            # Convert set to list for JSON serialization
            collection_stats["pattern_types"] = list(collection_stats["pattern_types"])

            stats["collections"].append(collection_stats)
            stats["detailed_breakdown"][collection_name] = collection_stats

            print(f"  Total points: {total_points}")
            print(f"  Sampled: {collection_stats['sampled']}")
            print(f"  With AST elements: {collection_stats['with_ast']} ({collection_stats.get('ast_percentage', 0):.1f}%)")
            print(f"  With patterns: {collection_stats['with_patterns']} ({collection_stats.get('pattern_percentage', 0):.1f}%)")
            if collection_stats["pattern_types"]:
                print(f"  Pattern types: {', '.join(collection_stats['pattern_types'])}")
            if "quality_score_range" in collection_stats:
                print(f"  Quality score range: {collection_stats['quality_score_range']['min']:.1f} - {collection_stats['quality_score_range']['max']:.1f}")

    except Exception as e:
        print(f"Error analyzing Qdrant: {e}")
        return stats

    finally:
        await client.close()

    # Calculate overall percentages
    if stats["total_points"] > 0:
        total_sampled = sum(c["sampled"] for c in stats["collections"])
        if total_sampled > 0:
            stats["overall_ast_percentage"] = stats["points_with_ast"] / total_sampled * 100
            stats["overall_pattern_percentage"] = stats["points_with_patterns"] / total_sampled * 100

    return stats


async def main():
    """Main entry point."""
    print("=== AST-GREP Coverage Analysis ===")
    print("Checking Qdrant collections for AST-GREP enrichment...\n")

    stats = await check_ast_grep_coverage()

    print("\n=== Summary ===")
    print(f"Total collections analyzed: {len(stats['collections'])}")
    print(f"Total points in all collections: {stats['total_points']}")
    print(f"Overall AST coverage: {stats.get('overall_ast_percentage', 0):.1f}%")
    print(f"Overall pattern coverage: {stats.get('overall_pattern_percentage', 0):.1f}%")

    # Save detailed report
    # Use relative path from script location
    script_dir = Path(__file__).parent
    report_path = script_dir.parent / "docs" / "analysis" / "ast-grep-coverage-report.json"
    os.makedirs(os.path.dirname(report_path), exist_ok=True)

    with open(report_path, "w") as f:
        json.dump(stats, f, indent=2)

    print(f"\nDetailed report saved to: {report_path}")


if __name__ == "__main__":
    asyncio.run(main())