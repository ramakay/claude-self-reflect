#!/usr/bin/env python3
"""
Smart Event Extraction - Captures Important Moments from Conversations

Instead of sending 79 full messages (439K tokens), we extract only the
important events: errors, fixes, goals, and outcomes.

Based on state-transition detection and information density scoring.
"""

import json
import re
from typing import Dict, List, Any, Tuple
from datetime import datetime


class ConversationState:
    """Track conversation state for transition detection."""
    EXPLORING = "exploring"
    PROBLEM_STATED = "problem"
    ATTEMPTING = "attempting"
    ERROR = "error"
    SOLUTION = "solution"
    VALIDATED = "validated"


def calculate_importance_score(msg: Dict, index: int, total: int) -> float:
    """
    Score message importance using information density.
    Higher score = more important to include.
    """
    score = 0.0

    # Handle nested message structure from Claude Code JSONL
    if "message" in msg:
        msg_data = msg["message"]
    else:
        msg_data = msg

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
        # Count code blocks
        code_blocks = content.count("```") // 2
        score += 5.0 * code_blocks

    # Tool usage (actual work being done)
    if msg_data.get("type") == "tool_use":
        tool_name = str(msg_data.get("name", "")).lower()
        if "edit" in tool_name or "write" in tool_name:
            score += 7.0  # File modifications are important
        elif "bash" in tool_name:
            score += 5.0  # Commands show action
        else:
            score += 3.0  # Other tools

    # User requests (problem statements)
    if msg_data.get("role") == "user" and len(content) > 100:
        score += 6.0

    # Position bias - beginnings and ends often important
    relative_pos = index / max(total, 1)
    if relative_pos < 0.1 or relative_pos > 0.8:
        score *= 1.2  # Boost beginning and end

    return score


def extract_error_context(messages: List[Dict], error_index: int) -> Dict:
    """Extract error with surrounding context."""
    msg = messages[error_index]

    # Handle nested message structure
    if "message" in msg:
        msg_data = msg["message"]
    else:
        msg_data = msg

    content = json.dumps(msg_data.get("content", ""))

    # Get context window
    context_before = messages[max(0, error_index-2):error_index]
    context_after = messages[error_index+1:min(len(messages), error_index+3)]

    # Extract error text
    error_text = content[:500]  # First 500 chars

    # Check if resolved
    resolved = False
    resolution_text = None
    for i in range(error_index+1, min(len(messages), error_index+10)):
        # Handle nested structure
        check_data = messages[i].get("message", messages[i])
        check_msg = json.dumps(check_data.get("content", "")).lower()
        if any(word in check_msg for word in ["fixed", "solved", "working", "success"]):
            resolved = True
            resolution_text = check_msg[:200]
            break

    return {
        "index": error_index,
        "error_text": error_text,
        "context_before": [m.get("message", m).get("content", {}) for m in context_before],
        "resolved": resolved,
        "resolution": resolution_text
    }


def extract_file_modifications(messages: List[Dict]) -> List[Dict]:
    """Extract all file modifications with context."""
    modifications = []

    for i, msg in enumerate(messages):
        # Handle nested structure
        msg_data = msg.get("message", msg)

        if msg_data.get("type") != "tool_use":
            continue

        content = msg_data.get("content", [])
        if isinstance(content, str):
            continue

        for item in (content if isinstance(content, list) else [content]):
            if not isinstance(item, dict):
                continue

            tool_name = item.get("name", "")
            if "edit" not in tool_name.lower() and "write" not in tool_name.lower():
                continue

            # Extract file path
            file_path = item.get("input", {}).get("file_path", "unknown")

            # Find WHY this was done (look at recent messages)
            reason = "Unknown"
            for j in range(max(0, i-3), i):
                check_data = messages[j].get("message", messages[j])
                check_content = json.dumps(check_data.get("content", ""))
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
        # Handle nested structure
        msg_data = msg.get("message", msg)

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
                "goal": content[:500]  # First 500 chars
            })

    return goals


def build_event_timeline(messages: List[Dict]) -> str:
    """Build a compact event-based timeline."""

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

    # Find errors
    errors = []
    for i, msg in enumerate(messages):
        msg_data = msg.get("message", msg)
        content_str = json.dumps(msg_data.get("content", "")).lower()
        if any(kw in content_str for kw in ["error", "exception", "failed"]):
            errors.append(extract_error_context(messages, i))

    # Build compact narrative
    narrative_parts = []

    # User goals section
    if user_goals:
        narrative_parts.append("## User Goals")
        for goal in user_goals[:3]:  # Top 3 goals
            narrative_parts.append(f"[Message {goal['index']}] {goal['goal']}")
        narrative_parts.append("")

    # Errors section
    if errors:
        narrative_parts.append("## Errors Encountered")
        for error in errors[:5]:  # Top 5 errors
            status = "✅ Resolved" if error["resolved"] else "❌ Unresolved"
            narrative_parts.append(f"[Message {error['index']}] {status}")
            narrative_parts.append(f"Error: {error['error_text'][:200]}")
            if error["resolved"] and error["resolution"]:
                narrative_parts.append(f"Fix: {error['resolution']}")
            narrative_parts.append("")

    # File modifications section
    if modifications:
        narrative_parts.append("## Files Modified")
        for mod in modifications[:10]:  # Top 10 modifications
            narrative_parts.append(f"[Message {mod['index']}] {mod['action']}: {mod['file']}")
            narrative_parts.append(f"Context: {mod['reason'][:150]}")
            narrative_parts.append("")

    # Key moments (top-scored messages)
    narrative_parts.append("## Key Moments (by importance)")
    for idx in top_indices[:10]:  # Top 10 moments
        msg = messages[idx]
        msg_data = msg.get("message", msg)
        role = msg_data.get("role", "unknown")
        content = json.dumps(msg_data.get("content", ""))[:300]
        narrative_parts.append(f"[Message {idx}] {role}: {content}")
        narrative_parts.append("")

    return "\n".join(narrative_parts)


def extract_events(messages: List[Dict], max_tokens: int = 4000) -> Dict[str, Any]:
    """
    Extract important events from conversation.

    Returns structured events that fit within token budget.
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
                if line.startswith("[Message"):
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
        print("Usage: python extract_events.py <conversation.jsonl>")
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
    print(f"EVENT EXTRACTION RESULTS")
    print(f"{'='*80}\n")
    print(f"Original messages: {result['original_message_count']}")
    print(f"Estimated tokens: {result['estimated_tokens']:,}")
    print(f"Compression ratio: {result['compression_ratio']*100:.1f}%")
    print(f"\n{'='*80}")
    print(f"EXTRACTED TIMELINE")
    print(f"{'='*80}\n")
    print(result['event_timeline'])
