"""Project name normalization utilities."""

import hashlib
import logging
from pathlib import Path

logger = logging.getLogger(__name__)


class ProjectNormalizer:
    """
    Normalize project names and generate collection names.
    
    This is the CRITICAL component that was broken before.
    Must correctly extract project names from Claude's dash-separated format.
    """
    
    @staticmethod
    def normalize_project_name(project_path: str) -> str:
        """
        Normalize a project path to a consistent project name.
        
        CRITICAL: This must match the implementation in utils.py
        
        Examples:
        - "-Users-name-projects-claude-self-reflect" -> "claude-self-reflect"
        - "claude-self-reflect" -> "claude-self-reflect"
        - "/path/to/-Users-name-projects-myapp" -> "myapp"
        """
        # Get the final component of the path
        if '/' in project_path:
            final_component = project_path.split('/')[-1]
        else:
            final_component = project_path
        
        # Handle Claude's dash-separated format
        if final_component.startswith('-') and 'projects' in final_component:
            # Find the last occurrence of 'projects-'
            idx = final_component.rfind('projects-')
            if idx != -1:
                # Extract everything after 'projects-'
                project_name = final_component[idx + len('projects-'):]
                logger.debug(f"Normalized '{project_path}' to '{project_name}'")
                return project_name
        
        # Already normalized or different format
        logger.debug(f"Project path '{project_path}' already normalized")
        return final_component
    
    def get_project_name(self, file_path: Path) -> str:
        """
        Extract project name from a file path.
        
        Args:
            file_path: Path to a conversation file
            
        Returns:
            Normalized project name
        """
        # Get the parent directory name
        parent_name = file_path.parent.name
        
        # Normalize it
        return self.normalize_project_name(parent_name)
    
    def get_collection_name(self, file_path: Path) -> str:
        """
        Generate collection name for a file.
        
        Format: conv_HASH_local
        Where HASH is first 8 chars of MD5 hash of normalized project name.
        """
        project_name = self.get_project_name(file_path)
        
        # Generate hash
        project_hash = hashlib.md5(project_name.encode()).hexdigest()[:8]
        
        # Generate collection name
        collection_name = f"conv_{project_hash}_local"
        
        logger.debug(f"Collection for project '{project_name}': {collection_name}")
        return collection_name
    
    @staticmethod
    def validate_normalization() -> bool:
        """
        Self-test to ensure normalization is working correctly.
        
        Returns:
            True if all tests pass
        """
        test_cases = [
            ("-Users-name-projects-claude-self-reflect", "claude-self-reflect", "7f6df0fc"),
            ("claude-self-reflect", "claude-self-reflect", "7f6df0fc"),
            ("/Users/name/.claude/projects/-Users-name-projects-myapp", "myapp", None),
            ("-Users-name-projects-procsolve-website", "procsolve-website", "9f2f312b")
        ]
        
        normalizer = ProjectNormalizer()
        all_passed = True
        
        for input_path, expected_name, expected_hash in test_cases:
            normalized = normalizer.normalize_project_name(input_path)
            if normalized != expected_name:
                logger.error(
                    f"Normalization failed: '{input_path}' -> '{normalized}' "
                    f"(expected '{expected_name}')"
                )
                all_passed = False
            
            if expected_hash:
                actual_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
                if actual_hash != expected_hash:
                    logger.error(
                        f"Hash mismatch for '{normalized}': "
                        f"{actual_hash} != {expected_hash}"
                    )
                    all_passed = False
        
        return all_passed