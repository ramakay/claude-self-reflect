#!/usr/bin/env python3
"""
Health endpoint for streaming watcher monitoring.
Provides HTTP endpoint for health checks and metrics.
"""

import json
import time
import psutil
from pathlib import Path
from aiohttp import web
import asyncio
import logging

logger = logging.getLogger(__name__)

class WatcherHealthServer:
    """Simple HTTP health endpoint for watcher monitoring."""
    
    def __init__(self, watcher, port=8080):
        self.watcher = watcher
        self.port = port
        self.app = web.Application()
        self.setup_routes()
        self.start_time = time.time()
        
    def setup_routes(self):
        """Setup HTTP routes."""
        self.app.router.add_get('/health', self.health_check)
        self.app.router.add_get('/metrics', self.metrics)
        self.app.router.add_get('/ready', self.readiness_check)
        
    async def health_check(self, request):
        """Liveness probe - is the process alive?"""
        try:
            # Basic health - can we respond?
            health = {
                "status": "healthy",
                "timestamp": time.time(),
                "uptime_seconds": time.time() - self.start_time
            }
            return web.json_response(health)
        except Exception as e:
            return web.json_response(
                {"status": "unhealthy", "error": str(e)},
                status=503
            )
    
    async def readiness_check(self, request):
        """Readiness probe - are we ready to process files?"""
        try:
            # Check if we can connect to Qdrant
            collections = await self.watcher.qdrant_store.client.get_collections()
            
            # Check if we've processed anything recently (within 30 min)
            last_success = self.watcher.stats.get("last_success_time", 0)
            time_since_success = time.time() - last_success
            
            # Check memory usage
            process = psutil.Process()
            memory_mb = process.memory_info().rss / 1024 / 1024
            
            ready = {
                "ready": True,
                "qdrant_connected": collections is not None,
                "memory_mb": round(memory_mb, 2),
                "time_since_last_success": round(time_since_success, 2)
            }
            
            # Not ready if:
            # - Can't connect to Qdrant
            # - Memory > 2GB
            # - No success in 30 minutes (if we've been running that long)
            if not collections:
                ready["ready"] = False
                ready["reason"] = "Qdrant not accessible"
            elif memory_mb > 2048:
                ready["ready"] = False
                ready["reason"] = f"Memory too high: {memory_mb}MB"
            elif time_since_success > 1800 and (time.time() - self.start_time) > 1800:
                ready["ready"] = False
                ready["reason"] = "No successful imports in 30 minutes"
            
            status = 200 if ready["ready"] else 503
            return web.json_response(ready, status=status)
            
        except Exception as e:
            return web.json_response(
                {"ready": False, "error": str(e)},
                status=503
            )
    
    async def metrics(self, request):
        """Detailed metrics endpoint."""
        try:
            process = psutil.Process()
            
            metrics = {
                "timestamp": time.time(),
                "uptime_seconds": time.time() - self.start_time,
                "stats": dict(self.watcher.stats),
                "queue": {
                    "hot_count": len([f for f, cat, _ in self.watcher.categorized_files if cat == "HOT"]),
                    "warm_count": len([f for f, cat, _ in self.watcher.categorized_files if cat == "WARM"]),
                    "cold_count": len([f for f, cat, _ in self.watcher.categorized_files if cat == "COLD"]),
                    "total": len(self.watcher.categorized_files)
                },
                "memory": {
                    "rss_mb": round(process.memory_info().rss / 1024 / 1024, 2),
                    "vms_mb": round(process.memory_info().vms / 1024 / 1024, 2),
                    "percent": round(process.memory_percent(), 2)
                },
                "cpu": {
                    "percent": process.cpu_percent(interval=0.1),
                    "throttled": self.watcher.cpu_monitor.should_throttle()
                },
                "file_first_seen_count": len(self.watcher.file_first_seen),
                "last_import": {
                    "time": self.watcher.stats.get("last_success_time", 0),
                    "file": self.watcher.stats.get("last_success_file", "none")
                }
            }
            
            return web.json_response(metrics)
            
        except Exception as e:
            return web.json_response(
                {"error": str(e)},
                status=500
            )
    
    async def start(self):
        """Start the health server."""
        runner = web.AppRunner(self.app)
        await runner.setup()
        site = web.TCPSite(runner, '0.0.0.0', self.port)
        await site.start()
        logger.info(f"Health endpoint started on port {self.port}")
        logger.info(f"  - Liveness:  http://localhost:{self.port}/health")
        logger.info(f"  - Readiness: http://localhost:{self.port}/ready")
        logger.info(f"  - Metrics:   http://localhost:{self.port}/metrics")


# Integration with streaming-watcher.py:
# Add to StreamingWatcher.__init__:
#   self.health_server = WatcherHealthServer(self, port=8080)
#
# Add to StreamingWatcher.run():
#   await self.health_server.start()
#
# Update stats tracking:
#   self.stats["last_success_time"] = time.time()
#   self.stats["last_success_file"] = str(jsonl_file)