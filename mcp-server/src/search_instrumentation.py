#!/usr/bin/env python3
"""
Search Instrumentation for Claude Self-Reflect
Tracks actual search patterns to understand user needs
"""

import json
import time
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional, Any
import re
import logging

logger = logging.getLogger(__name__)

class SearchInstrumentation:
    """Track and analyze search patterns to understand actual user needs"""
    
    def __init__(self, log_dir: str = "~/.claude-self-reflect/search-logs"):
        self.log_dir = Path(log_dir).expanduser()
        self.log_dir.mkdir(parents=True, exist_ok=True)
        self.current_session_file = self.log_dir / f"search_{datetime.now().strftime('%Y%m%d_%H%M%S')}.jsonl"
        
    def classify_query(self, query: str) -> Dict[str, bool]:
        """Classify query type to understand search patterns"""
        query_lower = query.lower()
        
        return {
            "is_error_search": bool(re.search(r'\b(error|exception|failed|cannot|issue|bug|broken)\b', query_lower)),
            "is_temporal_search": bool(re.search(r'\b(yesterday|today|last week|recent|ago|earlier)\b', query_lower)),
            "is_tool_search": bool(re.search(r'\b(docker|git|npm|python|react|vue|postgres|redis)\b', query_lower)),
            "is_solution_search": bool(re.search(r'\b(fix|solve|work|solution|resolved|handled)\b', query_lower)),
            "is_code_search": bool(re.search(r'\b(function|class|import|async|await|def |return)\b', query_lower)),
            "is_file_search": bool(re.search(r'\.(py|js|ts|jsx|tsx|md|json|yaml)\b', query_lower)),
            "has_pattern_keywords": bool(re.search(r'\b(pattern|hook|async|promise|component)\b', query_lower)),
        }
    
    def log_search_event(self, 
                         query: str,
                         results_count: int,
                         results_clicked: List[str] = None,
                         time_to_click: Optional[float] = None,
                         used_ast_metadata: bool = False) -> None:
        """Log a search event for analysis"""
        
        event = {
            "timestamp": datetime.now().isoformat(),
            "query": query,
            "query_length": len(query),
            "query_types": self.classify_query(query),
            "results_count": results_count,
            "results_clicked": results_clicked or [],
            "time_to_click": time_to_click,
            "used_ast_metadata": used_ast_metadata,
            "click_through_rate": len(results_clicked or []) / max(results_count, 1)
        }
        
        # Append to JSONL log file
        with open(self.current_session_file, 'a') as f:
            f.write(json.dumps(event) + '\n')
            
        logger.info(f"Logged search: {query[:50]}... Types: {[k for k, v in event['query_types'].items() if v]}")
    
    def analyze_recent_searches(self, hours: int = 24) -> Dict[str, Any]:
        """Analyze recent search patterns to understand user behavior"""
        
        cutoff_time = time.time() - (hours * 3600)
        all_events = []
        
        # Read all recent log files
        for log_file in self.log_dir.glob("search_*.jsonl"):
            if log_file.stat().st_mtime > cutoff_time:
                with open(log_file) as f:
                    for line in f:
                        try:
                            all_events.append(json.loads(line))
                        except json.JSONDecodeError:
                            continue
        
        if not all_events:
            return {"message": "No search events found"}
        
        # Analyze patterns
        total_searches = len(all_events)
        query_type_counts = {}
        ast_usage_count = sum(1 for e in all_events if e.get('used_ast_metadata'))
        
        # Count query types
        for event in all_events:
            for query_type, is_type in event.get('query_types', {}).items():
                if is_type:
                    query_type_counts[query_type] = query_type_counts.get(query_type, 0) + 1
        
        # Calculate click-through rates
        clicked_searches = [e for e in all_events if e.get('results_clicked')]
        avg_ctr = sum(e.get('click_through_rate', 0) for e in all_events) / max(total_searches, 1)
        
        # Find searches with pattern keywords that were clicked
        pattern_searches = [e for e in all_events if e.get('query_types', {}).get('has_pattern_keywords')]
        pattern_searches_clicked = [e for e in pattern_searches if e.get('results_clicked')]
        
        return {
            "total_searches": total_searches,
            "query_type_distribution": {
                k: f"{(v/total_searches)*100:.1f}%" 
                for k, v in sorted(query_type_counts.items(), key=lambda x: x[1], reverse=True)
            },
            "ast_metadata_usage": f"{(ast_usage_count/total_searches)*100:.1f}%",
            "average_click_through_rate": f"{avg_ctr*100:.1f}%",
            "searches_with_clicks": f"{(len(clicked_searches)/total_searches)*100:.1f}%",
            "pattern_keyword_searches": {
                "total": len(pattern_searches),
                "clicked": len(pattern_searches_clicked),
                "effectiveness": f"{(len(pattern_searches_clicked)/max(len(pattern_searches),1))*100:.1f}%"
            },
            "top_query_types": list(query_type_counts.keys())[:3],
            "recommendation": self._generate_recommendation(query_type_counts, ast_usage_count, total_searches)
        }
    
    def _generate_recommendation(self, query_types: Dict[str, int], ast_usage: int, total: int) -> str:
        """Generate recommendation based on search patterns"""
        
        if total == 0:
            return "Insufficient data"
        
        ast_percentage = (ast_usage / total) * 100
        error_percentage = (query_types.get('is_error_search', 0) / total) * 100
        solution_percentage = (query_types.get('is_solution_search', 0) / total) * 100
        
        if ast_percentage < 20:
            return "DELETE AST: Less than 20% usage - complexity without value"
        elif error_percentage > 40:
            return "FOCUS ON ERROR EXTRACTION: Users primarily search for errors"
        elif solution_percentage > 30:
            return "BUILD SOLUTION SCORING: Users want solutions that worked"
        else:
            return "SIMPLIFY TO BASICS: Focus on semantic search + error extraction"


# Integration helper for MCP server
def instrument_search(query: str, results: List[Any], clicked_ids: List[str] = None) -> None:
    """Helper function to instrument searches from MCP server"""
    
    instrumenter = SearchInstrumentation()
    
    # Check if any results used AST metadata
    used_ast = any(
        'code_patterns' in getattr(r, 'metadata', {}) 
        for r in results
    )
    
    instrumenter.log_search_event(
        query=query,
        results_count=len(results),
        results_clicked=clicked_ids,
        used_ast_metadata=used_ast
    )


if __name__ == "__main__":
    # Test the instrumentation
    instrumenter = SearchInstrumentation()
    
    # Simulate some searches
    test_queries = [
        "docker memory leak error",
        "how did we fix the authentication issue yesterday",
        "React hooks useState pattern",
        "find conversation about postgres optimization",
        "async await error handling pattern"
    ]
    
    for query in test_queries:
        instrumenter.log_search_event(
            query=query,
            results_count=5,
            results_clicked=["result_1", "result_2"] if "error" in query else ["result_1"],
            time_to_click=2.5,
            used_ast_metadata="pattern" in query.lower()
        )
    
    # Analyze the patterns
    analysis = instrumenter.analyze_recent_searches(hours=1)
    print(json.dumps(analysis, indent=2))