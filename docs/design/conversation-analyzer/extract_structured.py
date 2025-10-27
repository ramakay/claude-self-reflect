#!/usr/bin/env python3
"""
Extract structured data from Claude Code conversation JSONL.
Handles large files by trimming to stay within token budgets.
"""

import json
import sys
from pathlib import Path
from typing import Dict, List, Any
from datetime import datetime


def estimate_tokens(text: str) -> int:
    """Rough token estimation (1 token ≈ 4 characters)."""
    return len(text) // 4


def trim_conversation(messages: List[Dict], max_tokens: int = 150000) -> List[Dict]:
    """
    Trim conversation to fit within token budget.
    Strategy: Keep first 20% + last 50% of messages (where solutions usually are).
    """
    if not messages:
        return []

    # Estimate total tokens
    total_text = json.dumps(messages)
    total_tokens = estimate_tokens(total_text)

    if total_tokens <= max_tokens:
        return messages

    # Keep first 20% and last 50% of messages
    n = len(messages)
    first_count = max(10, int(n * 0.2))
    last_count = max(20, int(n * 0.5))

    trimmed = messages[:first_count] + [
        {
            "role": "assistant",
            "content": f"[... {n - first_count - last_count} messages omitted for brevity ...]",
            "type": "text"
        }
    ] + messages[-last_count:]

    print(f"Trimmed conversation: {n} → {len(trimmed)} messages (~{estimate_tokens(json.dumps(trimmed))} tokens)", file=sys.stderr)

    return trimmed


def extract_files(messages: List[Dict]) -> Dict[str, List[str]]:
    """Extract files that were read, edited, or created."""
    files = {"read": set(), "edited": set(), "created": set()}

    for msg in messages:
        if msg.get("type") != "tool_use":
            continue

        content = msg.get("content", [])
        if isinstance(content, str):
            continue

        for item in content if isinstance(content, list) else [content]:
            if not isinstance(item, dict):
                continue

            tool_name = item.get("name") or item.get("type", "")

            if "read" in tool_name.lower():
                if "file_path" in item.get("input", {}):
                    files["read"].add(item["input"]["file_path"])
            elif "edit" in tool_name.lower():
                if "file_path" in item.get("input", {}):
                    files["edited"].add(item["input"]["file_path"])
            elif "write" in tool_name.lower():
                if "file_path" in item.get("input", {}):
                    files["created"].add(item["input"]["file_path"])

    return {k: sorted(list(v)) for k, v in files.items()}


def extract_tools(messages: List[Dict]) -> Dict[str, int]:
    """Count tool usage."""
    tools = {}

    for msg in messages:
        if msg.get("type") != "tool_use":
            continue

        content = msg.get("content", [])
        if isinstance(content, str):
            continue

        for item in content if isinstance(content, list) else [content]:
            if not isinstance(item, dict):
                continue

            tool_name = item.get("name") or item.get("type", "unknown")
            tools[tool_name] = tools.get(tool_name, 0) + 1

    return dict(sorted(tools.items(), key=lambda x: x[1], reverse=True))


def extract_errors(messages: List[Dict]) -> List[Dict[str, Any]]:
    """Extract error messages and track if they were resolved."""
    errors = []

    for i, msg in enumerate(messages):
        content_str = json.dumps(msg.get("content", "")).lower()

        # Look for error indicators
        if any(keyword in content_str for keyword in ["error", "failed", "exception", "traceback"]):
            # Check if resolved in next few messages
            resolved = False
            for j in range(i + 1, min(i + 5, len(messages))):
                next_content = json.dumps(messages[j].get("content", "")).lower()
                if any(word in next_content for word in ["success", "fixed", "working", "resolved"]):
                    resolved = True
                    break

            errors.append({
                "message_index": i,
                "preview": content_str[:200],
                "resolved": resolved
            })

    return errors


def extract_structured_data(jsonl_path: Path, max_tokens: int = 150000) -> Dict[str, Any]:
    """
    Extract structured data from conversation JSONL.
    Returns JSON suitable for LLM analysis.
    """
    messages = []

    # Read JSONL
    with open(jsonl_path, 'r') as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
                messages.append(msg)
            except json.JSONDecodeError:
                continue

    # Trim if needed
    messages = trim_conversation(messages, max_tokens)

    # Extract components
    files = extract_files(messages)
    tools = extract_tools(messages)
    errors = extract_errors(messages)

    # Build structured data
    return {
        "conversation_id": jsonl_path.stem,
        "total_messages": len(messages),
        "messages": messages,  # Trimmed messages
        "files": files,
        "tools_used": tools,
        "errors": errors,
        "has_code": any("```" in json.dumps(msg.get("content", "")) for msg in messages),
        "metadata": {
            "source_file": str(jsonl_path),
            "extracted_at": datetime.now().isoformat(),
            "trimmed": len(messages) < sum(1 for _ in open(jsonl_path))
        }
    }


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python extract_structured.py <conversation.jsonl>", file=sys.stderr)
        sys.exit(1)

    jsonl_path = Path(sys.argv[1])
    if not jsonl_path.exists():
        print(f"Error: File not found: {jsonl_path}", file=sys.stderr)
        sys.exit(1)

    structured_data = extract_structured_data(jsonl_path)
    print(json.dumps(structured_data, indent=2))
