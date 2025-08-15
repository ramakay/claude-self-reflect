#!/usr/bin/env python3
"""Pre-download FastEmbed models to avoid startup delays.

This script downloads the FastEmbed models ahead of time so the MCP server
doesn't hang on first startup. It can also copy models from Docker containers
if they already have them cached.
"""

import os
import sys
import shutil
import subprocess
from pathlib import Path
import argparse
import logging

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

def download_model(model_name: str = "sentence-transformers/all-MiniLM-L6-v2") -> bool:
    """Download FastEmbed model to local cache."""
    try:
        logger.info(f"Downloading model: {model_name}")
        
        # Import FastEmbed
        try:
            from fastembed import TextEmbedding
        except ImportError:
            logger.error("FastEmbed not installed. Please run: pip install fastembed")
            return False
        
        # Initialize model (this triggers download if needed)
        logger.info("Initializing model (this may take a few minutes on first run)...")
        model = TextEmbedding(model_name=model_name)
        
        # Test embedding to ensure it works
        logger.info("Testing model...")
        test_embedding = list(model.embed(["test"]))
        
        if test_embedding:
            logger.info(f"✓ Model downloaded and working: {model_name}")
            logger.info(f"  Embedding dimension: {len(test_embedding[0])}")
            return True
        else:
            logger.error("Model test failed")
            return False
            
    except Exception as e:
        logger.error(f"Failed to download model: {e}")
        return False

def copy_from_docker(container_name: str = "claude-reflection-streaming") -> bool:
    """Copy FastEmbed cache from Docker container if available."""
    try:
        logger.info(f"Checking if container '{container_name}' has cached models...")
        
        # Check if container exists and is running
        result = subprocess.run(
            ["docker", "ps", "--filter", f"name={container_name}", "--format", "{{.Names}}"],
            capture_output=True, text=True
        )
        
        if container_name not in result.stdout:
            logger.warning(f"Container '{container_name}' not running")
            return False
        
        # Check if cache exists in container
        cache_check = subprocess.run(
            ["docker", "exec", container_name, "ls", "-la", "/root/.cache/fastembed"],
            capture_output=True, text=True
        )
        
        if cache_check.returncode != 0:
            logger.warning("No FastEmbed cache found in container")
            return False
        
        # Copy cache from container
        local_cache = Path.home() / ".cache" / "fastembed"
        local_cache.parent.mkdir(parents=True, exist_ok=True)
        
        logger.info(f"Copying cache from container to {local_cache}...")
        
        # Remove existing cache if present
        if local_cache.exists():
            logger.info("Removing existing local cache...")
            shutil.rmtree(local_cache)
        
        # Copy from container
        copy_result = subprocess.run(
            ["docker", "cp", f"{container_name}:/root/.cache/fastembed", str(local_cache.parent)],
            capture_output=True, text=True
        )
        
        if copy_result.returncode == 0:
            logger.info("✓ Successfully copied cache from Docker container")
            return True
        else:
            logger.error(f"Failed to copy cache: {copy_result.stderr}")
            return False
            
    except Exception as e:
        logger.error(f"Error copying from Docker: {e}")
        return False

def check_cache_status() -> dict:
    """Check the status of the FastEmbed cache."""
    cache_dir = Path.home() / ".cache" / "fastembed"
    model_dir = cache_dir / "models--qdrant--all-MiniLM-L6-v2-onnx"
    
    status = {
        "cache_exists": cache_dir.exists(),
        "cache_path": str(cache_dir),
        "model_cached": model_dir.exists(),
        "model_path": str(model_dir),
        "size_mb": 0
    }
    
    if model_dir.exists():
        # Calculate total size
        total_size = sum(f.stat().st_size for f in model_dir.rglob('*') if f.is_file())
        status["size_mb"] = round(total_size / (1024 * 1024), 2)
    
    return status

def main():
    parser = argparse.ArgumentParser(description="Setup FastEmbed model cache")
    parser.add_argument(
        "--method", 
        choices=["download", "docker", "auto"],
        default="auto",
        help="Method to obtain model: download from HuggingFace, copy from Docker, or auto"
    )
    parser.add_argument(
        "--container",
        default="claude-reflection-streaming",
        help="Docker container name to copy from (if using docker method)"
    )
    parser.add_argument(
        "--model",
        default="sentence-transformers/all-MiniLM-L6-v2",
        help="Model name to download"
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Force re-download even if cache exists"
    )
    
    args = parser.parse_args()
    
    # Check current status
    logger.info("Checking cache status...")
    status = check_cache_status()
    
    if status["model_cached"] and not args.force:
        logger.info(f"✓ Model already cached at: {status['model_path']}")
        logger.info(f"  Cache size: {status['size_mb']} MB")
        logger.info("Use --force to re-download")
        return 0
    
    success = False
    
    if args.method == "auto":
        # Try Docker first (faster), then download
        logger.info("Trying to copy from Docker container first...")
        success = copy_from_docker(args.container)
        
        if not success:
            logger.info("Docker copy failed, downloading from HuggingFace...")
            success = download_model(args.model)
    
    elif args.method == "docker":
        success = copy_from_docker(args.container)
    
    elif args.method == "download":
        success = download_model(args.model)
    
    # Final status
    if success:
        final_status = check_cache_status()
        logger.info("\n=== Setup Complete ===")
        logger.info(f"Model cached at: {final_status['model_path']}")
        logger.info(f"Cache size: {final_status['size_mb']} MB")
        logger.info("\nThe MCP server should now start without delays!")
        return 0
    else:
        logger.error("\n=== Setup Failed ===")
        logger.error("Please check the errors above and try again")
        logger.error("You may need to:")
        logger.error("1. Check your internet connection")
        logger.error("2. Ensure Docker is running (for docker method)")
        logger.error("3. Install fastembed: pip install fastembed")
        return 1

if __name__ == "__main__":
    sys.exit(main())