"""
Context Management for Claude 2.0.1 Context Editing API.

Implements automatic tool result clearing with configurable triggers,
token counting preview, and response tracking for statusline visualization.

Complexity: Designed following PR #69 patterns (target complexity <5)
"""

import os
import logging
from typing import Dict, Any, Optional, List
from datetime import datetime
from dataclasses import dataclass
from pathlib import Path
import json

logger = logging.getLogger(__name__)

# Context Management Configuration
BETA_HEADER = "context-management-2025-06-27"

@dataclass
class ClearingStats:
    """Statistics from a single clearing event."""
    timestamp: str
    original_tokens: int
    after_tokens: int
    cleared_tokens: int
    cleared_tools: int
    worth_cache_break: bool


class ContextManagerConfig:
    """
    Configuration for Context Editing API.
    Complexity: 1 (simple data structure)
    """

    def __init__(
        self,
        trigger_tokens: int = 30000,
        keep_tool_uses: int = 5,
        clear_at_least_tokens: int = 5000,
        exclude_tools: Optional[List[str]] = None
    ):
        self.trigger_tokens = trigger_tokens
        self.keep_tool_uses = keep_tool_uses
        self.clear_at_least_tokens = clear_at_least_tokens
        self.exclude_tools = exclude_tools or [
            "reflect_on_past",
            "csr_reflect_on_past",
            "search_memory",
            "store_reflection",
            "get_full_conversation",
            "store_to_memory"
        ]

    def to_api_config(self) -> Dict[str, Any]:
        """Convert to API-compatible configuration."""
        return {
            "edits": [{
                "type": "clear_tool_uses_20250919",
                "trigger": {
                    "type": "input_tokens",
                    "value": self.trigger_tokens
                },
                "keep": {
                    "type": "tool_uses",
                    "value": self.keep_tool_uses
                },
                "clear_at_least": {
                    "type": "input_tokens",
                    "value": self.clear_at_least_tokens
                },
                "exclude_tools": self.exclude_tools,
                "clear_tool_inputs": False
            }]
        }


class TokenCountPreview:
    """
    Preview token counting before clearing.
    Complexity: 2 (simple calculation logic)
    """

    @staticmethod
    def calculate(original: int, after: int, min_savings: int = 5000) -> Dict[str, Any]:
        """Calculate clearing impact."""
        savings = original - after

        return {
            "will_clear": savings > 0,
            "original_tokens": original,
            "after_tokens": after,
            "savings": savings,
            "percentage": round((savings / original * 100), 1) if original > 0 else 0,
            "worth_cache_break": savings >= min_savings
        }


class ContextStatsTracker:
    """
    Track context management statistics for statusline.
    Complexity: 3 (simple state management)
    """

    def __init__(self):
        self.session = {
            "total_cleared_tokens": 0,
            "total_cleared_tools": 0,
            "clearing_events": [],
            "cache_breaks": 0,
            "current_tokens": 0
        }

    def track_clearing(self, stats: ClearingStats) -> None:
        """Track a clearing event."""
        self.session["total_cleared_tokens"] += stats.cleared_tokens
        self.session["total_cleared_tools"] += stats.cleared_tools
        if stats.worth_cache_break:
            self.session["cache_breaks"] += 1

        self.session["clearing_events"].append({
            "timestamp": stats.timestamp,
            "original_tokens": stats.original_tokens,
            "after_tokens": stats.after_tokens,
            "cleared_tokens": stats.cleared_tokens,
            "cleared_tools": stats.cleared_tools
        })

    def update_current_tokens(self, tokens: int) -> None:
        """Update current token count."""
        self.session["current_tokens"] = tokens

    def get_statusline_text(self) -> str:
        """
        Format for statusline display.
        Complexity: 4 (conditional formatting)
        """
        # No clearing yet
        if self.session["total_cleared_tokens"] == 0:
            if self.session["current_tokens"] > 25000:
                return f"Context: {self._format_tokens(self.session['current_tokens'])} (clearing soon)"
            return ""

        # Recent clearing - show last event
        if self.session["clearing_events"]:
            last = self.session["clearing_events"][-1]
            orig = self._format_tokens(last["original_tokens"])
            after = self._format_tokens(last["after_tokens"])
            cleared = self._format_tokens(last["cleared_tokens"])
            tools = last["cleared_tools"]

            return f"Context: {orig}→{after} (-{cleared}↓ -{tools}🔧)"

        # Session total
        total = self._format_tokens(self.session["total_cleared_tokens"])
        tools = self.session["total_cleared_tools"]
        return f"Session: {total} saved • {tools} tools cleared"

    @staticmethod
    def _format_tokens(tokens: int) -> str:
        """Format token count for display."""
        if tokens >= 1000:
            return f"{tokens//1000}k"
        return str(tokens)

    def to_dict(self) -> Dict[str, Any]:
        """Export session stats."""
        return self.session.copy()


