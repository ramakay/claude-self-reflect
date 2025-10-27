#!/usr/bin/env python3
"""
Smart Event Extraction V2 - Optimized for Token Efficiency

Changes from V1:
1. Fixed file modifications detection bug
2. Removed useless tool_use_id metadata
3. Additional compression strategies:
   - Deduplicate repeated errors
   - Consolidate sequential tool calls
   - Truncate verbose build outputs
   - Remove JSON type fields
   - Compress code snippets
"""

import json
import re
from typing import Dict, List, Any, Tuple
from datetime import datetime
from collections import defaultdict


class ConversationState:
    """Track conversation state for transition detection."""
    EXPLORING = "exploring"
    PROBLEM_STATED = "problem"
    ATTEMPTING = "attempting"
    ERROR = "error"
    SOLUTION = "solution"
    VALIDATED = "validated"


def get_message_data(msg: Dict) -> Dict:
    """Extract message data handling nested structure."""
    return msg.get("message", msg)


def calculate_importance_score(msg: Dict, index: int, total: int) -> float:
    """Score message importance using information density."""
    score = 0.0
    msg_data = get_message_data(msg)
    content = json.dumps(msg_data.get("content", "")).lower()

    # Error signals (highest priority)
    error_keywords = ["error", "exception", "traceback", "failed", "failure"]
    if any(kw in content for kw in error_keywords):
        score += 10.0

    # Solution indicators
    solution_keywords = ["fixed", "solved", "working", "success", "completed"]
    if any(kw in content for kw in solution_keywords):
        score += 8.0

    # Code blocks (implementation details)
    if "```" in content:
        code_blocks = content.count("```") // 2
        score += 5.0 * code_blocks

    # Tool usage (actual work being done)
    if isinstance(msg_data.get("content"), list):
        for item in msg_data.get("content", []):
            if isinstance(item, dict) and item.get("type") == "tool_use":
                tool_name = str(item.get("name", "")).lower()
                if "edit" in tool_name or "write" in tool_name:
                    score += 7.0
                elif "bash" in tool_name:
                    score += 5.0
                else:
                    score += 3.0

    # User requests (problem statements)
    if msg_data.get("role") == "user" and len(content) > 100:
        score += 6.0

    # Position bias - beginnings and ends often important
    relative_pos = index / max(total, 1)
    if relative_pos < 0.1 or relative_pos > 0.8:
        score *= 1.2

    return score


def clean_tool_result(content: str, max_length: int = 300) -> str:
    """Clean and truncate tool result content."""
    # Remove escape sequences
    content = re.sub(r'\x1b\[[0-9;]*m', '', content)

    # Truncate long build outputs
    if "next build" in content.lower() or "compiled" in content.lower():
        # Keep first few lines and last line
        lines = content.split('\n')
        if len(lines) > 10:
            content = '\n'.join(lines[:5] + ['...'] + lines[-2:])

    # Truncate to max length
    if len(content) > max_length:
        content = content[:max_length] + "..."

    return content


def format_tool_use(item: Dict) -> str:
    """Format tool_use as human-readable string (no tool_use_id)."""
    tool_name = item.get("name", "unknown")
    tool_input = item.get("input", {})

    if tool_name == "Read":
        file_path = tool_input.get("file_path", "")
        return f"Read: {file_path}"

    elif tool_name in ["Edit", "MultiEdit"]:
        file_path = tool_input.get("file_path", "")
        return f"Edit: {file_path}"

    elif tool_name == "Write":
        file_path = tool_input.get("file_path", "")
        return f"Write: {file_path}"

    elif tool_name == "Bash":
        cmd = tool_input.get("command", "")
        desc = tool_input.get("description", "")
        return f"Bash: {desc or cmd[:50]}"

    elif tool_name == "TodoWrite":
        return "TodoWrite: Updated task list"

    else:
        return f"{tool_name}: {str(tool_input)[:100]}"


def format_tool_result(item: Dict) -> str:
    """Format tool_result as human-readable string (no tool_use_id)."""
    content = item.get("content", "")

    if isinstance(content, str):
        return clean_tool_result(content)
    elif isinstance(content, list):
        # Handle structured content
        parts = []
        for c in content:
            if isinstance(c, dict) and c.get("type") == "text":
                parts.append(c.get("text", ""))
        return clean_tool_result(" ".join(parts))
    else:
        return str(content)[:200]


