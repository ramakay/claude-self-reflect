#!/usr/bin/env python3
"""
Diagnostic script to check Claude Self-Reflect installation and identify issues.
"""

import os
import sys
import json
import subprocess
from pathlib import Path
from typing import Dict, List, Tuple
import urllib.request
import urllib.error
from datetime import datetime

class Colors:
    """Terminal colors for output"""
    GREEN = '\033[92m'
    YELLOW = '\033[93m'
    RED = '\033[91m'
    BLUE = '\033[94m'
    ENDC = '\033[0m'
    BOLD = '\033[1m'

def print_header(text: str):
    """Print a section header"""
    print(f"\n{Colors.BOLD}{Colors.BLUE}{'='*60}{Colors.ENDC}")
    print(f"{Colors.BOLD}{Colors.BLUE}{text}{Colors.ENDC}")
    print(f"{Colors.BOLD}{Colors.BLUE}{'='*60}{Colors.ENDC}")

def print_status(name: str, status: bool, message: str = ""):
    """Print a status line with colored indicator"""
    icon = f"{Colors.GREEN}✅{Colors.ENDC}" if status else f"{Colors.RED}❌{Colors.ENDC}"
    status_text = f"{Colors.GREEN}OK{Colors.ENDC}" if status else f"{Colors.RED}FAILED{Colors.ENDC}"
    print(f"{icon} {name}: {status_text}")
    if message:
        print(f"   {Colors.YELLOW}{message}{Colors.ENDC}")

def check_docker() -> Tuple[bool, str]:
    """Check if Docker is installed and running"""
    try:
        result = subprocess.run(['docker', 'info'], capture_output=True, text=True)
        if result.returncode == 0:
            # Check docker compose v2
            compose_result = subprocess.run(['docker', 'compose', 'version'], capture_output=True, text=True)
            if compose_result.returncode == 0:
                return True, "Docker and Docker Compose v2 are running"
            else:
                return False, "Docker Compose v2 not found. Please update Docker Desktop"
        else:
            return False, "Docker is not running"
    except FileNotFoundError:
        return False, "Docker is not installed"

def check_qdrant() -> Tuple[bool, str]:
    """Check if Qdrant is running and accessible"""
    try:
        req = urllib.request.Request('http://localhost:6333')
        with urllib.request.urlopen(req, timeout=5) as response:
            if response.status == 200:
                data = json.loads(response.read().decode())
                version = data.get('version', 'unknown')
                return True, f"Qdrant {version} is running on port 6333"
    except:
        pass
    return False, "Qdrant is not accessible on localhost:6333"

def check_collections() -> Tuple[bool, str, List[str]]:
    """Check if Qdrant has any collections"""
    try:
        req = urllib.request.Request('http://localhost:6333/collections')
        with urllib.request.urlopen(req, timeout=5) as response:
            if response.status == 200:
                data = json.loads(response.read().decode())
                collections = data.get('result', {}).get('collections', [])
                if collections:
                    collection_names = [c['name'] for c in collections]
                    return True, f"Found {len(collections)} collections", collection_names
                else:
                    return False, "No collections found - import may not have run", []
    except:
        pass
    return False, "Could not query Qdrant collections", []

def check_claude_projects() -> Tuple[bool, str, Dict]:
    """Check Claude projects directory for JSONL files"""
    claude_dir = Path.home() / '.claude' / 'projects'
    stats = {
        'total_projects': 0,
        'total_files': 0,
        'total_size': 0,
        'sample_projects': []
    }
    
    if not claude_dir.exists():
        return False, f"Claude projects directory not found: {claude_dir}", stats
    
    try:
        projects = list(claude_dir.iterdir())
        for project in projects:
            if project.is_dir():
                jsonl_files = list(project.glob('*.jsonl'))
                if jsonl_files:
                    stats['total_projects'] += 1
                    stats['total_files'] += len(jsonl_files)
                    for f in jsonl_files:
                        stats['total_size'] += f.stat().st_size
                    if len(stats['sample_projects']) < 3:
                        stats['sample_projects'].append(project.name)
        
        if stats['total_files'] == 0:
            return False, "No JSONL files found in Claude projects", stats
        
        size_mb = stats['total_size'] / (1024 * 1024)
        return True, f"Found {stats['total_files']} files across {stats['total_projects']} projects ({size_mb:.1f} MB)", stats
    except Exception as e:
        return False, f"Error scanning Claude projects: {e}", stats

