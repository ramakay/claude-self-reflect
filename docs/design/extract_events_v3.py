#!/usr/bin/env python3
"""
Smart Event Extraction V3 - Opus-Validated Approach

Based on Opus 4.1 analysis:
1. Single-vector approach: 500-token search index + 1000-token context cache
2. Fixed scoring: Requests=10, Edits=9, Blocking errors=9, Build=7, Test fails=6
3. Edit patterns instead of raw changes
4. Conversation signature metadata for filtering
5. Request-response pairing
"""

import json
import re
from typing import Dict, List, Any, Tuple, Optional
from datetime import datetime
from collections import defaultdict


def get_message_data(msg: Dict) -> Dict:
    """Extract message data handling nested structure."""
    return msg.get("message", msg)


def calculate_importance_score_v3(msg: Dict, index: int, total: int) -> float:
    """
    Score message importance (OPUS-VALIDATED WEIGHTS).

    Priority hierarchy:
    1. User requests (10pts) - The "what"
    2. Successful edits (9pts) - The "how"
    3. Blocking errors (9pts) - Critical learning moments
    4. Build success (7pts) - Validation
    5. Test failures (6pts) - Negative signals
    6. Code reads (3pts) - Intermediate steps
    """
    score = 0.0
    msg_data = get_message_data(msg)
    content = json.dumps(msg_data.get("content", "")).lower()

    # User requests (the problem) - HIGHEST PRIORITY
    if msg_data.get("role") == "user":
        # Filter out tool_result noise
        if not any(x in content for x in ["tool_result", "tool_use_id", "is_error"]):
            if len(content) > 50:
                score += 10.0

    # Successful edits (the solution)
    if isinstance(msg_data.get("content"), list):
        for item in msg_data.get("content", []):
            if isinstance(item, dict) and item.get("type") == "tool_use":
                tool_name = str(item.get("name", "")).lower()
                if "edit" in tool_name or "write" in tool_name:
                    if "todo" not in tool_name:  # Exclude TodoWrite
                        score += 9.0
                elif "bash" in tool_name:
                    score += 5.0
                elif "read" in tool_name:
                    score += 3.0

    # Blocking errors (critical learning moments)
    error_keywords = ["error", "exception", "traceback", "failed", "failure", "err_"]
    if any(kw in content for kw in error_keywords):
        # Check if this is a blocking error (not resolved quickly)
        score += 9.0

    # Build/test success
    if "compiled successfully" in content or "build" in content and "success" in content:
        score += 7.0

    # Test failures
    if "test" in content and ("failed" in content or "error" in content):
        score += 6.0

    # Solution indicators
    solution_keywords = ["fixed", "solved", "working", "success", "completed"]
    if any(kw in content for kw in solution_keywords):
        score += 8.0

    # Position bias - beginnings and ends often important
    relative_pos = index / max(total, 1)
    if relative_pos < 0.1 or relative_pos > 0.8:
        score *= 1.1  # Smaller boost (was 1.2)

    return score


def extract_edit_pattern(messages: List[Dict], edit_index: int) -> Dict:
    """
    Extract edit as a reusable pattern, not just raw changes.

    Opus recommendation: "Describe the reusable pattern, not specific implementation"
    """
    msg_data = get_message_data(messages[edit_index])

    pattern = {
        "index": edit_index,
        "file": "unknown",
        "operation_type": "unknown",
        "pattern_description": "",
        "why": "Unknown"
    }

    # Find the tool_use
    content = msg_data.get("content", [])
    if not isinstance(content, list):
        return pattern

    for item in content:
        if not isinstance(item, dict) or item.get("type") != "tool_use":
            continue

        tool_name = item.get("name", "")
        if "edit" not in tool_name.lower() and "write" not in tool_name.lower():
            continue

        if "todo" in tool_name.lower():
            continue

        # Extract file
        tool_input = item.get("input", {})
        pattern["file"] = tool_input.get("file_path", "unknown")

        # Determine operation type from edits
        if tool_name == "MultiEdit":
            edits = tool_input.get("edits", [])
            edit_count = len(edits)

            # Analyze edit patterns
            if edit_count > 5:
                pattern["operation_type"] = "cascade_updates"
                pattern["pattern_description"] = f"Batch operation: {edit_count} coordinated changes"
            elif any("remove" in str(e).lower() or "delete" in str(e).lower() for e in edits):
                pattern["operation_type"] = "removal"
                pattern["pattern_description"] = "Item removal with cascade cleanup"
            else:
                pattern["operation_type"] = "refactor"
                pattern["pattern_description"] = f"Multi-point refactoring ({edit_count} changes)"

        elif tool_name == "Edit":
            old_str = tool_input.get("old_string", "")
            new_str = tool_input.get("new_string", "")

            if len(new_str) > len(old_str) * 2:
                pattern["operation_type"] = "expansion"
                pattern["pattern_description"] = "Code expansion/feature addition"
            elif len(new_str) < len(old_str) * 0.5:
                pattern["operation_type"] = "removal"
                pattern["pattern_description"] = "Code removal/simplification"
            else:
                pattern["operation_type"] = "modification"
                pattern["pattern_description"] = "In-place modification"

        elif tool_name == "Write":
            pattern["operation_type"] = "creation"
            pattern["pattern_description"] = "New file creation"

        # Find WHY (look at recent user messages)
        for j in range(max(0, edit_index-5), edit_index):
            check_data = get_message_data(messages[j])
            if check_data.get("role") == "user":
                content_str = str(check_data.get("content", ""))
                if len(content_str) > 50 and "tool_result" not in content_str:
                    pattern["why"] = content_str[:150]
                    break

        return pattern

    return pattern