class ResponseTracker:
    """
    Track context management responses from API.
    Complexity: 3 (simple parsing and storage)
    """

    @staticmethod
    def extract_stats(response: Dict[str, Any]) -> Optional[ClearingStats]:
        """Extract clearing statistics from API response with defensive checks."""
        cm = response.get("context_management")
        if not cm or not cm.get("applied_edits"):
            return None

        for edit in cm["applied_edits"]:
            # Defensive checks (CodeRabbit fix)
            if not isinstance(edit, dict):
                continue

            if edit.get("type") != "clear_tool_uses_20250919":
                continue

            # Verify required fields exist
            if "cleared_input_tokens" not in edit or "cleared_tool_uses" not in edit:
                continue

            # Safely extract and coerce values
            usage = response.get("usage", {})
            input_tokens = int(usage.get("input_tokens", 0))
            cleared = int(edit.get("cleared_input_tokens", 0))
            cleared_tools = edit.get("cleared_tool_uses", 0)

            if cleared == 0:  # No actual clearing happened
                continue

            original = input_tokens + cleared

            return ClearingStats(
                timestamp=datetime.now().isoformat(),
                original_tokens=original,
                after_tokens=input_tokens,
                cleared_tokens=cleared,
                cleared_tools=cleared_tools,
                worth_cache_break=cleared >= 5000
            )

        return None


class ContextManager:
    """
    Main context management coordinator.
    Complexity: 2 (simple delegation)
    """

    def __init__(self, config: Optional[ContextManagerConfig] = None):
        self.config = config or ContextManagerConfig()
        self.stats_tracker = ContextStatsTracker()
        self.response_tracker = ResponseTracker()

    def get_api_config(self) -> Dict[str, Any]:
        """Get API-compatible configuration."""
        return self.config.to_api_config()

    def get_beta_header(self) -> str:
        """Get beta header for API requests."""
        return BETA_HEADER

    def track_response(self, response: Dict[str, Any]) -> Optional[ClearingStats]:
        """Track response and update statistics."""
        stats = self.response_tracker.extract_stats(response)
        if stats:
            self.stats_tracker.track_clearing(stats)
        return stats

    def update_current_tokens(self, tokens: int) -> None:
        """Update current conversation token count."""
        self.stats_tracker.update_current_tokens(tokens)

    def get_statusline(self) -> str:
        """Get formatted statusline text."""
        return self.stats_tracker.get_statusline_text()

    def get_session_stats(self) -> Dict[str, Any]:
        """Get all session statistics."""
        return self.stats_tracker.to_dict()

    def save_to_unified_state(self, state_file: Path) -> None:
        """
        Save context management stats to unified state with atomic write.
        Complexity: 3 (read, write temp, atomic replace)
        """
        try:
            if state_file.exists():
                with open(state_file, 'r') as f:
                    state = json.load(f)
            else:
                state = {}

            state["context_management"] = self.get_session_stats()
            state["last_updated"] = datetime.now().isoformat()

            # Atomic write (CodeRabbit fix): write to temp file then replace
            temp_file = state_file.with_suffix('.tmp')
            with open(temp_file, 'w') as f:
                json.dump(state, f, indent=2)
                f.flush()
                os.fsync(f.fileno())  # Ensure data is on disk

            # Atomic replace
            temp_file.replace(state_file)

        except Exception as e:
            logger.error(f"Failed to save context management state: {e}")
            # Clean up temp file if it exists
            temp_file = state_file.with_suffix('.tmp')
            if temp_file.exists():
                try:
                    temp_file.unlink()
                except Exception:
                    pass


# Module-level instance for convenience
_default_manager: Optional[ContextManager] = None


def get_context_manager() -> ContextManager:
    """
    Get or create default context manager.
    Complexity: 1 (singleton access)
    """
    global _default_manager
    if _default_manager is None:
        _default_manager = ContextManager()
    return _default_manager


def preview_clearing(
    original_tokens: int,
    after_tokens: int,
    min_savings: int = 5000
) -> Dict[str, Any]:
    """
    Preview token clearing impact.
    Complexity: 1 (delegation to TokenCountPreview)
    """
    return TokenCountPreview.calculate(original_tokens, after_tokens, min_savings)