def check_import_state() -> Tuple[bool, str, Dict]:
    """Check the import state file"""
    config_dir = Path.home() / '.claude-self-reflect' / 'config'
    state_file = config_dir / 'imported-files.json'
    
    stats = {
        'imported_count': 0,
        'last_import': None,
        'has_metadata': False
    }
    
    if not state_file.exists():
        return False, "No import state file found - imports haven't run yet", stats
    
    try:
        with open(state_file) as f:
            state = json.load(f)
        
        imported = state.get('imported_files', {})
        stats['imported_count'] = len(imported)
        
        # Check for metadata (new format)
        for file_path, data in imported.items():
            if isinstance(data, dict):
                stats['has_metadata'] = True
                if data.get('imported_at'):
                    import_time = data['imported_at']
                    if not stats['last_import'] or import_time > stats['last_import']:
                        stats['last_import'] = import_time
            elif isinstance(data, str):
                # Old format
                if not stats['last_import'] or data > stats['last_import']:
                    stats['last_import'] = data
        
        if stats['imported_count'] == 0:
            return False, "Import state exists but no files imported", stats
        
        msg = f"Imported {stats['imported_count']} files"
        if stats['last_import']:
            msg += f" (last: {stats['last_import'][:19]})"
        if not stats['has_metadata']:
            msg += " - OLD FORMAT (consider re-importing for metadata)"
        
        return True, msg, stats
    except Exception as e:
        return False, f"Error reading import state: {e}", stats

def check_env_file() -> Tuple[bool, str, Dict]:
    """Check .env file configuration"""
    env_file = Path('.env')
    config = {
        'has_voyage_key': False,
        'prefer_local': True,
        'claude_logs_path': None,
        'config_path': None
    }
    
    if not env_file.exists():
        return False, ".env file not found", config
    
    try:
        with open(env_file) as f:
            content = f.read()
        
        for line in content.split('\n'):
            if '=' in line and not line.startswith('#'):
                key, value = line.split('=', 1)
                key = key.strip()
                value = value.strip()
                
                if key == 'VOYAGE_KEY' and value and not value.startswith('your-'):
                    config['has_voyage_key'] = True
                elif key == 'PREFER_LOCAL_EMBEDDINGS':
                    config['prefer_local'] = value.lower() == 'true'
                elif key == 'CLAUDE_LOGS_PATH':
                    config['claude_logs_path'] = value
                elif key == 'CONFIG_PATH':
                    config['config_path'] = value
        
        # Check critical paths
        issues = []
        if config['claude_logs_path'] and '~' in config['claude_logs_path']:
            issues.append("CLAUDE_LOGS_PATH contains ~ which Docker won't expand")
        if config['config_path'] and '~' in config['config_path']:
            issues.append("CONFIG_PATH contains ~ which Docker won't expand")
        
        if issues:
            return False, "; ".join(issues), config
        
        mode = "Local embeddings" if config['prefer_local'] else "Voyage AI embeddings"
        return True, f"Configured for {mode}", config
    except Exception as e:
        return False, f"Error reading .env: {e}", config

def check_docker_containers() -> Tuple[bool, str, List[str]]:
    """Check which Docker containers are running"""
    try:
        result = subprocess.run(
            ['docker', 'compose', 'ps', '--format', 'json'],
            capture_output=True, text=True, cwd='.'
        )
        
        if result.returncode != 0:
            return False, "Could not query Docker containers", []
        
        running = []
        lines = result.stdout.strip().split('\n')
        for line in lines:
            if line:
                try:
                    container = json.loads(line)
                    if container.get('State') == 'running':
                        running.append(container.get('Service', 'unknown'))
                except:
                    pass
        
        if not running:
            return False, "No containers running", []
        
        essential = ['qdrant']
        missing = [s for s in essential if s not in running]
        
        if missing:
            return False, f"Essential services not running: {', '.join(missing)}", running
        
        return True, f"Running: {', '.join(running)}", running
    except Exception as e:
        return False, f"Error checking containers: {e}", []

