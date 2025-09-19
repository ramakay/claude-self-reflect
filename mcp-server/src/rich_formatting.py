"""Rich formatting for search results with emojis and enhanced display."""

import json
import time
from datetime import datetime, timezone
from typing import List, Dict, Any, Optional
import logging
from .safe_getters import safe_get_list, safe_get_str

logger = logging.getLogger(__name__)


def format_search_results_rich(
    results: List[Dict],
    query: str,
    target_project: str,
    collections_searched: int,
    timing_info: Dict[str, float],
    start_time: float,
    brief: bool = False,
    include_raw: bool = False,
    indexing_status: Optional[Dict] = None
) -> str:
    """Format search results with rich formatting including emojis and performance metrics."""

    # Initialize upfront summary
    upfront_summary = ""

    # Show result summary with emojis
    if results:
        score_info = "high" if results[0]['score'] >= 0.85 else "good" if results[0]['score'] >= 0.75 else "partial"
        upfront_summary += f"🎯 RESULTS: {len(results)} matches ({score_info} relevance, top score: {results[0]['score']:.3f})\n"

        # Show performance metrics
        total_time = time.time() - start_time
        indexing_info = ""
        if indexing_status and indexing_status.get("percentage", 100) < 100.0:
            indexing_info = f" | 📊 {indexing_status['indexed_conversations']}/{indexing_status['total_conversations']} indexed"
        upfront_summary += f"⚡ PERFORMANCE: {int(total_time * 1000)}ms ({collections_searched} collections searched{indexing_info})\n"
    else:
        upfront_summary += f"❌ NO RESULTS: No conversations found matching '{query}'\n"

    # Start XML format with upfront summary
    result_text = upfront_summary + "\n<search>\n"

    # Add indexing status if not fully baselined
    if indexing_status and indexing_status.get("percentage", 100) < 95.0:
        result_text += f'  <info status="indexing" progress="{indexing_status["percentage"]:.1f}%" backlog="{indexing_status.get("backlog_count", 0)}">\n'
        result_text += f'    <message>📊 Indexing: {indexing_status["indexed_conversations"]}/{indexing_status["total_conversations"]} conversations ({indexing_status["percentage"]:.1f}% complete)</message>\n'
        result_text += f"  </info>\n"

    # Add high-level result summary
    if results:
        # Count time-based results
        now = datetime.now(timezone.utc)
        today_count = 0
        yesterday_count = 0
        week_count = 0

        for result in results:
            timestamp_str = result.get('timestamp', '')
            if timestamp_str:
                try:
                    # Clean timestamp
                    timestamp_clean = timestamp_str.replace('Z', '+00:00') if timestamp_str.endswith('Z') else timestamp_str
                    timestamp_dt = datetime.fromisoformat(timestamp_clean)
                    if timestamp_dt.tzinfo is None:
                        timestamp_dt = timestamp_dt.replace(tzinfo=timezone.utc)

                    days_ago = (now - timestamp_dt).days
                    if days_ago == 0:
                        today_count += 1
                    elif days_ago == 1:
                        yesterday_count += 1
                    if days_ago <= 7:
                        week_count += 1
                except:
                    pass

        # Compact summary with key info
        time_info = ""
        if today_count > 0:
            time_info = f"{today_count} today"
        elif yesterday_count > 0:
            time_info = f"{yesterday_count} yesterday"
        elif week_count > 0:
            time_info = f"{week_count} this week"
        else:
            time_info = "older results"

        score_info = "high" if results[0]['score'] >= 0.85 else "good" if results[0]['score'] >= 0.75 else "partial"

        result_text += f'  <summary count="{len(results)}" relevance="{score_info}" recency="{time_info}" top-score="{results[0]["score"]:.3f}">\n'

        # Short preview of top result
        top_excerpt = results[0].get('excerpt', results[0].get('content', ''))[:100].strip()
        if '...' not in top_excerpt:
            top_excerpt += "..."
        result_text += f'    <preview>{top_excerpt}</preview>\n'
        result_text += f"  </summary>\n"
    else:
        result_text += f"  <result-summary>\n"
        result_text += f"    <headline>No matches found</headline>\n"
        result_text += f"    <relevance>No conversations matched your query</relevance>\n"
        result_text += f"  </result-summary>\n"

    # Add aggregated insights section (NEW FEATURE)
    if results and len(results) > 1:
        result_text += "  <insights>\n"
        result_text += f"    <!-- Processing {len(results)} results for pattern analysis -->\n"

        # Aggregate file modification patterns
        file_frequency = {}
        tool_frequency = {}
        concept_frequency = {}

        for result in results:
            # Count file modifications - using safe_get_list for consistency
            files = safe_get_list(result, 'files_analyzed')
            for file in files:
                file_frequency[file] = file_frequency.get(file, 0) + 1

            # Count tool usage - using safe_get_list for consistency
            tools = safe_get_list(result, 'tools_used')
            for tool in tools:
                tool_frequency[tool] = tool_frequency.get(tool, 0) + 1

            # Count concepts - using safe_get_list for consistency
            concepts = safe_get_list(result, 'concepts')
            for concept in concepts:
                concept_frequency[concept] = concept_frequency.get(concept, 0) + 1

        # Show most frequently modified files
        if file_frequency:
            top_files = sorted(file_frequency.items(), key=lambda x: x[1], reverse=True)[:3]
            if top_files:
                result_text += '    <pattern type="files">\n'
                result_text += f'      <title>📁 Frequently Modified Files</title>\n'
                for file, count in top_files:
                    percentage = (count / len(results)) * 100
                    result_text += f'      <item count="{count}" pct="{percentage:.0f}%">{file}</item>\n'
                result_text += '    </pattern>\n'

        # Show common tools used
        if tool_frequency:
            top_tools = sorted(tool_frequency.items(), key=lambda x: x[1], reverse=True)[:3]
            if top_tools:
                result_text += '    <pattern type="tools">\n'
                result_text += f'      <title>🔧 Common Tools Used</title>\n'
                for tool, count in top_tools:
                    percentage = (count / len(results)) * 100
                    result_text += f'      <item count="{count}" pct="{percentage:.0f}%">{tool}</item>\n'
                result_text += '    </pattern>\n'

        # Show related concepts
        if concept_frequency:
            top_concepts = sorted(concept_frequency.items(), key=lambda x: x[1], reverse=True)[:3]
            if top_concepts:
                result_text += '    <pattern type="concepts">\n'
                result_text += f'      <title>💡 Related Concepts</title>\n'
                for concept, count in top_concepts:
                    percentage = (count / len(results)) * 100
                    result_text += f'      <item count="{count}" pct="{percentage:.0f}%">{concept}</item>\n'
                result_text += '    </pattern>\n'

        # Add workflow suggestion based on patterns
        if file_frequency and tool_frequency:
            most_common_file = list(file_frequency.keys())[0] if file_frequency else None
            most_common_tool = list(tool_frequency.keys())[0] if tool_frequency else None
            if most_common_file and most_common_tool:
                result_text += '    <suggestion>\n'
                result_text += f'      <title>💭 Pattern Detection</title>\n'
                result_text += f'      <text>Similar conversations often involve {most_common_tool} on {most_common_file}</text>\n'
                result_text += '    </suggestion>\n'

        # Always show a summary even if no clear patterns
        if not file_frequency and not tool_frequency and not concept_frequency:
            result_text += '    <summary>\n'
            result_text += f'      <title>📊 Analysis Summary</title>\n'
            result_text += f'      <text>Analyzed {len(results)} conversations for patterns</text>\n'

            # Show temporal distribution
            now = datetime.now(timezone.utc)
            time_dist = {"today": 0, "week": 0, "month": 0, "older": 0}
            for result in results:
                timestamp_str = result.get('timestamp', '')
                if timestamp_str:
                    try:
                        timestamp_clean = timestamp_str.replace('Z', '+00:00') if timestamp_str.endswith('Z') else timestamp_str
                        timestamp_dt = datetime.fromisoformat(timestamp_clean)
                        if timestamp_dt.tzinfo is None:
                            timestamp_dt = timestamp_dt.replace(tzinfo=timezone.utc)
                        days_ago = (now - timestamp_dt).days
                        if days_ago == 0:
                            time_dist["today"] += 1
                        elif days_ago <= 7:
                            time_dist["week"] += 1
                        elif days_ago <= 30:
                            time_dist["month"] += 1
                        else:
                            time_dist["older"] += 1
                    except:
                        pass

            if any(time_dist.values()):
                dist_str = ", ".join([f"{v} {k}" for k, v in time_dist.items() if v > 0])
                result_text += f'      <temporal>Time distribution: {dist_str}</temporal>\n'

            result_text += '    </summary>\n'

        result_text += "  </insights>\n\n"

    # Add metadata
    result_text += f"  <meta>\n"
    result_text += f"    <q>{query}</q>\n"
    result_text += f"    <scope>{target_project if target_project != 'all' else 'all'}</scope>\n"
    result_text += f"    <count>{len(results)}</count>\n"
    if results:
        result_text += f"    <range>{results[-1]['score']:.3f}-{results[0]['score']:.3f}</range>\n"

    # Add performance metadata
    total_time = time.time() - start_time
    result_text += f"    <perf>\n"
    result_text += f"      <ttl>{int(total_time * 1000)}</ttl>\n"
    result_text += f"      <emb>{int((timing_info.get('embedding_end', 0) - timing_info.get('embedding_start', 0)) * 1000)}</emb>\n"
    result_text += f"      <srch>{int((timing_info.get('search_all_end', 0) - timing_info.get('search_all_start', 0)) * 1000)}</srch>\n"
    result_text += f"      <cols>{collections_searched}</cols>\n"
    result_text += f"    </perf>\n"
    result_text += f"  </meta>\n"

    # Add individual results
    result_text += "  <results>\n"
    for i, result in enumerate(results):
        result_text += f'    <r rank="{i+1}">\n'
        result_text += f"      <s>{result['score']:.3f}</s>\n"
        result_text += f"      <p>{result.get('project_name', 'unknown')}</p>\n"

        # Calculate relative time
        timestamp_str = result.get('timestamp', '')
        if timestamp_str:
            try:
                timestamp_clean = timestamp_str.replace('Z', '+00:00') if timestamp_str.endswith('Z') else timestamp_str
                timestamp_dt = datetime.fromisoformat(timestamp_clean)
                if timestamp_dt.tzinfo is None:
                    timestamp_dt = timestamp_dt.replace(tzinfo=timezone.utc)
                now = datetime.now(timezone.utc)
                days_ago = (now - timestamp_dt).days
                if days_ago == 0:
                    time_str = "today"
                elif days_ago == 1:
                    time_str = "yesterday"
                else:
                    time_str = f"{days_ago}d"
                result_text += f"      <t>{time_str}</t>\n"
            except:
                result_text += f"      <t>unknown</t>\n"

        # Get excerpt/content
        excerpt = result.get('excerpt', result.get('content', ''))

        if not brief and excerpt:
            # Extract title from first line of excerpt
            excerpt_lines = excerpt.split('\n')
            title = excerpt_lines[0][:80] + "..." if len(excerpt_lines[0]) > 80 else excerpt_lines[0]
            result_text += f"      <title>{title}</title>\n"

            # Key finding - summarize the main point
            key_finding = excerpt[:100] + "..." if len(excerpt) > 100 else excerpt
            result_text += f"      <key-finding>{key_finding.strip()}</key-finding>\n"

        # Always include excerpt
        if brief:
            brief_excerpt = excerpt[:100] + "..." if len(excerpt) > 100 else excerpt
            result_text += f"      <excerpt>{brief_excerpt.strip()}</excerpt>\n"
        else:
            result_text += f"      <excerpt><![CDATA[{excerpt}]]></excerpt>\n"

        # Add conversation ID if present
        if result.get('conversation_id'):
            result_text += f"      <cid>{result['conversation_id']}</cid>\n"

        # Include raw data if requested
        if include_raw and result.get('raw_payload'):
            result_text += "      <raw>\n"
            payload = result['raw_payload']
            result_text += f"        <txt><![CDATA[{payload.get('text', '')}]]></txt>\n"
            result_text += f"        <id>{result.get('id', '')}</id>\n"
            result_text += "      </raw>\n"

        # Add metadata fields if present
        if result.get('files_analyzed'):
            result_text += f"      <files>{', '.join(result['files_analyzed'][:5])}</files>\n"
        if result.get('tools_used'):
            result_text += f"      <tools>{', '.join(result['tools_used'][:5])}</tools>\n"
        if result.get('concepts'):
            result_text += f"      <concepts>{', '.join(result['concepts'][:5])}</concepts>\n"

        result_text += "    </r>\n"

    result_text += "  </results>\n"
    result_text += "</search>\n"

    return result_text