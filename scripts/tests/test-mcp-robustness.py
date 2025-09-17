#!/usr/bin/env python3
"""Test MCP server robustness - verifies the fix for hanging downloads."""

import os
import sys
import subprocess
import time
import json
import shutil
from pathlib import Path
import tempfile
import signal
import logging

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class MCPRobustnessTest:
    def __init__(self):
        self.project_root = Path(__file__).parent.parent
        self.mcp_dir = self.project_root / "mcp-server"
        self.cache_dir = self.mcp_dir / ".fastembed-cache"
        self.run_script = self.mcp_dir / "run-mcp.sh"
        self.test_results = []
        
    def cleanup_cache(self):
        """Remove cache to test fresh download."""
        if self.cache_dir.exists():
            logger.info(f"Removing cache directory: {self.cache_dir}")
            shutil.rmtree(self.cache_dir)
        else:
            logger.info("Cache directory doesn't exist")
    
    def create_stale_locks(self):
        """Create stale lock files to test cleanup."""
        locks_dir = self.cache_dir / ".locks" / "models--qdrant--all-MiniLM-L6-v2-onnx"
        locks_dir.mkdir(parents=True, exist_ok=True)
        
        # Create some fake lock files
        for i in range(3):
            lock_file = locks_dir / f"test_{i}.lock"
            lock_file.touch()
            # Make them old (set mtime to 10 minutes ago)
            old_time = time.time() - 600
            os.utime(lock_file, (old_time, old_time))
        
        logger.info(f"Created {len(list(locks_dir.glob('*.lock')))} stale lock files")
        return locks_dir
    
    def test_mcp_startup(self, test_name, timeout=30, env_vars=None):
        """Test if MCP server starts without hanging."""
        logger.info(f"\n=== Test: {test_name} ===")
        
        # Prepare environment
        env = os.environ.copy()
        env['PYTHONUNBUFFERED'] = '1'
        if env_vars:
            env.update(env_vars)
            logger.info(f"Environment: {env_vars}")
        
        # Create test request
        test_request = json.dumps({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {
                "protocolVersion": "1.0.0",
                "clientInfo": {"name": "test", "version": "1.0"}
            },
            "id": 1
        })
        
        try:
            # Start MCP server
            logger.info(f"Starting MCP server (timeout: {timeout}s)...")
            process = subprocess.Popen(
                [str(self.run_script)],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=env,
                text=True
            )
            
            # Send test request
            stdout, stderr = process.communicate(
                input=test_request + "\n",
                timeout=timeout
            )
            
            # Check for success indicators
            success = False
            if "Embedding manager initialized" in stderr:
                logger.info("✓ Embedding manager initialized")
                success = True
            elif "Model loaded" in stderr:
                logger.info("✓ Model loaded")
                success = True
            elif '"result"' in stdout:
                logger.info("✓ MCP responded to initialization")
                success = True
            elif "Failed to initialize" in stderr:
                logger.error("✗ Initialization failed")
                logger.error(f"Stderr: {stderr[:500]}")
            
            # Check for hanging indicators
            if "Fetching 5 files:   0%" in stderr and "100%" not in stderr:
                logger.error("✗ Download appears to be hanging!")
                success = False
            
            if "Lock.*not acquired" in stderr:
                logger.error("✗ File lock issues detected!")
                success = False
                
            self.test_results.append({
                "test": test_name,
                "success": success,
                "duration": timeout,
                "stdout_sample": stdout[:200] if stdout else None,
                "stderr_sample": stderr[:500] if stderr else None
            })
            
            return success
            
        except subprocess.TimeoutExpired:
            logger.error(f"✗ MCP server hung after {timeout}s!")
            process.kill()
            self.test_results.append({
                "test": test_name,
                "success": False,
                "error": "Timeout",
                "duration": timeout
            })
            return False
        except Exception as e:
            logger.error(f"✗ Test failed: {e}")
            self.test_results.append({
                "test": test_name,
                "success": False,
                "error": str(e)
            })
            return False
    
    def run_all_tests(self):
        """Run comprehensive test suite."""
        logger.info("Starting MCP Robustness Test Suite")
        logger.info("=" * 50)
        
        # Test 1: Fresh install with local embeddings
        logger.info("\n### Test 1: Fresh Install with Local Embeddings ###")
        self.cleanup_cache()
        success1 = self.test_mcp_startup(
            "Fresh install - Local",
            timeout=120,  # Allow time for download
            env_vars={
                "PREFER_LOCAL_EMBEDDINGS": "true",
                "FASTEMBED_SKIP_HUGGINGFACE": "true"
            }
        )
        
        # Test 2: Existing cache
        logger.info("\n### Test 2: Startup with Existing Cache ###")
        success2 = self.test_mcp_startup(
            "Existing cache",
            timeout=10,  # Should be fast
            env_vars={
                "PREFER_LOCAL_EMBEDDINGS": "true"
            }
        )
        
        # Test 3: Stale locks cleanup
        logger.info("\n### Test 3: Stale Lock Cleanup ###")
        locks_dir = self.create_stale_locks()
        success3 = self.test_mcp_startup(
            "Stale locks",
            timeout=15,
            env_vars={
                "PREFER_LOCAL_EMBEDDINGS": "true"
            }
        )
        # Check if locks were cleaned
        remaining_locks = len(list(locks_dir.glob('*.lock')))
        logger.info(f"Remaining locks after test: {remaining_locks}")
        
        # Test 4: Voyage AI preference (should try Voyage first)
        logger.info("\n### Test 4: Voyage AI Preference ###")
        success4 = self.test_mcp_startup(
            "Voyage preference",
            timeout=10,
            env_vars={
                "PREFER_LOCAL_EMBEDDINGS": "false",
                "VOYAGE_KEY": os.getenv("VOYAGE_KEY", "test-key")
            }
        )
        
        # Test 5: Local only - no fallback
        logger.info("\n### Test 5: Local Only (No Fallback) ###")
        success5 = self.test_mcp_startup(
            "Local only - no fallback",
            timeout=10,
            env_vars={
                "PREFER_LOCAL_EMBEDDINGS": "true",
                "VOYAGE_KEY": ""  # No Voyage key
            }
        )
        
        # Print summary
        logger.info("\n" + "=" * 50)
        logger.info("TEST SUMMARY")
        logger.info("=" * 50)
        
        total_tests = len(self.test_results)
        passed_tests = sum(1 for r in self.test_results if r.get("success"))
        
        for result in self.test_results:
            status = "✓ PASS" if result.get("success") else "✗ FAIL"
            logger.info(f"{status}: {result['test']}")
            if result.get("error"):
                logger.info(f"  Error: {result['error']}")
        
        logger.info(f"\nTotal: {passed_tests}/{total_tests} tests passed")
        
        # Check cache status
        logger.info(f"\nCache directory: {self.cache_dir}")
        if self.cache_dir.exists():
            cache_size = sum(f.stat().st_size for f in self.cache_dir.rglob('*') if f.is_file())
            logger.info(f"Cache size: {cache_size / (1024*1024):.2f} MB")
            model_dir = self.cache_dir / "models--qdrant--all-MiniLM-L6-v2-onnx"
            if model_dir.exists():
                logger.info("✓ Model cached successfully")
        
        return passed_tests == total_tests

def main():
    # Check prerequisites
    mcp_dir = Path(__file__).parent.parent / "mcp-server"
    if not (mcp_dir / "venv").exists():
        logger.error("Virtual environment not found. Please run setup first.")
        return 1
    
    if not (mcp_dir / "run-mcp.sh").exists():
        logger.error("run-mcp.sh not found.")
        return 1
    
    # Run tests
    tester = MCPRobustnessTest()
    success = tester.run_all_tests()
    
    if success:
        logger.info("\n🎉 All tests passed! MCP server is robust.")
        return 0
    else:
        logger.error("\n❌ Some tests failed. Please check the logs above.")
        return 1

if __name__ == "__main__":
    sys.exit(main())