def build_conversation_signature(messages: List[Dict], errors: List[Dict], patterns: List[Dict], metadata: Optional[Dict] = None) -> Dict:
    """
    Create conversation signature for filtering/search.

    Opus recommendation: Enable searching "all successful React fixes" vs "debugging sessions"
    """
    msg_data_list = [get_message_data(m) for m in messages]

    # Detect completion status
    last_10 = msg_data_list[-10:]
    has_build_success = any(
        "compiled successfully" in str(m.get("content", "")).lower() or
        ("build" in str(m.get("content", "")).lower() and "success" in str(m.get("content", "")).lower())
        for m in last_10
    )
    has_test_success = any("test" in str(m.get("content", "")).lower() and "pass" in str(m.get("content", "")).lower() for m in last_10)
    has_deployment = any("deploy" in str(m.get("content", "")).lower() and "success" in str(m.get("content", "")).lower() for m in last_10)

    # Check for explicit success confirmation in final messages
    has_completion_confirmation = any(
        "all tasks completed" in str(m.get("content", "")).lower() or
        "successfully" in str(m.get("content", "")).lower() and ("deployment" in str(m.get("content", "")).lower() or "completed" in str(m.get("content", "")).lower())
        for m in last_10
    )

    # Only count truly blocking unresolved errors (not TodoWrite noise or empty errors)
    # AND only errors in the last 20% of conversation (earlier errors may have been worked around)
    last_20_percent_index = int(len(messages) * 0.8)
    blocking_errors = [
        e for e in errors
        if not e.get("resolved", True)
        and "todowrite" not in e["error_text"].lower()
        and len(e["error_text"].strip()) > 20
        and e["index"] > last_20_percent_index  # Only recent errors are blocking
        and "vercel" not in e["error_text"].lower()  # Deployment errors often get worked around
        and not any(url in e["error_text"] for url in ["http://", "https://"])  # URLs aren't errors
    ]

    if (has_build_success or has_test_success or has_deployment or has_completion_confirmation) and not blocking_errors:
        completion_status = "success"
    elif blocking_errors:
        completion_status = "failed"
    else:
        completion_status = "partial"

    # Detect frameworks/languages
    all_content = " ".join(str(m.get("content", "")) for m in msg_data_list).lower()

    frameworks = []
    if "react" in all_content or "jsx" in all_content:
        frameworks.append("react")
    if "next" in all_content or "next.js" in all_content:
        frameworks.append("nextjs")
    if "typescript" in all_content or ".tsx" in all_content or ".ts" in all_content:
        frameworks.append("typescript")
    if "python" in all_content or ".py" in all_content:
        frameworks.append("python")

    # Pattern reusability
    high_value_patterns = ["cascade_updates", "removal", "refactor"]
    pattern_reusability = "high" if any(p["operation_type"] in high_value_patterns for p in patterns) else "medium"

    # Error recovery
    error_recovery = any(e.get("resolved", False) for e in errors)

    signature = {
        "completion_status": completion_status,
        "frameworks": frameworks,
        "pattern_reusability": pattern_reusability,
        "error_recovery": error_recovery,
        "total_edits": len(patterns),
        "iteration_count": len([m for m in msg_data_list if m.get("role") == "user"])
    }

    # Integrate metadata if provided
    if metadata:
        tool_usage = metadata.get('tool_usage', {})
        signature.update({
            "tools_used": list(tool_usage.get('tools_summary', {}).keys())[:10],
            "files_modified": [f['path'] if isinstance(f, dict) else f for f in tool_usage.get('files_edited', [])][:10],
            "concepts": list(metadata.get('concepts', []))[:10],
            "analysis_only": len(tool_usage.get('files_edited', [])) == 0
        })

    return signature


