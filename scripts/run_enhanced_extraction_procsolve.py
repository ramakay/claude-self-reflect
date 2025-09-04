#!/usr/bin/env python3
"""
Run enhanced pattern extraction on procsolve-website collections
Makes procsolve-website the golden template with comprehensive patterns
"""

import json
import logging
from qdrant_client import QdrantClient
from enhanced_pattern_extractor import EnhancedPatternExtractor
from pattern_propagator import PatternPropagator
from tqdm import tqdm

# Setup logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(message)s')
logger = logging.getLogger(__name__)

def main():
    # Initialize clients
    client = QdrantClient('localhost', port=6333)
    extractor = EnhancedPatternExtractor()
    propagator = PatternPropagator(client)
    
    # Procsolve collections
    collections = [
        'conv_3ce27839_local',  # Main collection (11537 points)
        'conv_9f2f312b_local',  # 2707 points  
        'conv_331439c5_local'   # 150 points
    ]
    
    total_extracted = 0
    total_propagated = 0
    all_patterns_found = {}
    
    for collection_name in collections:
        logger.info(f"\nProcessing collection: {collection_name}")
        
        try:
            # Get collection info
            info = client.get_collection(collection_name)
            total_points = info.points_count
            logger.info(f"  Total points: {total_points}")
            
            # Scroll through all points
            offset = None
            batch_size = 100
            extracted_count = 0
            propagated_count = 0
            
            with tqdm(total=total_points, desc=f"Processing {collection_name}") as pbar:
                while True:
                    # Get batch of points
                    results = client.scroll(
                        collection_name=collection_name,
                        limit=batch_size,
                        offset=offset,
                        with_payload=True
                    )
                    
                    points = results[0]
                    if not points:
                        break
                    
                    # Process each point
                    updates = []
                    source_points = []
                    
                    for point in points:
                        payload = point.payload
                        text = payload.get('text', '')
                        
                        if text:
                            # Extract patterns
                            pattern_data = extractor.extract_from_conversation(text)
                            
                            if pattern_data.get('code_patterns'):
                                updates.append({
                                    'id': point.id,
                                    'payload': {
                                        'code_patterns': pattern_data['code_patterns'],
                                        'pattern_stats': pattern_data.get('pattern_stats', {})
                                    }
                                })
                                extracted_count += 1
                                
                                # Track source points for propagation
                                source_points.append({
                                    'id': point.id,
                                    'patterns': pattern_data['code_patterns']
                                })
                                
                                # Aggregate patterns
                                for category, patterns in pattern_data['code_patterns'].items():
                                    if category not in all_patterns_found:
                                        all_patterns_found[category] = set()
                                    all_patterns_found[category].update(patterns)
                    
                    # Update points with patterns
                    if updates:
                        for update in updates:
                            try:
                                client.set_payload(
                                    collection_name=collection_name,
                                    payload=update['payload'],
                                    points=[update['id']]
                                )
                            except Exception as e:
                                logger.error(f"Failed to update point {update['id']}: {e}")
                    
                    # Skip propagation for now - focus on direct extraction
                    # Pattern propagation can be done in a second pass if needed
                    
                    pbar.update(len(points))
                    
                    # Get next batch
                    offset = results[1]
                    if offset is None:
                        break
            
            logger.info(f"  Extracted patterns from: {extracted_count} points")
            logger.info(f"  Propagated patterns to: {propagated_count} points")
            
            total_extracted += extracted_count
            total_propagated += propagated_count
            
        except Exception as e:
            logger.error(f"Error processing {collection_name}: {e}")
            continue
    
    # Print summary
    logger.info("\n" + "="*60)
    logger.info("EXTRACTION COMPLETE - PROCSOLVE-WEBSITE GOLDEN TEMPLATE")
    logger.info("="*60)
    logger.info(f"Total points with extracted patterns: {total_extracted}")
    logger.info(f"Total points with propagated patterns: {total_propagated}")
    logger.info(f"\nUnique patterns found by category:")
    
    total_unique = 0
    for category in sorted(all_patterns_found.keys()):
        patterns = all_patterns_found[category]
        logger.info(f"  {category}: {len(patterns)} patterns")
        # Show first 5 examples
        examples = list(patterns)[:5]
        for example in examples:
            logger.info(f"    - {example}")
        if len(patterns) > 5:
            logger.info(f"    ... and {len(patterns)-5} more")
        total_unique += len(patterns)
    
    logger.info(f"\nTOTAL UNIQUE PATTERNS: {total_unique}")
    
    # Get extractor stats
    stats = extractor.get_summary()
    logger.info(f"\nExtractor Statistics:")
    logger.info(f"  Catalog patterns available: {stats['catalog_patterns_used']}")
    logger.info(f"  Total pattern matches: {stats['total_patterns_found']}")
    
    # Final verification
    logger.info("\n" + "="*60)
    logger.info("VERIFICATION")
    logger.info("="*60)
    
    for collection_name in collections:
        try:
            # Count points with patterns
            offset = None
            with_patterns = 0
            total = 0
            
            while True:
                results = client.scroll(
                    collection_name=collection_name,
                    limit=100,
                    offset=offset,
                    with_payload=['code_patterns']
                )
                
                points = results[0]
                if not points:
                    break
                    
                for point in points:
                    total += 1
                    if point.payload.get('code_patterns'):
                        with_patterns += 1
                
                offset = results[1]
                if offset is None:
                    break
            
            coverage = (with_patterns/total)*100 if total > 0 else 0
            logger.info(f"{collection_name}: {with_patterns}/{total} points with patterns ({coverage:.1f}% coverage)")
            
        except Exception as e:
            logger.error(f"Verification failed for {collection_name}: {e}")

if __name__ == "__main__":
    main()