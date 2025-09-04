#!/usr/bin/env python3
"""
MCP tool to retrieve full conversation JSONL content
Provides agents with complete conversation context instead of truncated excerpts
"""

import json
from pathlib import Path
from typing import Optional, Dict, Any

async def get_full_conversation(conversation_id: str, project: Optional[str] = None) -> Dict[str, Any]:
    """
    Retrieve the full JSONL content for a conversation ID
    
    Args:
        conversation_id: The conversation ID from search results
        project: Optional project name to narrow search
    
    Returns:
        Dict containing full conversation data and metadata
    """
    
    # Base path for Claude conversations
    base_path = Path.home() / '.claude/projects'
    
    # If project specified, check that project directory
    if project:
        # Sanitize project name for directory lookup
        sanitized_project = project.replace('/', '-')
        project_dirs = [
            base_path / project,
            base_path / sanitized_project,
            base_path / f"-Users-ramakrishnanannaswamy-projects-{project}"
        ]
    else:
        # Search all project directories
        project_dirs = list(base_path.glob("*"))
    
    # Search for the JSONL file
    jsonl_path = None
    for proj_dir in project_dirs:
        if not proj_dir.is_dir():
            continue
            
        potential_path = proj_dir / f"{conversation_id}.jsonl"
        if potential_path.exists():
            jsonl_path = potential_path
            break
    
    if not jsonl_path:
        return {
            "error": f"Conversation {conversation_id} not found",
            "searched_dirs": [str(d) for d in project_dirs[:3]]
        }
    
    # Read and parse the JSONL file
    try:
        messages = []
        with open(jsonl_path, 'r', encoding='utf-8') as f:
            for line in f:
                if line.strip():
                    messages.append(json.loads(line))
        
        # Extract key patterns and code blocks
        code_blocks = []
        patterns_mentioned = []
        files_referenced = set()
        
        for msg in messages:
            content = str(msg.get('content', ''))
            
            # Extract code blocks
            import re
            code_pattern = r'```(?:([^\n`]*?)\n)?(.*?)```'
            for match in re.finditer(code_pattern, content, re.DOTALL):
                lang = match.group(1) or "unknown"
                code = match.group(2)
                if code.strip():
                    code_blocks.append({
                        "language": lang,
                        "code": code,
                        "length": len(code)
                    })
            
            # Extract file paths
            file_pattern = r'[/\w\-\.]+\.[a-zA-Z]{2,4}'
            for match in re.finditer(file_pattern, content):
                if '/' in match.group(0):
                    files_referenced.add(match.group(0))
        
        return {
            "success": True,
            "conversation_id": conversation_id,
            "file_path": str(jsonl_path),
            "file_size": jsonl_path.stat().st_size,
            "message_count": len(messages),
            "messages": messages,  # Full conversation
            "metadata": {
                "code_blocks": len(code_blocks),
                "files_referenced": list(files_referenced),
                "total_characters": sum(len(str(m.get('content', ''))) for m in messages),
                "has_code": len(code_blocks) > 0
            },
            "code_samples": code_blocks[:5],  # First 5 code blocks as samples
            "project": str(jsonl_path.parent.name)
        }
    
    except Exception as e:
        return {
            "error": f"Failed to read conversation: {str(e)}",
            "file_path": str(jsonl_path)
        }


if __name__ == "__main__":
    # Test with a known conversation ID
    import asyncio
    
    async def test():
        result = await get_full_conversation(
            "88de95ff-28da-4dc0-84c4-9f5aa47eb4b6",
            "procsolve-website"
        )
        
        if result.get("success"):
            print(f"✅ Successfully loaded conversation")
            print(f"  Messages: {result['message_count']}")
            print(f"  Code blocks: {result['metadata']['code_blocks']}")
            print(f"  Files referenced: {len(result['metadata']['files_referenced'])}")
            print(f"  Total size: {result['file_size']:,} bytes")
            
            # Show first code sample
            if result.get("code_samples"):
                sample = result["code_samples"][0]
                print(f"\nFirst code block ({sample['language']}):")
                print(sample['code'][:200] + "..." if len(sample['code']) > 200 else sample['code'])
        else:
            print(f"❌ Error: {result.get('error')}")
    
    asyncio.run(test())