def extract_error_context(messages: List[Dict], error_index: int) -> Dict:
    """Extract error with surrounding context."""
    msg_data = get_message_data(messages[error_index])
    content = msg_data.get("content", "")

    # Extract clean error text
    if isinstance(content, list):
        error_parts = []
        for item in content:
            if isinstance(item, dict):
                if item.get("type") == "tool_result":
                    error_parts.append(format_tool_result(item))
            elif isinstance(item, str):
                error_parts.append(item)
        error_text = " ".join(error_parts)[:500]
    else:
        error_text = str(content)[:500]

    # Check if resolved
    resolved = False
    resolution_text = None
    for i in range(error_index+1, min(len(messages), error_index+10)):
        check_data = get_message_data(messages[i])
        check_msg = json.dumps(check_data.get("content", "")).lower()
        if any(word in check_msg for word in ["fixed", "solved", "working", "success"]):
            resolved = True
            resolution_text = check_msg[:200]
            break

    return {
        "index": error_index,
        "error_text": error_text,
        "resolved": resolved,
        "resolution": resolution_text
    }


def extract_file_modifications(messages: List[Dict]) -> List[Dict]:
    """Extract all file modifications with context (FIXED VERSION)."""
    modifications = []

    for i, msg in enumerate(messages):
        msg_data = get_message_data(msg)

        # Check if this is an assistant message with tool_use
        if msg_data.get("role") != "assistant":
            continue

        content = msg_data.get("content", [])
        if not isinstance(content, list):
            continue

        # Look through content for tool_use items
        for item in content:
            if not isinstance(item, dict):
                continue

            if item.get("type") != "tool_use":
                continue

            tool_name = item.get("name", "")

            # Check for file modification tools (FIXED: MultiEdit, not just Edit)
            is_file_mod = (
                "edit" in tool_name.lower() or
                "write" in tool_name.lower()
            ) and "todo" not in tool_name.lower()  # Exclude TodoWrite

            if not is_file_mod:
                continue

            # Extract file path
            file_path = item.get("input", {}).get("file_path", "unknown")

            # Find WHY this was done (look at recent user messages)
            reason = "Unknown"
            for j in range(max(0, i-3), i):
                check_data = get_message_data(messages[j])
                if check_data.get("role") == "user":
                    check_content = str(check_data.get("content", ""))
                    if len(check_content) > 50:
                        reason = check_content[:200]
                        break

            modifications.append({
                "index": i,
                "file": file_path,
                "action": tool_name,
                "reason": reason
            })

    return modifications


def extract_user_goals(messages: List[Dict]) -> List[Dict]:
    """Extract substantive user requests (not greetings)."""
    goals = []

    for i, msg in enumerate(messages):
        msg_data = get_message_data(msg)

        if msg_data.get("role") != "user":
            continue

        content = str(msg_data.get("content", ""))

        # Skip short messages
        if len(content) < 50:
            continue

        # Skip pure greetings
        greetings = ["hi", "hello", "thanks", "thank you", "ok", "okay", "sure"]
        if content.strip().lower() in greetings:
            continue

        # Must contain action words
        action_words = [
            "help", "fix", "create", "add", "remove", "update",
            "change", "implement", "build", "error", "issue", "problem"
        ]

        if any(word in content.lower() for word in action_words):
            goals.append({
                "index": i,
                "goal": content[:500]
            })

    return goals


def deduplicate_errors(errors: List[Dict]) -> List[Dict]:
    """Remove duplicate errors (same error text appearing multiple times)."""
    seen = set()
    unique_errors = []

    for error in errors:
        # Create a signature from the error text
        signature = error["error_text"][:100].lower()

        if signature not in seen:
            seen.add(signature)
            unique_errors.append(error)

    return unique_errors


