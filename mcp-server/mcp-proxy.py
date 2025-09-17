#!/usr/bin/env python3
"""
MCP Proxy for Hot-Reload Development
Similar to mcpmon but for Python/FastMCP servers
"""

import os
import sys
import json
import signal
import asyncio
import subprocess
from pathlib import Path
from typing import Optional

class MCPProxy:
    """Proxy that allows hot-reload of MCP server with new environment variables"""

    def __init__(self):
        self.server_process: Optional[subprocess.Popen] = None
        self.config_file = Path.home() / '.claude-self-reflect' / 'dev-config.json'
        self.config_file.parent.mkdir(parents=True, exist_ok=True)
        self.running = True

    def load_config(self) -> dict:
        """Load configuration from file or use defaults"""
        if self.config_file.exists():
            with open(self.config_file) as f:
                return json.load(f)

        # Default config
        return {
            "PREFER_LOCAL_EMBEDDINGS": os.getenv("PREFER_LOCAL_EMBEDDINGS", "true"),
            "VOYAGE_KEY": os.getenv("VOYAGE_KEY", ""),
            "QDRANT_URL": os.getenv("QDRANT_URL", "http://localhost:6333")
        }

    def save_config(self, config: dict):
        """Save configuration to file"""
        with open(self.config_file, 'w') as f:
            json.dump(config, f, indent=2)

    async def start_server(self):
        """Start the MCP server with current configuration"""
        if self.server_process:
            await self.stop_server()

        config = self.load_config()

        # Build environment
        env = os.environ.copy()

        # Apply configuration
        for key, value in config.items():
            if value:
                env[key] = value
            elif key in env:
                # Clear the key if value is empty
                del env[key]

        # Special handling for local mode
        if config.get("PREFER_LOCAL_EMBEDDINGS") == "true":
            # Force local mode by removing VOYAGE_KEY
            if "VOYAGE_KEY" in env:
                del env["VOYAGE_KEY"]
                print(f"[PROXY] Local mode requested - VOYAGE_KEY cleared", file=sys.stderr)

        print(f"[PROXY] Starting server with config: {json.dumps(config, indent=2)}", file=sys.stderr)

        # Start the actual MCP server using run-mcp.sh which handles venv
        script_dir = Path(__file__).parent
        run_script = script_dir / "run-mcp.sh"

        # Make it executable
        run_script.chmod(0o755)

        self.server_process = subprocess.Popen(
            [str(run_script)],
            cwd=script_dir,
            env=env,
            stdin=sys.stdin,
            stdout=sys.stdout,
            stderr=subprocess.PIPE
        )

        # Forward stderr in background
        asyncio.create_task(self.forward_stderr())

    async def forward_stderr(self):
        """Forward stderr from server to our stderr"""
        if not self.server_process:
            return

        while self.server_process.poll() is None:
            line = self.server_process.stderr.readline()
            if line:
                sys.stderr.buffer.write(line)
                sys.stderr.flush()
            await asyncio.sleep(0.01)

    async def stop_server(self):
        """Stop the MCP server gracefully"""
        if self.server_process:
            print(f"[PROXY] Stopping server...", file=sys.stderr)
            self.server_process.terminate()
            try:
                await asyncio.wait_for(
                    asyncio.create_task(self.wait_for_process()),
                    timeout=5.0
                )
            except asyncio.TimeoutError:
                print(f"[PROXY] Force killing server...", file=sys.stderr)
                self.server_process.kill()
            self.server_process = None

    async def wait_for_process(self):
        """Wait for process to exit"""
        while self.server_process and self.server_process.poll() is None:
            await asyncio.sleep(0.1)

    async def watch_config(self):
        """Watch for configuration changes"""
        last_mtime = 0

        while self.running:
            try:
                if self.config_file.exists():
                    current_mtime = self.config_file.stat().st_mtime
                    if current_mtime > last_mtime and last_mtime > 0:
                        print(f"[PROXY] Config changed, restarting server...", file=sys.stderr)
                        await self.start_server()
                    last_mtime = current_mtime
            except Exception as e:
                print(f"[PROXY] Error watching config: {e}", file=sys.stderr)

            await asyncio.sleep(1)  # Check every second

    async def handle_signals(self):
        """Handle termination signals"""
        loop = asyncio.get_event_loop()

        def signal_handler(sig):
            print(f"[PROXY] Received signal {sig}, shutting down...", file=sys.stderr)
            self.running = False

        for sig in [signal.SIGTERM, signal.SIGINT]:
            loop.add_signal_handler(sig, lambda s=sig: signal_handler(s))

    async def run(self):
        """Main proxy loop"""
        # Set up signal handlers
        await self.handle_signals()

        # Start server with initial config
        await self.start_server()

        # Start config watcher
        watcher_task = asyncio.create_task(self.watch_config())

        # Wait for server to exit or shutdown signal
        while self.running and self.server_process:
            if self.server_process.poll() is not None:
                print(f"[PROXY] Server exited with code {self.server_process.returncode}", file=sys.stderr)
                if self.running:
                    print(f"[PROXY] Restarting server...", file=sys.stderr)
                    await self.start_server()
            await asyncio.sleep(0.5)

        # Cleanup
        watcher_task.cancel()
        await self.stop_server()
        print(f"[PROXY] Proxy shutdown complete", file=sys.stderr)

def main():
    """Entry point"""
    print(f"[PROXY] MCP Development Proxy starting...", file=sys.stderr)
    print(f"[PROXY] Config file: {Path.home() / '.claude-self-reflect' / 'dev-config.json'}", file=sys.stderr)
    print(f"[PROXY] Change config file to trigger restart", file=sys.stderr)

    proxy = MCPProxy()

    # Create default config file if it doesn't exist
    if not proxy.config_file.exists():
        default_config = {
            "PREFER_LOCAL_EMBEDDINGS": "true",
            "VOYAGE_KEY": "",
            "QDRANT_URL": "http://localhost:6333"
        }
        proxy.save_config(default_config)
        print(f"[PROXY] Created default config file", file=sys.stderr)

    asyncio.run(proxy.run())

if __name__ == "__main__":
    main()