"""Extract concepts and keywords from text."""

import re
import logging
from typing import Dict, Any, Set, List
from collections import Counter

logger = logging.getLogger(__name__)


class ConceptExtractor:
    """Extract key concepts and keywords from conversation text."""
    
    def __init__(self):
        # Technical concepts to look for
        self.tech_patterns = {
            'languages': re.compile(r'\b(python|javascript|typescript|java|rust|go|c\+\+|ruby|php|swift|kotlin)\b', re.IGNORECASE),
            'frameworks': re.compile(r'\b(react|vue|angular|django|flask|fastapi|express|spring|rails|laravel)\b', re.IGNORECASE),
            'databases': re.compile(r'\b(mongodb|postgres|mysql|redis|elasticsearch|dynamodb|sqlite|cassandra)\b', re.IGNORECASE),
            'cloud': re.compile(r'\b(aws|azure|gcp|docker|kubernetes|serverless|lambda|ec2|s3)\b', re.IGNORECASE),
            'tools': re.compile(r'\b(git|npm|yarn|webpack|babel|eslint|pytest|jest|vscode|vim)\b', re.IGNORECASE),
            'concepts': re.compile(r'\b(api|rest|graphql|microservices|ci\/cd|devops|agile|tdd|security|authentication)\b', re.IGNORECASE)
        }
        
        # Common stop words to exclude
        self.stop_words = {
            'the', 'a', 'an', 'and', 'or', 'but', 'in', 'on', 'at', 'to', 'for',
            'of', 'with', 'by', 'from', 'as', 'is', 'was', 'are', 'were', 'been',
            'be', 'have', 'has', 'had', 'do', 'does', 'did', 'will', 'would',
            'should', 'could', 'may', 'might', 'must', 'can', 'shall', 'need'
        }
    
    def extract(self, text: str) -> Dict[str, Any]:
        """
        Extract concepts from text.
        
        Returns:
            Dictionary with concepts list
        """
        concepts = set()
        
        # Extract technical concepts
        for category, pattern in self.tech_patterns.items():
            matches = pattern.findall(text)
            for match in matches:
                concepts.add(match.lower())
        
        # Extract capitalized terms (likely important)
        capitalized = re.findall(r'\b[A-Z][a-z]+(?:\s+[A-Z][a-z]+)*\b', text)
        for term in capitalized[:20]:  # Limit to prevent noise
            if term.lower() not in self.stop_words and len(term) > 3:
                concepts.add(term.lower())
        
        # Extract terms in backticks (code references)
        code_terms = re.findall(r'`([^`]+)`', text)
        for term in code_terms[:20]:
            # Clean and add if it's a reasonable concept
            clean_term = term.strip().lower()
            if len(clean_term) > 2 and len(clean_term) < 50:
                # Skip if it looks like code
                if not any(char in clean_term for char in ['{', '}', '(', ')', ';', '=']):
                    concepts.add(clean_term)
        
        # Extract file extensions mentioned
        extensions = re.findall(r'\b\w+\.(py|js|ts|jsx|tsx|java|go|rs|cpp|c|h|md|json|yaml|yml|xml|html|css|sql)\b', text)
        for ext in extensions[:10]:
            concepts.add(f"file:{ext.split('.')[-1]}")
        
        # Extract error types
        errors = re.findall(r'\b(\w+Error|Exception)\b', text)
        for error in errors[:10]:
            concepts.add(f"error:{error.lower()}")
        
        # Limit total concepts to prevent bloat
        concept_list = list(concepts)[:50]
        
        return {
            "concepts": concept_list,
            "concept_count": len(concept_list)
        }
    
    def extract_topics(self, text: str) -> List[str]:
        """
        Extract higher-level topics from text.
        
        This is a more sophisticated extraction for topic modeling.
        """
        topics = []
        
        # Check for common development topics
        topic_indicators = {
            'debugging': ['debug', 'error', 'bug', 'fix', 'issue', 'problem'],
            'testing': ['test', 'unit test', 'integration', 'pytest', 'jest', 'coverage'],
            'deployment': ['deploy', 'production', 'release', 'ci/cd', 'pipeline'],
            'optimization': ['optimize', 'performance', 'speed', 'efficiency', 'cache'],
            'security': ['security', 'authentication', 'authorization', 'encryption', 'vulnerability'],
            'database': ['database', 'sql', 'query', 'schema', 'migration'],
            'api': ['api', 'endpoint', 'rest', 'graphql', 'webhook'],
            'frontend': ['ui', 'ux', 'component', 'react', 'vue', 'css', 'style'],
            'backend': ['server', 'backend', 'api', 'database', 'microservice'],
            'architecture': ['architecture', 'design pattern', 'structure', 'refactor']
        }
        
        text_lower = text.lower()
        for topic, indicators in topic_indicators.items():
            if any(indicator in text_lower for indicator in indicators):
                topics.append(topic)
        
        return topics[:5]  # Return top 5 topics