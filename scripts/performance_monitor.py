#!/usr/bin/env python3
"""
Performance monitor for Claude and Docker VM Service.
Logs CPU, memory, and other metrics continuously to track performance issues.
"""

import subprocess
import time
import json
import signal
import sys
from datetime import datetime
from pathlib import Path
import threading
import os
import logging

# Set up logging
logging.basicConfig(level=logging.WARNING)
logger = logging.getLogger(__name__)

# Import psutil for process monitoring
try:
    import psutil
except ImportError:
    logger.error("psutil not installed. Install with: pip install psutil")
    psutil = None

class PerformanceMonitor:
    def __init__(self, log_dir="/tmp/claude-performance-logs"):
        self.log_dir = Path(log_dir)
        self.log_dir.mkdir(exist_ok=True)
        self.running = True
        self.start_time = datetime.now()
        self.log_file = self.log_dir / f"perf_{self.start_time.strftime('%Y%m%d_%H%M%S')}.jsonl"
        self.csv_file = self.log_dir / f"perf_{self.start_time.strftime('%Y%m%d_%H%M%S')}.csv"

        # Write CSV header
        with open(self.csv_file, 'w') as f:
            f.write("timestamp,process,cpu_percent,memory_mb,threads,ports,energy_impact\n")

        print(f"📊 Performance monitoring started")
        print(f"📁 Logs: {self.log_file}")
        print(f"📈 CSV: {self.csv_file}")
        print(f"🛑 Press Ctrl+C to stop\n")

    def get_process_stats(self):
        """Get stats for Claude and Docker VM using ps command."""
        try:
            # Use ps with compatible options (aux doesn't support -o on macOS)
            cmd = ['ps', 'aux']
            result = subprocess.run(cmd, capture_output=True, text=True, check=False)

            stats = {
                'timestamp': datetime.now().isoformat(),
                'claude': None,
                'docker_vm': None
            }

            # Parse ps output for our processes
            for line in result.stdout.split('\n'):
                if 'Claude' in line and 'Helper' not in line:
                    parts = line.split()
                    if len(parts) >= 6:
                        stats['claude'] = {
                            'cpu_percent': float(parts[2]) if parts[2] != '-' else 0.0,
                            'mem_percent': float(parts[3]) if parts[3] != '-' else 0.0,
                            'rss_kb': int(parts[4]) if parts[4].isdigit() else 0,
                            'memory_mb': int(parts[4]) / 1024 if parts[4].isdigit() else 0
                        }
                elif 'Docker' in line and 'Virtual' in line:
                    parts = line.split()
                    if len(parts) >= 6:
                        stats['docker_vm'] = {
                            'cpu_percent': float(parts[2]) if parts[2] != '-' else 0.0,
                            'mem_percent': float(parts[3]) if parts[3] != '-' else 0.0,
                            'rss_kb': int(parts[4]) if parts[4].isdigit() else 0,
                            'memory_mb': int(parts[4]) / 1024 if parts[4].isdigit() else 0
                        }

            # Try using top for more detailed stats
            self.enrich_with_top_stats(stats)

            return stats

        except subprocess.SubprocessError as e:
            logger.error(f"Subprocess error in performance monitoring: {e}")
            return {
                'timestamp': datetime.now().isoformat(),
                'error': f"Subprocess error: {e}"
            }
        except (ValueError, IndexError) as e:
            logger.warning(f"Parsing error in performance monitoring: {e}")
            return {
                'timestamp': datetime.now().isoformat(),
                'error': f"Parsing error: {e}"
            }
        except Exception as e:
            logger.error(f"Unexpected error in performance monitoring: {e}")
            return {
                'timestamp': datetime.now().isoformat(),
                'error': str(e)
            }

    def enrich_with_top_stats(self, stats):
        """Enrich stats using top command for better CPU measurements."""
        try:
            # Get top snapshot (using list args - no shell injection risk)
            cmd = ['top', '-l', '1', '-n', '10', '-stats', 'pid,command,cpu,mem,threads,ports']
            result = subprocess.run(cmd, capture_output=True, text=True, check=False)  # Safe: no shell=True

            for line in result.stdout.split('\n'):
                if 'Claude' in line and 'Helper' not in line:
                    parts = line.split()
                    if len(parts) >= 4 and stats.get('claude'):
                        cpu_str = parts[2].replace('%', '')
                        try:
                            stats['claude']['cpu_percent'] = float(cpu_str)
                        except (ValueError, TypeError):
                            pass  # Skip if CPU value can't be parsed
                        if len(parts) >= 5:
                            stats['claude']['threads'] = parts[4]
                        if len(parts) >= 6:
                            stats['claude']['ports'] = parts[5]

                elif 'com.docker.virtualization' in line:
                    parts = line.split()
                    if len(parts) >= 4 and stats.get('docker_vm'):
                        cpu_str = parts[2].replace('%', '')
                        try:
                            stats['docker_vm']['cpu_percent'] = float(cpu_str)
                        except (ValueError, TypeError):
                            pass  # Skip if CPU value can't be parsed
                        if len(parts) >= 5:
                            stats['docker_vm']['threads'] = parts[4]
                        if len(parts) >= 6:
                            stats['docker_vm']['ports'] = parts[5]

        except subprocess.SubprocessError:
            pass  # Top command failed, skip enrichment
        except (ValueError, IndexError):
            pass  # Parsing error, skip enrichment

    def log_stats(self, stats):
        """Log stats to both JSONL and CSV files."""
        # Write JSONL
        with open(self.log_file, 'a') as f:
            f.write(json.dumps(stats) + '\n')

        # Write CSV
        with open(self.csv_file, 'a') as f:
            timestamp = stats['timestamp']

            if stats.get('claude'):
                c = stats['claude']
                f.write(f"{timestamp},Claude,{c.get('cpu_percent', 0)},{c.get('memory_mb', 0)},"
                       f"{c.get('threads', 0)},{c.get('ports', 0)},0\n")

            if stats.get('docker_vm'):
                d = stats['docker_vm']
                f.write(f"{timestamp},DockerVM,{d.get('cpu_percent', 0)},{d.get('memory_mb', 0)},"
                       f"{d.get('threads', 0)},{d.get('ports', 0)},0\n")

    def print_stats(self, stats):
        """Print current stats to console."""
        if 'error' in stats:
            return

        # Clear line and print compact stats
        timestamp = datetime.now().strftime('%H:%M:%S')

        output = f"[{timestamp}] "

        if stats.get('claude'):
            c = stats['claude']
            output += f"Claude: {c.get('cpu_percent', 0):.1f}% CPU, {c.get('memory_mb', 0):.0f}MB "

        if stats.get('docker_vm'):
            d = stats['docker_vm']
            output += f"| Docker: {d.get('cpu_percent', 0):.1f}% CPU, {d.get('memory_mb', 0):.0f}MB"

        print(f"\r{output}", end='', flush=True)

    def monitor_loop(self):
        """Main monitoring loop."""
        while self.running:
            stats = self.get_process_stats()
            self.log_stats(stats)
            self.print_stats(stats)
            time.sleep(2)  # Sample every 2 seconds

    def signal_handler(self, signum, frame):
        """Handle Ctrl+C gracefully."""
        print(f"\n\n📊 Monitoring stopped after {datetime.now() - self.start_time}")
        print(f"📁 Logs saved to: {self.log_file}")
        print(f"📈 CSV saved to: {self.csv_file}")
        self.running = False
        sys.exit(0)

    def run(self):
        """Start monitoring."""
        signal.signal(signal.SIGINT, self.signal_handler)
        self.monitor_loop()

