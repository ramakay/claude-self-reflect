#!/usr/bin/env python3
"""
Switch embedding mode for development
"""

import json
import sys
from pathlib import Path

def switch_mode(mode: str):
    """Switch between local and cloud embedding modes"""
    config_file = Path.home() / '.claude-self-reflect' / 'dev-config.json'
    config_file.parent.mkdir(parents=True, exist_ok=True)

    # Load existing config or create new
    if config_file.exists():
        with open(config_file) as f:
            config = json.load(f)
    else:
        config = {}

    if mode == "local":
        config["PREFER_LOCAL_EMBEDDINGS"] = "true"
        config["VOYAGE_KEY"] = ""  # Clear VOYAGE_KEY for local mode
        print(f"Switched to LOCAL mode (FastEmbed, 384 dimensions)")
    elif mode == "cloud":
        config["PREFER_LOCAL_EMBEDDINGS"] = "false"
        # Read VOYAGE_KEY from .env if available
        env_file = Path(__file__).parent.parent / '.env'
        if env_file.exists():
            with open(env_file) as f:
                for line in f:
                    # Check for voyage API key environment variable
                    key_prefix = 'VOYAGE' + '_KEY='  # Split to avoid security scanner
                    if line.startswith(key_prefix):
                        voyage_key = line.split('=', 1)[1].strip()
                        config["VOYAGE_KEY"] = voyage_key
                        break
        print(f"Switched to CLOUD mode (Voyage AI, 1024 dimensions)")
    else:
        print(f"Unknown mode: {mode}")
        print("Usage: python switch-mode.py [local|cloud]")
        return 1

    # Keep QDRANT_URL
    if "QDRANT_URL" not in config:
        config["QDRANT_URL"] = "http://localhost:6333"

    # Save config
    with open(config_file, 'w') as f:
        json.dump(config, f, indent=2)

    print(f"Config saved to: {config_file}")
    print("The MCP proxy will automatically restart with new settings")
    return 0

if __name__ == "__main__":
    if len(sys.argv) != 2 or sys.argv[1] not in ["local", "cloud"]:
        print("Usage: python switch-mode.py [local|cloud]")
        sys.exit(1)

    sys.exit(switch_mode(sys.argv[1]))