def main():
    """Run all diagnostic checks"""
    print(f"{Colors.BOLD}{Colors.BLUE}")
    print("╔════════════════════════════════════════════════════════╗")
    print("║     Claude Self-Reflect Diagnostic Tool v1.0          ║")
    print("╚════════════════════════════════════════════════════════╝")
    print(f"{Colors.ENDC}")
    
    # Basic checks
    print_header("1. Environment Checks")
    
    docker_ok, docker_msg = check_docker()
    print_status("Docker", docker_ok, docker_msg)
    
    env_ok, env_msg, env_config = check_env_file()
    print_status("Environment (.env)", env_ok, env_msg)
    
    # Service checks
    print_header("2. Service Status")
    
    containers_ok, containers_msg, running_containers = check_docker_containers()
    print_status("Docker Containers", containers_ok, containers_msg)
    
    qdrant_ok, qdrant_msg = check_qdrant()
    print_status("Qdrant Database", qdrant_ok, qdrant_msg)
    
    # Data checks
    print_header("3. Data & Import Status")
    
    claude_ok, claude_msg, claude_stats = check_claude_projects()
    print_status("Claude Projects", claude_ok, claude_msg)
    if claude_stats['sample_projects']:
        print(f"   Sample projects: {', '.join(claude_stats['sample_projects'][:3])}")
    
    import_ok, import_msg, import_stats = check_import_state()
    print_status("Import State", import_ok, import_msg)
    
    collections_ok, collections_msg, collection_list = check_collections()
    print_status("Qdrant Collections", collections_ok, collections_msg)
    if collection_list:
        print(f"   Collections: {', '.join(collection_list[:5])}")
    
    # Summary and recommendations
    print_header("4. Summary & Recommendations")
    
    all_ok = all([docker_ok, env_ok, qdrant_ok, claude_ok])
    
    if all_ok and collections_ok:
        print(f"{Colors.GREEN}✅ System appears to be working correctly!{Colors.ENDC}")
    else:
        print(f"{Colors.YELLOW}⚠️  Issues detected:{Colors.ENDC}")
        
        if not docker_ok:
            print(f"\n{Colors.RED}Critical:{Colors.ENDC} Docker is required")
            print("  → Install Docker Desktop from https://docker.com")
        
        if env_ok and '~' in str(env_config.get('claude_logs_path', '')):
            print(f"\n{Colors.RED}Critical:{Colors.ENDC} Path expansion issue in .env")
            print("  → Run: claude-self-reflect setup")
            print("  → Or manually fix paths in .env to use full paths")
        
        if not qdrant_ok and docker_ok:
            print(f"\n{Colors.YELLOW}Issue:{Colors.ENDC} Qdrant not running")
            print("  → Run: docker compose --profile mcp up -d")
        
        if claude_ok and not collections_ok:
            print(f"\n{Colors.YELLOW}Issue:{Colors.ENDC} No collections found but JSONL files exist")
            print("  → Run: docker compose run --rm importer")
            print("  → This will import your conversation history")
        
        if not claude_ok:
            print(f"\n{Colors.YELLOW}Note:{Colors.ENDC} No Claude conversations found")
            print("  → This is normal if you haven't used Claude Desktop yet")
            print("  → The watcher will import new conversations automatically")
    
    # Quick commands
    print_header("5. Quick Commands")
    print("• Start services:  docker compose --profile mcp --profile watch up -d")
    print("• Import conversations:  docker compose run --rm importer")
    print("• View logs:  docker compose logs -f")
    print("• Check status:  claude-self-reflect status")
    print("• Restart everything:  docker compose down && docker compose --profile mcp --profile watch up -d")
    
    print(f"\n{Colors.BLUE}Documentation: https://github.com/ramakay/claude-self-reflect{Colors.ENDC}")
    
    # Return exit code based on critical issues
    if not docker_ok:
        sys.exit(1)
    if all_ok:
        sys.exit(0)
    else:
        sys.exit(2)  # Non-critical issues

if __name__ == "__main__":
    main()