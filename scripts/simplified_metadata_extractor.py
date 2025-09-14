#!/usr/bin/env python3
"""
Simplified Metadata Extractor for Claude Self-Reflect
Focuses on extracting actual VALUES and specific operations that provide discrimination
Enhanced with AST-GREP pattern extraction for code quality tracking
"""

import re
import json
from typing import Dict, List, Any, Set
import logging
from pathlib import Path
import sys

# Add parent for pattern imports
sys.path.append(str(Path(__file__).parent))
try:
    from pattern_registry_enhanced import extract_enhanced_patterns
    PATTERN_EXTRACTION_ENABLED = True
except ImportError:
    PATTERN_EXTRACTION_ENABLED = False

logger = logging.getLogger(__name__)

class SimplifiedMetadataExtractor:
    """Extract specific values and operations for better discrimination."""

    def extract_metadata(self, code: str) -> Dict[str, Any]:
        """Extract metadata focusing on specific values rather than patterns."""
        metadata = {
            "tools_defined": [],      # Actual tool names
            "collections_used": [],   # Actual collection names
            "models_used": [],        # Actual model names
            "search_params": {},      # Actual search parameters
            "operations": [],         # Specific operations performed
            "config_values": {},      # Configuration values
            "unique_identifiers": []  # Unique strings that identify functionality
        }

        # Extract MCP tool names
        tool_matches = re.findall(r'@(?:mcp\.tool|server\.tool).*?async\s+def\s+(\w+)', code, re.DOTALL)
        metadata["tools_defined"] = list(set(tool_matches))

        # Extract collection names (both literal and variable)
        collection_patterns = [
            r'collection_name\s*=\s*["\']([^"\']+)["\']',
            r'collections?\s*=\s*\[([^\]]+)\]',
            r'conv_[a-f0-9]{8}(?:_local|_voyage)?',
            r'reflections?_(?:local|voyage)'
        ]
        for pattern in collection_patterns:
            matches = re.findall(pattern, code)
            if matches:
                if isinstance(matches[0], str) and '[' not in matches[0]:
                    metadata["collections_used"].extend(matches)

        # Extract model names
        model_patterns = [
            r'model_name\s*=\s*["\']([^"\']+)["\']',
            r'model\s*=\s*["\']([^"\']+)["\']',
            r'all-MiniLM-L6-v2',
            r'voyage-(?:large-2|code-2|3)',
            r'text-embedding-ada-002'
        ]
        for pattern in model_patterns:
            matches = re.findall(pattern, code)
            metadata["models_used"].extend(matches)

        # Extract specific search parameters
        param_patterns = {
            "limit": r'limit\s*=\s*(\d+)',
            "min_score": r'min_score\s*=\s*([\d.]+)',
            "use_decay": r'use_decay\s*=\s*(True|False|1|0)',
            "brief": r'brief\s*=\s*(True|False)',
            "mode": r'mode\s*=\s*["\'](\w+)["\']'
        }
        for param_name, pattern in param_patterns.items():
            matches = re.findall(pattern, code)
            if matches:
                metadata["search_params"][param_name] = matches[0]

        # Extract specific operations
        operation_patterns = [
            (r'qdrant_client\.search', 'qdrant_search'),
            (r'qdrant_client\.upsert', 'qdrant_upsert'),
            (r'qdrant_client\.create_collection', 'create_collection'),
            (r'qdrant_client\.get_collections', 'get_collections'),
            (r'collection\.search', 'collection_search'),
            (r'asyncio\.gather', 'parallel_execution'),
            (r'apply_time_decay', 'time_decay'),
            (r'store_reflection', 'store_reflection'),
            (r'reflect_on_past', 'reflect_on_past')
        ]

        for pattern, op_name in operation_patterns:
            if re.search(pattern, code):
                metadata["operations"].append(op_name)

        # Extract configuration values
        config_patterns = {
            "embedding_size": r'size\s*=\s*(\d+)',
            "distance": r'distance\s*=\s*Distance\.(\w+)',
            "qdrant_url": r'QDRANT_URL.*?["\']([^"\']+)["\']',
            "voyage_key": r'VOYAGE_(?:API_)?KEY',
            "collection_prefix": r'COLLECTION_PREFIX.*?["\']([^"\']+)["\']'
        }

        for config_name, pattern in config_patterns.items():
            matches = re.findall(pattern, code)
            if matches:
                metadata["config_values"][config_name] = matches[0]

        # Extract unique identifiers (specific strings that identify functionality)
        unique_patterns = [
            r'sessionId.*?["\']([a-f0-9-]{36})["\']',
            r'conversation_id.*?["\']([a-f0-9-]{36})["\']',
            r'["\']cid["\']:\s*["\']([^"\']+)["\']',
            r'project.*?["\']([^"\']+)["\']'
        ]

        for pattern in unique_patterns:
            matches = re.findall(pattern, code)
            metadata["unique_identifiers"].extend(matches[:3])  # Limit to avoid too many

        # NEW: Add pattern extraction if available
        if PATTERN_EXTRACTION_ENABLED:
            try:
                pattern_metadata = extract_enhanced_patterns(code)
                metadata["patterns"] = pattern_metadata.get("patterns", [])
                metadata["pattern_categories"] = pattern_metadata.get("pattern_categories", [])
                metadata["quality_score"] = pattern_metadata.get("quality_score", 0.5)
                metadata["quality_level"] = pattern_metadata.get("quality_level", "unknown")
                metadata["anti_pattern_count"] = pattern_metadata.get("anti_pattern_count", 0)
                metadata["critical_issues"] = pattern_metadata.get("critical_issues", 0)
            except Exception as e:
                logger.debug(f"Pattern extraction failed: {e}")
                # Add default values if pattern extraction fails
                metadata["quality_score"] = 0.5
                metadata["quality_level"] = "unknown"

        # Clean up lists
        metadata["tools_defined"] = list(set(metadata["tools_defined"]))[:10]
        metadata["collections_used"] = list(set(metadata["collections_used"]))[:10]
        metadata["models_used"] = list(set(metadata["models_used"]))[:5]
        metadata["operations"] = list(set(metadata["operations"]))[:20]
        metadata["unique_identifiers"] = list(set(metadata["unique_identifiers"]))[:5]

        # Calculate value score (how much specific value this provides)
        value_score = 0
        value_score += len(metadata["tools_defined"]) * 2
        value_score += len(metadata["collections_used"]) * 3
        value_score += len(metadata["models_used"]) * 2
        value_score += len(metadata["search_params"]) * 1
        value_score += len(metadata["operations"]) * 1
        value_score += len(metadata["config_values"]) * 1
        value_score += len(metadata["unique_identifiers"]) * 2

        # Add pattern quality to value score if available
        if PATTERN_EXTRACTION_ENABLED and "quality_score" in metadata:
            value_score += int(metadata["quality_score"] * 10)

        metadata["value_score"] = value_score

        return metadata


# Create singleton instance
_extractor = SimplifiedMetadataExtractor()

def extract_simplified_metadata(code: str) -> Dict[str, Any]:
    """Extract simplified value-based metadata from code."""
    return _extractor.extract_metadata(code)