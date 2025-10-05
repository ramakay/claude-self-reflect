#!/usr/bin/env python3
"""
Migration statistics tracker for v2 chunking migration.
Provides real-time metrics on migration progress.
"""

import asyncio
import json
import os
from pathlib import Path
from typing import Dict, List, Optional
from datetime import datetime
from collections import defaultdict
import logging

from qdrant_client import AsyncQdrantClient
from rich.console import Console
from rich.table import Table
from rich.progress import Progress, BarColumn, TextColumn, TimeRemainingColumn
from rich.live import Live
from rich.layout import Layout
from rich.panel import Panel

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

console = Console()


class MigrationStats:
    """Track and display migration statistics."""
    
    def __init__(self, qdrant_url: str = None):
        self.qdrant_url = qdrant_url or os.getenv("QDRANT_URL", "http://localhost:6333")
        self.client = AsyncQdrantClient(url=self.qdrant_url)
        self.stats = defaultdict(lambda: {
            "total_chunks": 0,
            "v1_chunks": 0,
            "v2_chunks": 0,
            "truncated_chunks": 0,
            "empty_chunks": 0,
            "avg_text_length": 0,
            "collection_size_mb": 0
        })
    
    async def get_collection_stats(self, collection_name: str) -> Dict:
        """Get statistics for a single collection."""
        stats = {
            "total_chunks": 0,
            "v1_chunks": 0,
            "v2_chunks": 0,
            "truncated_chunks": 0,
            "empty_chunks": 0,
            "text_lengths": [],
            "timestamps": []
        }
        
        # Scroll through all points
        offset = None
        batch_size = 100
        
        while True:
            try:
                response = await self.client.scroll(
                    collection_name=collection_name,
                    limit=batch_size,
                    offset=offset,
                    with_payload=True,
                    with_vectors=False
                )
                
                points, next_offset = response
                
                if not points:
                    break
                
                for point in points:
                    stats["total_chunks"] += 1
                    payload = point.payload
                    
                    # Check version
                    version = payload.get('chunking_version', 'v1')
                    if version == 'v2':
                        stats["v2_chunks"] += 1
                    else:
                        stats["v1_chunks"] += 1
                    
                    # Check for truncation
                    text = payload.get('text', '')
                    text_length = len(text)
                    stats["text_lengths"].append(text_length)
                    
                    if text_length == 1500 or text.endswith('[...]'):
                        stats["truncated_chunks"] += 1
                    
                    if text_length == 0 or text == 'NO TEXT':
                        stats["empty_chunks"] += 1
                    
                    # Collect timestamps
                    if 'timestamp' in payload:
                        stats["timestamps"].append(payload['timestamp'])
                
                offset = next_offset
                if offset is None:
                    break
                    
            except Exception as e:
                logger.error(f"Error processing collection {collection_name}: {e}")
                break
        
        # Calculate averages
        if stats["text_lengths"]:
            stats["avg_text_length"] = sum(stats["text_lengths"]) / len(stats["text_lengths"])
        else:
            stats["avg_text_length"] = 0
        
        # Get collection info
        try:
            collection_info = await self.client.get_collection(collection_name)
            stats["vectors_count"] = collection_info.vectors_count or stats["total_chunks"]
            stats["points_count"] = collection_info.points_count or stats["total_chunks"]
        except:
            stats["vectors_count"] = stats["total_chunks"]
            stats["points_count"] = stats["total_chunks"]
        
        return stats
    
    async def get_all_stats(self) -> Dict:
        """Get statistics for all collections."""
        all_stats = {}
        
        # Get all collections
        response = await self.client.get_collections()
        collections = [c.name for c in response.collections if c.name.startswith("conv_")]
        
        # Process each collection
        for collection_name in collections:
            stats = await self.get_collection_stats(collection_name)
            
            # Extract project name
            project = collection_name.replace('conv_', '').replace('_local', '').replace('_voyage', '')
            
            all_stats[project] = {
                "collection": collection_name,
                **stats
            }
        
        return all_stats
    
    def calculate_global_stats(self, all_stats: Dict) -> Dict:
        """Calculate global statistics across all collections."""
        global_stats = {
            "total_collections": len(all_stats),
            "total_chunks": 0,
            "total_v1": 0,
            "total_v2": 0,
            "total_truncated": 0,
            "total_empty": 0,
            "migration_percentage": 0,
            "health_score": 0
        }
        
        for project, stats in all_stats.items():
            global_stats["total_chunks"] += stats["total_chunks"]
            global_stats["total_v1"] += stats["v1_chunks"]
            global_stats["total_v2"] += stats["v2_chunks"]
            global_stats["total_truncated"] += stats["truncated_chunks"]
            global_stats["total_empty"] += stats["empty_chunks"]
        
        # Calculate percentages
        if global_stats["total_chunks"] > 0:
            global_stats["migration_percentage"] = (global_stats["total_v2"] / global_stats["total_chunks"]) * 100
            
            # Health score: percentage of non-truncated, non-empty chunks
            healthy_chunks = global_stats["total_chunks"] - global_stats["total_truncated"] - global_stats["total_empty"]
            global_stats["health_score"] = (healthy_chunks / global_stats["total_chunks"]) * 100
        
        return global_stats
    
    def create_stats_table(self, all_stats: Dict) -> Table:
        """Create a rich table with statistics."""
        table = Table(title="Migration Statistics by Project", show_header=True)
        
        table.add_column("Project", style="cyan", no_wrap=True)
        table.add_column("Total", justify="right")
        table.add_column("v1", justify="right", style="yellow")
        table.add_column("v2", justify="right", style="green")
        table.add_column("Progress", justify="center")
        table.add_column("Truncated", justify="right", style="red")
        table.add_column("Empty", justify="right", style="red")
        table.add_column("Avg Length", justify="right")
        
        for project, stats in sorted(all_stats.items()):
            if stats["total_chunks"] > 0:
                progress_pct = (stats["v2_chunks"] / stats["total_chunks"]) * 100
                progress_bar = self.create_progress_bar(progress_pct)
            else:
                progress_pct = 0
                progress_bar = "N/A"
            
            table.add_row(
                project[:20],  # Truncate long project names
                str(stats["total_chunks"]),
                str(stats["v1_chunks"]),
                str(stats["v2_chunks"]),
                progress_bar,
                str(stats["truncated_chunks"]),
                str(stats["empty_chunks"]),
                f"{stats['avg_text_length']:.0f}"
            )
        
        return table
    
    def create_progress_bar(self, percentage: float, width: int = 20) -> str:
        """Create a text-based progress bar."""
        filled = int(width * percentage / 100)
        bar = "█" * filled + "░" * (width - filled)
        return f"{bar} {percentage:.1f}%"
    
    def create_summary_panel(self, global_stats: Dict) -> Panel:
        """Create a summary panel with global statistics."""
        content = f"""
[bold cyan]Global Migration Status[/bold cyan]

Total Collections: {global_stats['total_collections']}
Total Chunks: {global_stats['total_chunks']:,}

[bold green]Migration Progress:[/bold green]
├─ v2 Chunks: {global_stats['total_v2']:,} ({global_stats['total_v2']/global_stats['total_chunks']*100:.1f}%)
├─ v1 Chunks: {global_stats['total_v1']:,} ({global_stats['total_v1']/global_stats['total_chunks']*100:.1f}%)
└─ Progress: {self.create_progress_bar(global_stats['migration_percentage'])}

[bold yellow]Data Quality:[/bold yellow]
├─ Truncated: {global_stats['total_truncated']:,} ({global_stats['total_truncated']/global_stats['total_chunks']*100:.1f}%)
├─ Empty: {global_stats['total_empty']:,} ({global_stats['total_empty']/global_stats['total_chunks']*100:.1f}%)
└─ Health Score: {global_stats['health_score']:.1f}%

[dim]Last Updated: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}[/dim]
"""
        
        return Panel(content, title="Migration Summary", border_style="green")
    
    async def display_live_stats(self, refresh_interval: int = 30):
        """Display live statistics with auto-refresh."""
        console.print("[bold green]Starting live migration statistics...[/bold green]")
        console.print(f"Refreshing every {refresh_interval} seconds. Press Ctrl+C to exit.\n")
        
        try:
            while True:
                # Clear console
                console.clear()
                
                # Get current stats
                all_stats = await self.get_all_stats()
                global_stats = self.calculate_global_stats(all_stats)
                
                # Display summary
                console.print(self.create_summary_panel(global_stats))
                console.print()
                
                # Display table
                console.print(self.create_stats_table(all_stats))
                
                # Wait for refresh
                await asyncio.sleep(refresh_interval)
                
        except KeyboardInterrupt:
            console.print("\n[yellow]Stopped monitoring.[/yellow]")
    
    async def export_stats(self, output_file: Path):
        """Export statistics to JSON file."""
        all_stats = await self.get_all_stats()
        global_stats = self.calculate_global_stats(all_stats)
        
        export_data = {
            "timestamp": datetime.now().isoformat(),
            "global": global_stats,
            "collections": all_stats
        }
        
        with open(output_file, 'w') as f:
            json.dump(export_data, f, indent=2)
        
        console.print(f"[green]Stats exported to {output_file}[/green]")
    
    async def check_migration_readiness(self):
        """Check if system is ready for full v2 migration."""
        all_stats = await self.get_all_stats()
        global_stats = self.calculate_global_stats(all_stats)
        
        console.print("\n[bold cyan]Migration Readiness Check[/bold cyan]\n")
        
        # Check various criteria
        checks = []
        
        # Check 1: Migration percentage
        if global_stats['migration_percentage'] >= 80:
            checks.append(("✅", "Migration >80% complete", "green"))
        else:
            checks.append(("❌", f"Migration only {global_stats['migration_percentage']:.1f}% complete", "red"))
        
        # Check 2: Truncated chunks
        truncated_pct = (global_stats['total_truncated'] / global_stats['total_chunks']) * 100 if global_stats['total_chunks'] > 0 else 0
        if truncated_pct < 5:
            checks.append(("✅", f"Truncated chunks <5% ({truncated_pct:.1f}%)", "green"))
        else:
            checks.append(("⚠️", f"Truncated chunks at {truncated_pct:.1f}%", "yellow"))
        
        # Check 3: Empty chunks
        empty_pct = (global_stats['total_empty'] / global_stats['total_chunks']) * 100 if global_stats['total_chunks'] > 0 else 0
        if empty_pct < 1:
            checks.append(("✅", f"Empty chunks <1% ({empty_pct:.1f}%)", "green"))
        else:
            checks.append(("⚠️", f"Empty chunks at {empty_pct:.1f}%", "yellow"))
        
        # Check 4: Health score
        if global_stats['health_score'] >= 90:
            checks.append(("✅", f"Health score >90% ({global_stats['health_score']:.1f}%)", "green"))
        else:
            checks.append(("⚠️", f"Health score at {global_stats['health_score']:.1f}%", "yellow"))
        
        # Display checks
        for icon, message, color in checks:
            console.print(f"{icon} {message}", style=color)
        
        # Overall recommendation
        console.print("\n[bold]Recommendation:[/bold]")
        if all(check[0] == "✅" for check in checks):
            console.print("✅ System is ready for full v2 migration!", style="bold green")
        else:
            console.print("⚠️ Continue background migration before switching fully to v2", style="bold yellow")


async def main():
    """Main entry point."""
    import argparse
    parser = argparse.ArgumentParser(description="Monitor v2 chunking migration progress")
    parser.add_argument("--live", action="store_true", help="Show live statistics")
    parser.add_argument("--export", type=str, help="Export stats to JSON file")
    parser.add_argument("--check", action="store_true", help="Check migration readiness")
    parser.add_argument("--interval", type=int, default=30, help="Refresh interval for live mode (seconds)")
    args = parser.parse_args()
    
    stats = MigrationStats()
    
    if args.live:
        await stats.display_live_stats(args.interval)
    elif args.export:
        await stats.export_stats(Path(args.export))
    elif args.check:
        await stats.check_migration_readiness()
    else:
        # Default: show once
        all_stats = await stats.get_all_stats()
        global_stats = stats.calculate_global_stats(all_stats)
        
        console.print(stats.create_summary_panel(global_stats))
        console.print()
        console.print(stats.create_stats_table(all_stats))


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        console.print("\n[yellow]Interrupted by user.[/yellow]")