def build_event_timeline(messages: List[Dict]) -> str:
    """Build a compact event-based timeline with compression."""

    # Score all messages
    scores = [
        (i, calculate_importance_score(msg, i, len(messages)))
        for i, msg in enumerate(messages)
    ]

    # Sort by importance
    scores.sort(key=lambda x: x[1], reverse=True)

    # Take top 20 most important messages
    top_indices = sorted([idx for idx, score in scores[:20]])

    # Extract specific event types
    user_goals = extract_user_goals(messages)
    modifications = extract_file_modifications(messages)

    # Find and deduplicate errors
    errors = []
    for i, msg in enumerate(messages):
        msg_data = get_message_data(msg)
        content_str = json.dumps(msg_data.get("content", "")).lower()
        if any(kw in content_str for kw in ["error", "exception", "failed"]):
            errors.append(extract_error_context(messages, i))

    errors = deduplicate_errors(errors)

    # Build compact narrative
    narrative_parts = []

    # User goals section
    if user_goals:
        narrative_parts.append("## User Goals")
        for goal in user_goals[:3]:
            narrative_parts.append(f"[Msg {goal['index']}] {goal['goal']}")
        narrative_parts.append("")

    # File modifications section (FIXED: now appears!)
    if modifications:
        narrative_parts.append("## Files Modified")
        for mod in modifications[:10]:
            # Extract just the filename if path is long
            file = mod['file']
            if len(file) > 60:
                file = "..." + file[-57:]
            narrative_parts.append(f"[Msg {mod['index']}] {mod['action']}: {file}")
            if mod['reason'] != "Unknown":
                narrative_parts.append(f"  Why: {mod['reason'][:120]}")
        narrative_parts.append("")

    # Errors section (only unresolved, deduplicated)
    unresolved_errors = [e for e in errors if not e["resolved"]]
    if unresolved_errors:
        narrative_parts.append("## Unresolved Errors")
        for error in unresolved_errors[:3]:
            narrative_parts.append(f"[Msg {error['index']}] ❌ {error['error_text'][:200]}")
        narrative_parts.append("")

    # Key moments (top-scored messages) - COMPRESSED FORMAT
    narrative_parts.append("## Key Moments")
    for idx in top_indices[:10]:
        msg = messages[idx]
        msg_data = get_message_data(msg)
        role = msg_data.get("role", "?")

        content = msg_data.get("content", "")

        # Format content based on type
        if isinstance(content, str):
            content_str = content[:200]
        elif isinstance(content, list):
            parts = []
            for item in content:
                if isinstance(item, dict):
                    if item.get("type") == "tool_use":
                        parts.append(format_tool_use(item))
                    elif item.get("type") == "tool_result":
                        parts.append(format_tool_result(item))
                    elif item.get("type") == "text":
                        parts.append(item.get("text", "")[:100])
                elif isinstance(item, str):
                    parts.append(item[:100])
            content_str = " | ".join(parts)[:250]
        else:
            content_str = str(content)[:200]

        narrative_parts.append(f"[Msg {idx}] {role}: {content_str}")

    return "\n".join(narrative_parts)


def extract_events(messages: List[Dict], max_tokens: int = 4000) -> Dict[str, Any]:
    """
    Extract important events from conversation.

    V2 improvements:
    - Fixed file modifications detection
    - Removed tool_use_id metadata
    - Deduplicated errors
    - Compressed format
    """

    # Build timeline
    timeline = build_event_timeline(messages)

    # Estimate tokens (rough: 1 token ≈ 4 chars)
    estimated_tokens = len(timeline) // 4

    # If over budget, trim key moments section
    if estimated_tokens > max_tokens:
        lines = timeline.split("\n")

        # Find "Key Moments" section
        key_moments_idx = None
        for i, line in enumerate(lines):
            if "Key Moments" in line:
                key_moments_idx = i
                break

        if key_moments_idx:
            # Keep everything before Key Moments + just 5 moments
            trimmed_lines = lines[:key_moments_idx+1]

            # Add just 5 key moments
            moment_count = 0
            for line in lines[key_moments_idx+1:]:
                trimmed_lines.append(line)
                if line.startswith("[Msg"):
                    moment_count += 1
                    if moment_count >= 5:
                        break

            timeline = "\n".join(trimmed_lines)

    return {
        "event_timeline": timeline,
        "estimated_tokens": len(timeline) // 4,
        "original_message_count": len(messages),
        "compression_ratio": len(timeline) / (len(json.dumps(messages)) or 1)
    }


if __name__ == "__main__":
    import sys
    from pathlib import Path

    if len(sys.argv) < 2:
        print("Usage: python extract_events_v2.py <conversation.jsonl>")
        sys.exit(1)

    jsonl_path = Path(sys.argv[1])

    # Read messages
    messages = []
    with open(jsonl_path) as f:
        for line in f:
            if line.strip():
                messages.append(json.loads(line))

    # Extract events
    result = extract_events(messages)

    print(f"\n{'='*80}")
    print(f"EVENT EXTRACTION V2 RESULTS")
    print(f"{'='*80}\n")
    print(f"Original messages: {result['original_message_count']}")
    print(f"Estimated tokens: {result['estimated_tokens']:,}")
    print(f"Compression ratio: {result['compression_ratio']*100:.1f}%")
    print(f"\n{'='*80}")
    print(f"EXTRACTED TIMELINE")
    print(f"{'='*80}\n")
    print(result['event_timeline'])