def build_search_index(messages: List[Dict], patterns: List[Dict], errors: List[Dict]) -> str:
    """
    Build 500-token search index optimized for keyword matching.

    Opus structure:
    - User request (exact words)
    - Solution type + tools used
    - Files modified + operation types
    - Primary keywords (3-5 specific terms)
    """
    parts = []

    # Extract user requests (exclude tool_result noise AND meta commands)
    user_requests = []
    for i, msg in enumerate(messages):
        msg_data = get_message_data(msg)
        if msg_data.get("role") == "user":
            content = str(msg_data.get("content", ""))
            # Skip tool results, meta commands, and system messages
            if (len(content) > 50 and
                "tool_result" not in content and
                "tool_use_id" not in content and
                "<command-name>" not in content and
                "Caveat:" not in content and
                "<local-command" not in content):
                user_requests.append(content[:200])
                if len(user_requests) >= 2:  # Top 2 requests
                    break

    if user_requests:
        parts.append("## User Request")
        for req in user_requests:
            parts.append(req)
        parts.append("")

    # Edit patterns
    if patterns:
        parts.append("## Solution Pattern")
        for p in patterns[:3]:  # Top 3 patterns
            file_short = p["file"].split("/")[-1] if "/" in p["file"] else p["file"]
            parts.append(f"{p['operation_type']}: {file_short}")
            parts.append(f"  {p['pattern_description']}")
        parts.append("")

    # Unresolved errors only
    unresolved = [e for e in errors if not e.get("resolved", True)]
    if unresolved:
        parts.append("## Active Issues")
        for err in unresolved[:2]:
            parts.append(err["error_text"][:100])
        parts.append("")

    return "\n".join(parts)


def build_context_cache(messages: List[Dict], patterns: List[Dict], errors: List[Dict]) -> str:
    """
    Build 1000-token context cache with detailed implementation.

    Opus structure:
    - Full edit patterns with line numbers
    - Error→recovery sequences
    - Build/test output snippets
    - Code snippets showing the pattern
    """
    parts = []

    # Detailed edit patterns
    if patterns:
        parts.append("## Implementation Details")
        for p in patterns[:5]:
            parts.append(f"[Msg {p['index']}] {p['operation_type']}")
            parts.append(f"  File: {p['file']}")
            parts.append(f"  Pattern: {p['pattern_description']}")
            if p['why'] != "Unknown":
                parts.append(f"  Context: {p['why']}")
        parts.append("")

    # Error recovery sequences
    resolved_errors = [e for e in errors if e.get("resolved", False)]
    if resolved_errors:
        parts.append("## Error Recovery")
        for err in resolved_errors[:3]:
            parts.append(f"[Msg {err['index']}] Error: {err['error_text'][:100]}")
            if err.get("resolution"):
                parts.append(f"  Fix: {err['resolution'][:100]}")
        parts.append("")

    # Key validation moments
    parts.append("## Validation")
    for i, msg in enumerate(messages):
        msg_data = get_message_data(msg)
        content_str = str(msg_data.get("content", "")).lower()

        if "compiled successfully" in content_str or ("build" in content_str and "success" in content_str):
            parts.append(f"[Msg {i}] Build: Success")
        elif "test" in content_str and "pass" in content_str:
            parts.append(f"[Msg {i}] Tests: Passed")

    return "\n".join(parts)


def extract_error_context(messages: List[Dict], error_index: int) -> Dict:
    """Extract error with resolution tracking."""
    msg_data = get_message_data(messages[error_index])
    content = msg_data.get("content", "")

    # Extract clean error text
    if isinstance(content, list):
        error_parts = []
        for item in content:
            if isinstance(item, dict) and item.get("type") == "tool_result":
                result_content = item.get("content", "")
                if "error" in str(result_content).lower():
                    error_parts.append(str(result_content)[:300])
            elif isinstance(item, str):
                error_parts.append(item[:300])
        error_text = " ".join(error_parts)
    else:
        error_text = str(content)[:300]

    # Check if resolved (explicit or implicit)
    resolved = False
    resolution_text = None
    for i in range(error_index+1, min(len(messages), error_index+15)):
        check_data = get_message_data(messages[i])
        check_msg = json.dumps(check_data.get("content", "")).lower()

        # Explicit resolution
        if any(word in check_msg for word in ["fixed", "solved", "working"]):
            resolved = True
            resolution_text = check_msg[:200]
            break

        # Implicit resolution: success after error
        if "connection_refused" in error_text.lower():
            # If server starts or page loads successfully after, it's resolved
            if ("background" in check_msg and "running" in check_msg) or ("playwright" in check_msg and "success" not in check_msg and "error" not in check_msg):
                resolved = True
                resolution_text = "Server started / page loaded successfully"
                break

        # Build success after build error
        if ("build" in error_text.lower() or "compil" in error_text.lower()) and "compiled successfully" in check_msg:
            resolved = True
            resolution_text = "Build succeeded"
            break

    return {
        "index": error_index,
        "error_text": error_text,
        "resolved": resolved,
        "resolution": resolution_text
    }


