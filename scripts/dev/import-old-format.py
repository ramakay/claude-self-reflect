#!/usr/bin/env python3
"""
Import old format JSONL files from Claude conversations.
These files have a different structure with type/summary fields instead of messages.
"""

import json
import sys
from pathlib import Path
import hashlib
import uuid
from datetime import datetime
from qdrant_client import QdrantClient
from qdrant_client.models import Distance, VectorParams, PointStruct
from fastembed import TextEmbedding
import logging

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

def import_old_format_project(project_dir: Path, project_path: str = None):
    """Import old format JSONL files from a project directory."""
    
    # Initialize
    client = QdrantClient(url='http://localhost:6333')
    model = TextEmbedding(model_name='sentence-transformers/all-MiniLM-L6-v2', max_length=512)
    
    # Determine project path from directory name if not provided
    if not project_path:
        # Convert -Users-username-projects-projectname back to path
        dir_name = project_dir.name
        project_path = '/' + dir_name.strip('-').replace('-', '/')
    
    # Create collection name
    project_hash = hashlib.md5(project_path.encode()).hexdigest()[:8]
    collection_name = f'conv_{project_hash}_local'
    
    logger.info(f'Project: {project_path}')
    logger.info(f'Collection: {collection_name}')
    
    # Create collection if needed
    try:
        client.get_collection(collection_name)
        logger.info('Collection exists')
    except:
        client.create_collection(
            collection_name=collection_name,
            vectors_config=VectorParams(size=384, distance=Distance.COSINE)
        )
        logger.info('Created collection')
    
    # Process all JSONL files
    jsonl_files = list(project_dir.glob('*.jsonl'))
    logger.info(f'Found {len(jsonl_files)} files to import')
    
    total_points = 0
    for file_path in jsonl_files:
        logger.info(f'Processing {file_path.name}...')
        points_batch = []
        
        with open(file_path, 'r', encoding='utf-8') as f:
            conversation_text = []
            file_timestamp = file_path.stat().st_mtime
            
            for line_num, line in enumerate(f, 1):
                try:
                    data = json.loads(line)
                    msg_type = data.get('type', '')
                    
                    # Extract text content based on type
                    content = None
                    if msg_type == 'summary' and data.get('summary'):
                        content = f"[Conversation Summary] {data['summary']}"
                    elif msg_type == 'user' and data.get('summary'):
                        content = f"User: {data['summary']}"
                    elif msg_type == 'assistant' and data.get('summary'):
                        content = f"Assistant: {data['summary']}"
                    elif msg_type in ['user', 'assistant']:
                        # Try to get content from other fields
                        if 'content' in data:
                            content = f"{msg_type.title()}: {data['content']}"
                        elif 'text' in data:
                            content = f"{msg_type.title()}: {data['text']}"
                    
                    if content:
                        conversation_text.append(content)
                        
                        # Create chunks every 5 messages or at end
                        if len(conversation_text) >= 5:
                            chunk_text = '\n\n'.join(conversation_text)
                            if chunk_text.strip():
                                # Generate embedding
                                embedding = list(model.embed([chunk_text[:2000]]))[0]  # Limit to 2000 chars
                                
                                point = PointStruct(
                                    id=str(uuid.uuid4()),
                                    vector=embedding.tolist(),
                                    payload={
                                        'content': chunk_text[:1000],  # Store first 1000 chars
                                        'full_content': chunk_text[:4000],  # Store more for context
                                        'project_path': project_path,
                                        'file_path': str(file_path),
                                        'file_name': file_path.name,
                                        'conversation_id': file_path.stem,
                                        'chunk_index': len(points_batch),
                                        'timestamp': file_timestamp,
                                        'type': 'conversation_chunk'
                                    }
                                )
                                points_batch.append(point)
                                conversation_text = []
                
                except json.JSONDecodeError:
                    logger.warning(f'Invalid JSON at line {line_num} in {file_path.name}')
                except Exception as e:
                    logger.warning(f'Error processing line {line_num}: {e}')
            
            # Handle remaining text
            if conversation_text:
                chunk_text = '\n\n'.join(conversation_text)
                if chunk_text.strip():
                    embedding = list(model.embed([chunk_text[:2000]]))[0]
                    
                    point = PointStruct(
                        id=str(uuid.uuid4()),
                        vector=embedding.tolist(),
                        payload={
                            'content': chunk_text[:1000],
                            'full_content': chunk_text[:4000],
                            'project_path': project_path,
                            'file_path': str(file_path),
                            'file_name': file_path.name,
                            'conversation_id': file_path.stem,
                            'chunk_index': len(points_batch),
                            'timestamp': file_timestamp,
                            'type': 'conversation_chunk'
                        }
                    )
                    points_batch.append(point)
        
        # Upload batch
        if points_batch:
            client.upsert(collection_name=collection_name, points=points_batch)
            logger.info(f'  Uploaded {len(points_batch)} chunks from {file_path.name}')
            total_points += len(points_batch)
    
    # Verify
    info = client.get_collection(collection_name)
    logger.info(f'\nImport complete!')
    logger.info(f'Collection {collection_name} now has {info.points_count} points')
    logger.info(f'Added {total_points} new points in this import')
    
    return collection_name, total_points

def main():
    if len(sys.argv) < 2:
        print("Usage: python import-old-format.py <project-directory> [project-path]")
        print("Example: python import-old-format.py ~/.claude/projects/-Users-me-projects-myapp /Users/me/projects/myapp")
        sys.exit(1)
    
    project_dir = Path(sys.argv[1]).expanduser()
    project_path = sys.argv[2] if len(sys.argv) > 2 else None
    
    if not project_dir.exists():
        print(f"Error: Directory {project_dir} does not exist")
        sys.exit(1)
    
    import_old_format_project(project_dir, project_path)

if __name__ == "__main__":
    main()