"""
Message processor classes for handling different message types in JSONL import.
Refactored from extract_metadata_single_pass to reduce complexity.
"""

import re
import ast
import logging
from abc import ABC, abstractmethod
from typing import Dict, Any, List, Set, Optional
from pathlib import Path

logger = logging.getLogger(__name__)

# Constants for metadata limits (can be overridden via environment variables)
import os

MAX_CONCEPTS = int(os.getenv("MAX_CONCEPTS", "10"))
MAX_AST_ELEMENTS = int(os.getenv("MAX_AST_ELEMENTS", "30"))
MAX_CODE_BLOCKS = int(os.getenv("MAX_CODE_BLOCKS", "5"))
MAX_ELEMENTS_PER_BLOCK = int(os.getenv("MAX_ELEMENTS_PER_BLOCK", "10"))
MAX_FILES_ANALYZED = int(os.getenv("MAX_FILES_ANALYZED", "20"))
MAX_FILES_EDITED = int(os.getenv("MAX_FILES_EDITED", "20"))
MAX_TOOLS_USED = int(os.getenv("MAX_TOOLS_USED", "15"))
MAX_CONCEPT_MESSAGES = int(os.getenv("MAX_CONCEPT_MESSAGES", "50"))


class MessageProcessor(ABC):
    """Abstract base class for message processing."""

    @abstractmethod
    def process(self, item: Any, metadata: Dict[str, Any]) -> Optional[str]:
        """Process a message item and update metadata."""
        pass


class TextMessageProcessor(MessageProcessor):
    """Process text messages and extract code blocks."""

    def process(self, item: Dict[str, Any], metadata: Dict[str, Any]) -> Optional[str]:
        """Process text content and extract code blocks with AST elements."""
        if item.get('type') != 'text':
            return None

        text_content = item.get('text', '')

        # Check for code blocks
        if '```' in text_content:
            metadata['has_code_blocks'] = True
            self._extract_code_ast_elements(text_content, metadata)

        return text_content

    def _extract_code_ast_elements(self, text: str, metadata: Dict[str, Any]):
        """Extract AST elements from code blocks in text."""
        if 'ast_elements' not in metadata:
            metadata['ast_elements'] = []

        if len(metadata['ast_elements']) >= MAX_AST_ELEMENTS:
            return

        # More permissive regex to handle various fence formats
        code_blocks = re.findall(r'```[^`\n]*\n?(.*?)```', text, re.DOTALL)

        for code_block in code_blocks[:MAX_CODE_BLOCKS]:
            if len(metadata['ast_elements']) >= MAX_AST_ELEMENTS:
                break

            ast_elems = extract_ast_elements(code_block)
            for elem in list(ast_elems)[:MAX_ELEMENTS_PER_BLOCK]:
                if elem not in metadata['ast_elements'] and len(metadata['ast_elements']) < MAX_AST_ELEMENTS:
                    metadata['ast_elements'].append(elem)


class ThinkingMessageProcessor(MessageProcessor):
    """Process thinking messages."""

    def process(self, item: Dict[str, Any], metadata: Dict[str, Any]) -> Optional[str]:
        """Process thinking content."""
        if item.get('type') != 'thinking':
            return None

        return item.get('thinking', '')


class ToolMessageProcessor(MessageProcessor):
    """Process tool use messages and extract file references."""

    def process(self, item: Dict[str, Any], metadata: Dict[str, Any]) -> Optional[str]:
        """Process tool use and extract file references."""
        if item.get('type') != 'tool_use':
            return None

        tool_name = item.get('name', '')

        # Track tool usage
        if 'tools_used' not in metadata:
            metadata['tools_used'] = []

        if tool_name and tool_name not in metadata['tools_used']:
            if len(metadata['tools_used']) < MAX_TOOLS_USED:
                metadata['tools_used'].append(tool_name)

        # Extract file references
        if 'input' in item:
            self._extract_file_references(item['input'], tool_name, metadata)

        # Return tool use as text
        tool_input = str(item.get('input', ''))[:500]
        return f"[Tool: {tool_name}] {tool_input}"

    def _extract_file_references(self, input_data: Any, tool_name: str, metadata: Dict[str, Any]):
        """Extract file references from tool input."""
        if not isinstance(input_data, dict):
            return

        # Initialize metadata lists if not present
        if 'files_edited' not in metadata:
            metadata['files_edited'] = []
        if 'files_analyzed' not in metadata:
            metadata['files_analyzed'] = []

        is_edit = tool_name in ['Edit', 'Write', 'MultiEdit', 'NotebookEdit']

        # Check file_path field
        if 'file_path' in input_data:
            file_ref = input_data['file_path']
            if is_edit:
                if file_ref not in metadata['files_edited'] and len(metadata['files_edited']) < MAX_FILES_EDITED:
                    metadata['files_edited'].append(file_ref)
            else:
                if file_ref not in metadata['files_analyzed'] and len(metadata['files_analyzed']) < MAX_FILES_ANALYZED:
                    metadata['files_analyzed'].append(file_ref)

        # Check path field (for non-edit tools)
        if 'path' in input_data and not is_edit:
            file_ref = input_data['path']
            if file_ref not in metadata['files_analyzed'] and len(metadata['files_analyzed']) < MAX_FILES_ANALYZED:
                metadata['files_analyzed'].append(file_ref)