def analyze_logs(log_file):
    """Analyze collected performance logs."""
    print("\n📊 Performance Analysis")
    print("=" * 50)

    claude_cpu = []
    claude_mem = []
    docker_cpu = []
    docker_mem = []

    with open(log_file, 'r') as f:
        for line in f:
            try:
                data = json.loads(line)
                if data.get('claude'):
                    claude_cpu.append(data['claude'].get('cpu_percent', 0))
                    claude_mem.append(data['claude'].get('memory_mb', 0))
                if data.get('docker_vm'):
                    docker_cpu.append(data['docker_vm'].get('cpu_percent', 0))
                    docker_mem.append(data['docker_vm'].get('memory_mb', 0))
            except:
                pass

    if claude_cpu:
        print(f"\n🎯 Claude:")
        print(f"  CPU  - Avg: {sum(claude_cpu)/len(claude_cpu):.1f}%, Max: {max(claude_cpu):.1f}%")
        print(f"  Mem  - Avg: {sum(claude_mem)/len(claude_mem):.0f}MB, Max: {max(claude_mem):.0f}MB")

        # Detect spikes
        cpu_spikes = [c for c in claude_cpu if c > 80]
        if cpu_spikes:
            print(f"  ⚠️  CPU Spikes (>80%): {len(cpu_spikes)} times")

    if docker_cpu:
        print(f"\n🐳 Docker VM:")
        print(f"  CPU  - Avg: {sum(docker_cpu)/len(docker_cpu):.1f}%, Max: {max(docker_cpu):.1f}%")
        print(f"  Mem  - Avg: {sum(docker_mem)/len(docker_mem):.0f}MB, Max: {max(docker_mem):.0f}MB")

        # Detect spikes
        cpu_spikes = [c for c in docker_cpu if c > 50]
        if cpu_spikes:
            print(f"  ⚠️  CPU Spikes (>50%): {len(cpu_spikes)} times")

if __name__ == "__main__":
    import sys

    if len(sys.argv) > 1 and sys.argv[1] == "--analyze":
        # Analyze mode
        if len(sys.argv) > 2:
            analyze_logs(sys.argv[2])
        else:
            # Find most recent log
            log_dir = Path("/tmp/claude-performance-logs")
            if log_dir.exists():
                logs = sorted(log_dir.glob("perf_*.jsonl"))
                if logs:
                    analyze_logs(logs[-1])
                    print(f"\n📁 Analyzed: {logs[-1]}")
    else:
        # Monitor mode
        monitor = PerformanceMonitor()
        monitor.run()