def extract_events_v3(messages: List[Dict], metadata: Optional[Dict] = None) -> Dict[str, Any]:
    """
    V3 extraction with Opus recommendations and optional metadata enrichment.

    Args:
        messages: List of conversation messages
        metadata: Optional dict with {
            'tool_usage': {...},  # From extract_tool_usage_from_jsonl
            'concepts': [...]     # From extract_concepts
        }

    Returns:
    - search_index: 500 tokens for vector embedding
    - context_cache: 1000 tokens for payload storage
    - signature: Metadata for filtering (enriched with tool/concept data if provided)
    - metadata: Original metadata passed through for use in narrative generation
    """

    # Score all messages with V3 weights
    scores = [
        (i, calculate_importance_score_v3(msg, i, len(messages)))
        for i, msg in enumerate(messages)
    ]
    scores.sort(key=lambda x: x[1], reverse=True)
    top_indices = [idx for idx, score in scores[:20]]

    # Extract edit patterns
    patterns = []
    for i in top_indices:
        msg_data = get_message_data(messages[i])
        if msg_data.get("role") == "assistant":
            content = msg_data.get("content", [])
            if isinstance(content, list):
                for item in content:
                    if isinstance(item, dict) and item.get("type") == "tool_use":
                        tool_name = item.get("name", "")
                        if ("edit" in tool_name.lower() or "write" in tool_name.lower()) and "todo" not in tool_name.lower():
                            pattern = extract_edit_pattern(messages, i)
                            patterns.append(pattern)

    # Extract errors
    errors = []
    for i, msg in enumerate(messages):
        msg_data = get_message_data(msg)
        content_str = json.dumps(msg_data.get("content", "")).lower()
        if any(kw in content_str for kw in ["error", "exception", "failed"]):
            errors.append(extract_error_context(messages, i))

    # Build outputs (with metadata enrichment)
    search_index = build_search_index(messages, patterns, errors)
    context_cache = build_context_cache(messages, patterns, errors)
    signature = build_conversation_signature(messages, errors, patterns, metadata)

    result = {
        "search_index": search_index,
        "context_cache": context_cache,
        "signature": signature,
        "stats": {
            "original_messages": len(messages),
            "search_index_tokens": len(search_index) // 4,
            "context_cache_tokens": len(context_cache) // 4,
            "total_tokens": (len(search_index) + len(context_cache)) // 4,
            "patterns_found": len(patterns),
            "errors_found": len(errors)
        }
    }

    # Include metadata in result for narrative generation
    if metadata:
        result["metadata"] = metadata

    return result


if __name__ == "__main__":
    import sys
    from pathlib import Path

    if len(sys.argv) < 2:
        print("Usage: python extract_events_v3.py <conversation.jsonl>")
        sys.exit(1)

    jsonl_path = Path(sys.argv[1])

    # Read messages
    messages = []
    with open(jsonl_path) as f:
        for line in f:
            if line.strip():
                messages.append(json.loads(line))

    # Extract events V3
    result = extract_events_v3(messages)

    print(f"\n{'='*80}")
    print(f"EVENT EXTRACTION V3 (OPUS-VALIDATED)")
    print(f"{'='*80}\n")

    print(f"Original messages: {result['stats']['original_messages']}")
    print(f"Search index: {result['stats']['search_index_tokens']:,} tokens")
    print(f"Context cache: {result['stats']['context_cache_tokens']:,} tokens")
    print(f"Total: {result['stats']['total_tokens']:,} tokens")
    print(f"Patterns found: {result['stats']['patterns_found']}")
    print(f"Errors found: {result['stats']['errors_found']}")

    print(f"\n{'='*80}")
    print(f"CONVERSATION SIGNATURE")
    print(f"{'='*80}\n")
    print(json.dumps(result['signature'], indent=2))

    print(f"\n{'='*80}")
    print(f"SEARCH INDEX (for vector embedding)")
    print(f"{'='*80}\n")
    print(result['search_index'])

    print(f"\n{'='*80}")
    print(f"CONTEXT CACHE (for payload storage)")
    print(f"{'='*80}\n")
    print(result['context_cache'])