class ToolResultProcessor(MessageProcessor):
    """Process tool result messages."""

    def process(self, item: Any, metadata: Dict[str, Any]) -> Optional[str]:
        """Process tool results."""
        # Handle both dict items and top-level tool results
        if isinstance(item, dict):
            if item.get('type') == 'tool_result':
                result_content = str(item.get('content', ''))[:1000]
                return f"[Result] {result_content}"
            elif item.get('type') == 'tool_use':
                # Already handled by ToolMessageProcessor
                return None

        return None


class MessageProcessorFactory:
    """Factory for creating appropriate message processors."""

    def __init__(self):
        self.processors = {
            'text': TextMessageProcessor(),
            'thinking': ThinkingMessageProcessor(),
            'tool_use': ToolMessageProcessor(),
            'tool_result': ToolResultProcessor()
        }

    def get_processor(self, message_type: str) -> Optional[MessageProcessor]:
        """Get the appropriate processor for a message type."""
        return self.processors.get(message_type)

    def process_content(self, content: Any, metadata: Dict[str, Any]) -> str:
        """Process content of various types and return text representation."""
        text_parts = []

        if isinstance(content, list):
            for item in content:
                if isinstance(item, dict):
                    item_type = item.get('type', '')
                    processor = self.get_processor(item_type)
                    if processor:
                        text = processor.process(item, metadata)
                        if text:
                            text_parts.append(text)
                elif isinstance(item, str):
                    text_parts.append(item)
        elif isinstance(content, str):
            text_parts.append(content)

        return '\n'.join(text_parts)


def extract_ast_elements(code_text: str) -> Set[str]:
    """Extract AST elements from Python code."""
    elements = set()

    try:
        tree = ast.parse(code_text)
        for node in ast.walk(tree):
            if isinstance(node, ast.FunctionDef):
                elements.add(f"func:{node.name}")
            elif isinstance(node, ast.ClassDef):
                elements.add(f"class:{node.name}")
            elif isinstance(node, ast.Import):
                for alias in node.names:
                    elements.add(f"import:{alias.name}")
            elif isinstance(node, ast.ImportFrom):
                module = node.module or ''
                for alias in node.names:
                    elements.add(f"from:{module}.{alias.name}")
    except (SyntaxError, ValueError):
        # Not Python code or invalid syntax
        pass

    return elements


def extract_concepts(text: str) -> List[str]:
    """Extract key concepts from text using simple heuristics."""
    concepts = []

    # Common programming concepts
    concept_patterns = [
        (r'\b(async|await|promise|future)\b', 'async-programming'),
        (r'\b(test|spec|jest|pytest|unittest)\b', 'testing'),
        (r'\b(docker|container|kubernetes|k8s)\b', 'containerization'),
        (r'\b(api|rest|graphql|endpoint)\b', 'api-development'),
        (r'\b(react|vue|angular|svelte)\b', 'frontend-framework'),
        (r'\b(database|sql|postgres|mysql|mongodb)\b', 'database'),
        (r'\b(auth|authentication|oauth|jwt)\b', 'authentication'),
        (r'\b(error|exception|bug|fix)\b', 'debugging'),
        (r'\b(refactor|optimize|performance)\b', 'optimization'),
        (r'\b(deploy|ci|cd|pipeline)\b', 'deployment')
    ]

    text_lower = text.lower()
    seen_concepts = set()

    for pattern, concept in concept_patterns:
        if re.search(pattern, text_lower) and concept not in seen_concepts:
            concepts.append(concept)
            seen_concepts.add(concept)
            if len(concepts) >= MAX_CONCEPTS:
                break

    return concepts