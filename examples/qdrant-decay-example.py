"""
Example of how to use Qdrant's built-in decay functions instead of client-side calculation
"""

from qdrant_client.models import (
    Query, Prefetch, Formula, 
    ExpDecayExpression, DecayParamsExpression,
    Expression, SearchRequest
)

# Current client-side approach (what we're doing now)
def current_approach():
    # 1. Search without decay
    results = await qdrant_client.search(
        collection_name=collection_name,
        query_vector=query_embedding,
        limit=limit * 3,  # Get extra results
        with_payload=True
    )
    
    # 2. Calculate decay manually in Python
    for point in results:
        age_ms = calculate_age(point.payload['timestamp'])
        decay_factor = np.exp(-age_ms / scale_ms)
        adjusted_score = point.score + (DECAY_WEIGHT * decay_factor)

# Better approach using Qdrant's built-in decay
def qdrant_native_decay():
    # Use Qdrant's Formula with exp_decay
    search_request = {
        "vector": query_embedding,
        "limit": limit,
        "with_payload": True,
        "params": {
            "quantization": {"ignore": False, "rescore": True}
        },
        # Apply decay formula server-side
        "formula": {
            "sum": [
                {"variable": "score"},  # Original similarity score
                {
                    "mult": [
                        {"constant": DECAY_WEIGHT},  # Weight multiplier
                        {
                            "exp_decay": {
                                "x": {"datetime_key": "timestamp"},  # Use timestamp field
                                "target": {"datetime": "now"},  # Decay from current time
                                "scale": DECAY_SCALE_DAYS * 24 * 60 * 60 * 1000,  # Scale in ms
                                "midpoint": 0.5  # Standard exponential decay
                            }
                        }
                    ]
                }
            ]
        }
    }
    
    # Or using the Python client with proper models:
    results = await qdrant_client.query_points(
        collection_name=collection_name,
        query=Query(
            nearest=query_embedding,
            formula=Formula(
                sum=[
                    Expression(variable="score"),
                    Expression(
                        mult=MultExpression(
                            mult=[
                                Expression(constant=DECAY_WEIGHT),
                                Expression(
                                    exp_decay=DecayParamsExpression(
                                        x=Expression(datetime_key="timestamp"),
                                        target=Expression(datetime="now"),
                                        scale=DECAY_SCALE_DAYS * 24 * 60 * 60 * 1000,
                                        midpoint=0.5
                                    )
                                )
                            ]
                        )
                    )
                ]
            )
        ),
        limit=limit,
        score_threshold=min_score
    )

# Benefits of Qdrant's native decay:
# 1. Server-side calculation - more efficient
# 2. Works with score_threshold properly
# 3. No need to fetch extra results
# 4. Consistent with Qdrant's scoring system
# 5. Can combine with other Qdrant features like prefetch