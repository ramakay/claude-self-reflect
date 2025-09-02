"""
Project name resolution for claude-self-reflect.
Handles mapping between user-friendly names and internal collection names.
"""

import hashlib
import logging
import re
from pathlib import Path
from typing import List, Dict, Optional, Set
from time import time
from qdrant_client import QdrantClient

logger = logging.getLogger(__name__)

# Project discovery markers - common parent directories that indicate project roots
PROJECT_MARKERS = {'projects', 'code', 'Code', 'repos', 'repositories', 
                   'dev', 'Development', 'work', 'src', 'github', 'gitlab'}

# Patterns to filter out from project segments - keep minimal
# Users create Claude conversations from their actual working directories
FILTER_PATTERNS = {
    r'^[a-f0-9]{32}$',  # Full MD5 hashes
    r'^[a-f0-9]{40}$',  # Full SHA1 hashes
    r'^\.$',  # Single dot
    r'^\.\.$',  # Double dot
}


class ProjectResolver:
    """Resolves user-friendly project names to collection names."""
    
    def __init__(self, qdrant_client: QdrantClient):
        self.client = qdrant_client
        self._cache: Dict[str, Set[str]] = {}
        self._cache_ttl: Dict[str, float] = {}
        self._cache_duration = 300  # 5 minutes TTL
        self._reverse_cache: Dict[str, str] = {}
        # Collection names cache
        self._collections_cache: List[str] = []
        self._collections_cache_time: float = 0
        # Compile filter patterns for efficiency
        import re
        self._filter_patterns = [re.compile(p) for p in FILTER_PATTERNS]
        
    def find_collections_for_project(self, user_project_name: str) -> List[str]:
        """
        Find all collections that match a user-provided project name.
        
        Tries multiple strategies:
        1. Direct hash of the input
        2. Normalized name hash
        3. Scan collections for matching project metadata
        4. Fuzzy matching on collection names
        
        Args:
            user_project_name: User-provided project name (e.g., "example-project", "Example-Project", full path)
            
        Returns:
            List of collection names that match the project
        """
        if user_project_name in self._cache:
            # Check if cache entry is still valid
            if time() - self._cache_ttl.get(user_project_name, 0) < self._cache_duration:
                return list(self._cache[user_project_name])
            else:
                # Cache expired, remove it
                del self._cache[user_project_name]
                del self._cache_ttl[user_project_name]
            
        matching_collections = set()
        
        # Get all collections (with caching)
        collection_names = self._get_collection_names()
        if not collection_names:
            return []
        
        # Strategy 1: Direct hash of input (handles full paths)
        # Try both MD5 (used by streaming-watcher) and SHA256 (legacy)
        direct_hash_md5 = hashlib.md5(user_project_name.encode()).hexdigest()[:8]
        direct_hash_sha256 = hashlib.sha256(user_project_name.encode()).hexdigest()[:16]
        
        # Match exact hash segment between underscores, not substring
        direct_matches = [c for c in collection_names 
                         if f"_{direct_hash_md5}_" in c or c.endswith(f"_{direct_hash_md5}") or
                            f"_{direct_hash_sha256}_" in c or c.endswith(f"_{direct_hash_sha256}")]
        matching_collections.update(direct_matches)
        
        # Strategy 2: Try normalized version
        normalized = self._normalize_project_name(user_project_name)
        if normalized != user_project_name:
            norm_hash_md5 = hashlib.md5(normalized.encode()).hexdigest()[:8]
            norm_hash_sha256 = hashlib.sha256(normalized.encode()).hexdigest()[:16]
            
            # Match exact hash segment between underscores, not substring
            norm_matches = [c for c in collection_names 
                           if f"_{norm_hash_md5}_" in c or c.endswith(f"_{norm_hash_md5}") or
                              f"_{norm_hash_sha256}_" in c or c.endswith(f"_{norm_hash_sha256}")]
            matching_collections.update(norm_matches)
        
        # Strategy 3: Case-insensitive normalized version
        lower_normalized = normalized.lower()
        if lower_normalized != normalized:
            lower_hash_md5 = hashlib.md5(lower_normalized.encode()).hexdigest()[:8]
            lower_hash_sha256 = hashlib.sha256(lower_normalized.encode()).hexdigest()[:16]
            
            # Match exact hash segment between underscores, not substring
            lower_matches = [c for c in collection_names 
                            if f"_{lower_hash_md5}_" in c or c.endswith(f"_{lower_hash_md5}") or
                               f"_{lower_hash_sha256}_" in c or c.endswith(f"_{lower_hash_sha256}")]
            matching_collections.update(lower_matches)
        
        # Strategy 4: ALWAYS try mapping project name to full directory path in .claude/projects/
        # This ensures we find all related collections, not just the first match
        # This handles the case where streaming-watcher uses full path but MCP uses short name
        if not user_project_name.startswith('-'):
            # Check if there's a matching directory in .claude/projects/
            projects_dir = Path.home() / ".claude" / "projects"
            if projects_dir.exists():
                for proj_dir in projects_dir.iterdir():
                    if proj_dir.is_dir():
                        # Check if the directory name contains the project name
                        # This handles both "claude-self-reflect" and "-Users-...-projects-claude-self-reflect"
                        if (proj_dir.name.endswith(f"-{user_project_name}") or 
                            f"-{user_project_name}" in proj_dir.name or
                            proj_dir.name == user_project_name):
                            # Found a matching directory - hash its name
                            dir_name = proj_dir.name
                            dir_hash_md5 = hashlib.md5(dir_name.encode()).hexdigest()[:8]
                            
                            # Find collections with this hash
                            dir_matches = [c for c in collection_names 
                                          if f"_{dir_hash_md5}_" in c or c.endswith(f"_{dir_hash_md5}")]
                            matching_collections.update(dir_matches)
        
        # Strategy 5: Use segment-based discovery for complex paths
        if not matching_collections:
            # Extract segments from the input
            segments = self._extract_project_segments(user_project_name)
            if segments:
                # Score and generate candidates
                scores = self._score_segments(segments, user_project_name)
                candidates = self._generate_search_candidates(segments, scores)
                
                # Try each candidate
                for candidate in candidates:
                    candidate_hash_md5 = hashlib.md5(candidate.encode()).hexdigest()[:8]
                    candidate_hash_sha256 = hashlib.sha256(candidate.encode()).hexdigest()[:16]
                    
                    # Match exact hash segment between underscores, not substring
                    candidate_matches = [c for c in collection_names 
                                       if f"_{candidate_hash_md5}_" in c or c.endswith(f"_{candidate_hash_md5}") or
                                          f"_{candidate_hash_sha256}_" in c or c.endswith(f"_{candidate_hash_sha256}")]
                    matching_collections.update(candidate_matches)
                    
                    # Stop if we found matches
                    if matching_collections:
                        break
        
        # Strategy 5: Scan ALL collections to build a mapping
        # This finds collections where the stored project name contains our search term
        if not matching_collections:
            # Get all projects first
            all_projects = self.get_all_projects()
            
            # Find matching project names
            search_lower = user_project_name.lower()
            for project_name, project_collections in all_projects.items():
                if (search_lower in project_name.lower() or 
                    project_name.lower() in search_lower or
                    project_name.lower() == search_lower):
                    matching_collections.update(project_collections)
                    
        # Strategy 5: Direct collection scan as last resort
        if not matching_collections and len(collection_names) < 200:  # Only for reasonable collection counts
            # Sample proportionally - 5% of collections up to 10
            sample_size = min(10, max(1, len(collection_names) // 20))
            for coll_name in collection_names[:sample_size]:
                try:
                    # Get a sample point to check metadata structure
                    result = self.client.scroll(
                        collection_name=coll_name,
                        limit=1,
                        with_payload=True
                    )
                    if not result or not result[0]:
                        continue
                    if result[0]:
                        point = result[0][0]
                        project_in_payload = point.payload.get('project', '')
                        
                        # Check if this project matches
                        if self._project_matches(project_in_payload, user_project_name):
                            # Get the hash from this collection name
                            # Format: conv_HASH_local or conv_HASH_voyage
                            parts = coll_name.split('_')
                            if len(parts) >= 2:
                                coll_hash = parts[1]
                                # Find all collections with this hash
                                hash_matches = [c for c in collection_names if coll_hash in c]
                                matching_collections.update(hash_matches)
                                break
                except Exception as e:
                    logger.debug(f"Failed to scroll {coll_name}: {e}")
                    continue
        
        # Cache the result with TTL
        result = list(matching_collections)
        self._cache[user_project_name] = matching_collections
        self._cache_ttl[user_project_name] = time()
        
        return result
    
    def _get_collection_names(self, force_refresh: bool = False) -> List[str]:
        """
        Get all collection names with caching.
        
        Args:
            force_refresh: Force refresh the cache
            
        Returns:
            List of collection names starting with 'conv_'
        """
        # Check cache validity
        if not force_refresh and self._collections_cache:
            if time() - self._collections_cache_time < self._cache_duration:
                return self._collections_cache
        
        # Fetch fresh collection list
        try:
            all_collections = self.client.get_collections().collections
            collection_names = [c.name for c in all_collections if c.name.startswith('conv_')]
            
            # Update cache
            self._collections_cache = collection_names
            self._collections_cache_time = time()
            
            return collection_names
        except Exception as e:
            logger.error(f"Failed to get collections: {e}")
            # Return cached version if available, even if expired
            return self._collections_cache if self._collections_cache else []
    
    def _normalize_project_name(self, project_path: str) -> str:
        """
        Normalize project name for consistent hashing.
        Extracts the actual project name from various path formats.
        """
        if not project_path:
            return ""
        
        # Remove trailing slashes
        project_path = project_path.rstrip('/')
        
        # Handle Claude logs format (starts with dash)
        if project_path.startswith('-'):
            # Split on dashes but don't convert to path separators
            # This preserves project names that contain dashes
            path_str = project_path[1:]  # Remove leading dash
            path_parts = path_str.split('-')  # Split on dashes, not path separators
            
            # Look for common project parent directories
            project_parents = {'projects', 'code', 'Code', 'repos', 'repositories', 
                              'dev', 'Development', 'work', 'src', 'github'}
            
            # Find the project name after a known parent directory
            for i, part in enumerate(path_parts):
                if part.lower() in project_parents and i + 1 < len(path_parts):
                    # Return everything after the parent directory
                    remaining = path_parts[i + 1:]
                    
                    # Use segment-based approach for complex paths
                    # Return the most likely project name from remaining segments
                    if remaining:
                        # If it's a single segment, return it
                        if len(remaining) == 1:
                            return remaining[0]
                        # For multiple segments, look for project-like patterns
                        for r in remaining:
                            r_lower = r.lower()
                            # Prioritize segments with project indicators
                            if any(ind in r_lower for ind in ['app', 'service', 'project', 'api', 'client']):
                                return r
                    
                    # Otherwise join remaining parts
                    return '-'.join(remaining)
            
            # Fallback: use the last component
            return path_parts[-1] if path_parts else project_path
        
        # For regular paths or simple names
        path_obj = Path(project_path)
        
        # If it's already a simple name, return it
        if '/' not in project_path and '\\' not in project_path:
            return project_path
            
        # Otherwise extract from path
        return path_obj.name
    
    def _project_matches(self, stored_project: str, target_project: str) -> bool:
        """
        Check if a stored project name matches the target.
        Handles various naming conventions.
        """
        # Exact match
        if stored_project == target_project:
            return True
            
        # Case-insensitive match
        if stored_project.lower() == target_project.lower():
            return True
            
        # Check if target appears at the end of stored (for paths)
        if stored_project.endswith(f"-{target_project}") or stored_project.endswith(f"/{target_project}"):
            return True
            
        # Check if normalized versions match
        stored_norm = self._normalize_project_name(stored_project)
        target_norm = self._normalize_project_name(target_project)
        
        if stored_norm.lower() == target_norm.lower():
            return True
            
        # Check if segments match
        stored_segments = self._extract_project_segments(stored_project)
        target_segments = self._extract_project_segments(target_project)
        
        # If any segments match, consider it a match
        if stored_segments and target_segments:
            stored_set = set(s.lower() for s in stored_segments)
            target_set = set(s.lower() for s in target_segments)
            if stored_set & target_set:  # Intersection
                return True
            
        return False
    
    def get_all_projects(self) -> Dict[str, List[str]]:
        """
        Get all available projects and their collections.
        Returns a mapping of project names to collection names.
        """
        projects = {}
        
        try:
            # Use cached collection names
            collection_names = self._get_collection_names()
            
            # Group collections by hash
            hash_groups = {}
            for coll_name in collection_names:
                parts = coll_name.split('_')
                if len(parts) >= 2:
                    coll_hash = parts[1]
                    if coll_hash not in hash_groups:
                        hash_groups[coll_hash] = []
                    hash_groups[coll_hash].append(coll_name)
            
            # Sample each group to find project name
            for coll_hash, colls in hash_groups.items():
                # Skip empty collections
                sample_coll = colls[0]
                try:
                    info = self.client.get_collection(sample_coll)
                    if info.points_count == 0:
                        continue
                        
                    # Get a sample point
                    result = self.client.scroll(
                        collection_name=sample_coll,
                        limit=1,
                        with_payload=True
                    )
                    
                    if result[0]:
                        point = result[0][0]
                        project_name = point.payload.get('project', f'unknown_{coll_hash}')
                        
                        # Try to extract a friendly name
                        friendly_name = self._normalize_project_name(project_name)
                        if friendly_name:
                            projects[friendly_name] = colls
                        else:
                            projects[project_name] = colls
                            
                except Exception as e:
                    logger.debug(f"Error sampling {sample_coll}: {e}")
                    continue
                    
        except Exception as e:
            logger.error(f"Failed to get all projects: {e}")
            
        return projects
    
    def _extract_project_segments(self, path: str) -> List[str]:
        """
        Extract meaningful segments from a dash-encoded or regular path.
        
        Examples:
        - -Users-name-projects-my-app-src -> ['my', 'app', 'src']
        - -Users-name-Code-example-project -> ['example', 'project']
        
        Args:
            path: Path in any format
            
        Returns:
            List of meaningful segments that could be project names
        """
        segments = []
        
        # Handle dash-encoded paths
        if path.startswith('-'):
            # Remove leading dash and split
            parts = path[1:].split('-')
            
            # Find marker position
            marker_idx = -1
            for i, part in enumerate(parts):
                if part.lower() in PROJECT_MARKERS:
                    marker_idx = i
                    break
            
            # Extract segments after marker
            if marker_idx >= 0:
                # Everything after the marker is a candidate
                candidate_parts = parts[marker_idx + 1:]
            else:
                # No marker found, use last few segments
                candidate_parts = parts[-3:] if len(parts) > 3 else parts
            
            # Filter out unwanted patterns
            for part in candidate_parts:
                if not self._should_filter_segment(part):
                    segments.append(part)
        
        # Handle regular paths
        else:
            path_obj = Path(path)
            parts = list(path_obj.parts)
            
            # Find marker position
            marker_idx = -1
            for i, part in enumerate(parts):
                if part.lower() in PROJECT_MARKERS:
                    marker_idx = i
                    break
            
            # Extract segments after marker
            if marker_idx >= 0:
                candidate_parts = parts[marker_idx + 1:]
            else:
                # Use the path name itself
                candidate_parts = [path_obj.name] if path_obj.name else []
            
            # Process segments
            for part in candidate_parts:
                # Split on common separators
                sub_parts = part.replace('-', ' ').replace('_', ' ').split()
                for sub in sub_parts:
                    if not self._should_filter_segment(sub):
                        segments.append(sub)
        
        return segments
    
    def _should_filter_segment(self, segment: str) -> bool:
        """
        Check if a segment should be filtered out.
        
        Args:
            segment: Segment to check
            
        Returns:
            True if segment should be filtered, False otherwise
        """
        if not segment or len(segment) < 2:
            return True
            
        # Check against filter patterns
        for pattern in self._filter_patterns:
            if pattern.match(segment):
                return True
                
        # Don't filter common words - users might have projects named "for", "with", etc.
        # Only filter if it's a single character (except valid single chars like 'a', 'x')
        if len(segment) == 1 and segment not in {'a', 'x', 'c', 'r', 'v'}:
            return True
            
        return False
    
    def _score_segments(self, segments: List[str], original_path: str) -> Dict[str, float]:
        """
        Score segments by likelihood of being the project name.
        
        Args:
            segments: List of segments to score
            original_path: Original path for context
            
        Returns:
            Dictionary of segment to score (0-1)
        """
        scores = {}
        
        for i, segment in enumerate(segments):
            score = 1.0
            
            # Position scoring - earlier segments after marker are more likely
            position_weight = 1.0 - (i * 0.1)
            score *= max(0.3, position_weight)
            
            # Length scoring - very short or very long segments less likely
            if len(segment) < 3:
                score *= 0.5
            elif len(segment) > 20:
                score *= 0.7
                
            # Case scoring - proper case or lowercase more likely
            if segment.isupper():
                score *= 0.8
                
            # Contains project-like patterns
            if any(indicator in segment.lower() for indicator in ['app', 'project', 'service', 'client', 'server', 'api']):
                score *= 1.2
                
            scores[segment] = min(1.0, score)
            
        return scores
    
    def _generate_search_candidates(self, segments: List[str], scores: Dict[str, float]) -> List[str]:
        """
        Generate search candidates from segments.
        
        Args:
            segments: List of segments
            scores: Segment scores
            
        Returns:
            List of search candidates ordered by likelihood
        """
        candidates = []
        
        # Add individual segments sorted by score
        sorted_segments = sorted(segments, key=lambda s: scores.get(s, 0), reverse=True)
        candidates.extend(sorted_segments[:5])  # Top 5 individual segments
        
        # Add combinations of high-scoring segments
        if len(segments) >= 2:
            # Adjacent pairs
            for i in range(len(segments) - 1):
                combined = f"{segments[i]}-{segments[i+1]}"
                candidates.append(combined)
                
            # Full combination if not too long
            if len(segments) <= 4:
                full_combo = '-'.join(segments)
                candidates.append(full_combo)
        
        # Add lowercase variants
        for candidate in list(candidates):
            lower = candidate.lower()
            if lower not in candidates:
                candidates.append(lower)
                
        return candidates