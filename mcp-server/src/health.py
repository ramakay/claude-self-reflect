#!/usr/bin/env python3
"""
Health check endpoint for Claude Self-Reflect system

Provides a simple way to monitor system health and import status.
"""

import json
import os
import sys
from pathlib import Path
from datetime import datetime, timedelta
from typing import Dict, Any

# No sys.path modification needed - using subprocess for imports

def check_qdrant_health() -> Dict[str, Any]:
    """Check if Qdrant is accessible and has data"""
    try:
        from qdrant_client import QdrantClient
        from qdrant_client.http.exceptions import ResponseHandlingException
        
        # Add timeout for network operations
        client = QdrantClient(
            'localhost', 
            port=6333,
            timeout=5  # 5 second timeout
        )
        
        collections = client.get_collections().collections
        
        return {
            'status': 'healthy',
            'collections': len(collections),
            'accessible': True
        }
    except (ResponseHandlingException, ConnectionError, TimeoutError) as e:
        # Sanitize error messages to avoid information disclosure
        return {
            'status': 'unhealthy',
            'error': 'Connection failed',
            'accessible': False
        }
    except Exception as e:
        return {
            'status': 'unhealthy',
            'error': 'Service unavailable',
            'accessible': False
        }

def check_import_status() -> Dict[str, Any]:
    """Check import status from status.py"""
    try:
        # Run status.py and parse output
        import subprocess
        import json
        
        result = subprocess.run(
            [sys.executable, str(Path(__file__).parent / "status.py")],
            capture_output=True,
            text=True,
            timeout=10
        )
        
        if result.returncode == 0:
            status = json.loads(result.stdout)
            return {
                'percentage': status['overall']['percentage'],
                'indexed': status['overall']['indexed'],
                'total': status['overall']['total'],
                'backlog': status['overall']['backlog']
            }
        else:
            return {
                'error': result.stderr,
                'percentage': 0
            }
    except Exception as e:
        return {
            'error': str(e),
            'percentage': 0
        }

def check_watcher_status() -> Dict[str, Any]:
    """Check if Docker watcher is running"""
    try:
        import subprocess
        result = subprocess.run(
            ["docker", "ps", "--filter", "name=claude-reflection-safe-watcher", "--format", "{{.Status}}"],
            capture_output=True,
            text=True,
            timeout=5
        )
        
        if result.stdout and "Up" in result.stdout:
            return {
                'status': 'running',
                'details': result.stdout.strip()
            }
        else:
            return {
                'status': 'stopped',
                'details': 'Container not running'
            }
    except Exception as e:
        return {
            'status': 'unknown',
            'error': 'Docker check failed'
        }

def check_recent_imports() -> Dict[str, Any]:
    """Check for recent import activity"""
    try:
        import_file = Path.home() / ".claude-self-reflect" / "config" / "imported-files.json"
        if import_file.exists():
            mtime = datetime.fromtimestamp(import_file.stat().st_mtime)
            age = datetime.now() - mtime
            
            return {
                'last_import': mtime.isoformat(),
                'hours_ago': round(age.total_seconds() / 3600, 1),
                'active': age < timedelta(hours=24)
            }
        return {
            'last_import': None,
            'active': False
        }
    except Exception as e:
        return {
            'error': 'Import check failed',
            'active': False
        }

def get_health_status() -> Dict[str, Any]:
    """Get comprehensive health status"""
    
    # Collect all health checks
    qdrant = check_qdrant_health()
    imports = check_import_status()
    watcher = check_watcher_status()
    recent = check_recent_imports()
    
    # Determine overall health
    is_healthy = (
        qdrant.get('accessible', False) and
        imports.get('percentage', 0) > 95 and
        watcher.get('status') == 'running'
    )
    
    return {
        'timestamp': datetime.now().isoformat(),
        'healthy': is_healthy,
        'status': 'healthy' if is_healthy else 'degraded',
        'components': {
            'qdrant': qdrant,
            'imports': imports,
            'watcher': watcher,
            'recent_activity': recent
        },
        'summary': {
            'import_percentage': imports.get('percentage', 0),
            'collections': qdrant.get('collections', 0),
            'watcher_running': watcher.get('status') == 'running',
            'recent_imports': recent.get('active', False)
        }
    }

def main():
    """Main entry point for CLI usage"""
    try:
        health = get_health_status()
        
        # Pretty print
        print(json.dumps(health, indent=2))
        
        # Exit code based on health
        sys.exit(0 if health['healthy'] else 1)
        
    except Exception as e:
        error_response = {
            'timestamp': datetime.now().isoformat(),
            'healthy': False,
            'status': 'error',
            'error': 'Health check failed'
        }
        print(json.dumps(error_response, indent=2))
        sys.exit(2)

if __name__ == "__main__":
    main()