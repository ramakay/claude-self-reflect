#!/usr/bin/env python3
"""
Pattern Registry for AST-based code pattern extraction
Provides versioned pattern definitions that can be loaded and compiled once at startup
"""

import json
import logging
from pathlib import Path
from typing import Dict, List, Any, Optional
from dataclasses import dataclass, field, asdict
from datetime import datetime

logger = logging.getLogger(__name__)

@dataclass
class PatternDefinition:
    """Definition of a code pattern to extract"""
    id: str
    name: str
    description: str
    languages: List[str]
    pattern: str  # AST-grep pattern or regex
    pattern_type: str = "ast"  # "ast" or "regex"
    confidence: float = 1.0
    version: str = "1.0.0"
    tags: List[str] = field(default_factory=list)
    examples: List[str] = field(default_factory=list)
    
    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)


class PatternRegistry:
    """Registry for managing code patterns with versioning"""
    
    DEFAULT_PATTERNS = [
        # React Hooks
        PatternDefinition(
            id="react_useState",
            name="React useState Hook",
            description="Detects React useState hook usage",
            languages=["javascript", "typescript", "jsx", "tsx"],
            pattern="useState($_)",
            pattern_type="ast",
            confidence=1.0,
            tags=["react", "hooks", "state"],
            examples=["const [count, setCount] = useState(0)"]
        ),
        PatternDefinition(
            id="react_useEffect",
            name="React useEffect Hook",
            description="Detects React useEffect hook usage",
            languages=["javascript", "typescript", "jsx", "tsx"],
            pattern="useEffect($_)",
            pattern_type="ast",
            confidence=1.0,
            tags=["react", "hooks", "effects"],
            examples=["useEffect(() => { fetchData() }, [])"]
        ),
        PatternDefinition(
            id="react_useCallback",
            name="React useCallback Hook",
            description="Detects React useCallback hook usage",
            languages=["javascript", "typescript", "jsx", "tsx"],
            pattern="useCallback($_)",
            pattern_type="ast",
            confidence=1.0,
            tags=["react", "hooks", "optimization"],
            examples=["const memoized = useCallback(() => {}, [deps])"]
        ),
        
        # Async Patterns
        PatternDefinition(
            id="async_await",
            name="Async/Await Pattern",
            description="Detects async/await usage",
            languages=["javascript", "typescript", "python"],
            pattern="async function $FUNC($_) { $$$ }",
            pattern_type="ast",
            confidence=1.0,
            tags=["async", "concurrency"],
            examples=["async function fetchData() { await fetch() }"]
        ),
        
        # Error Handling
        PatternDefinition(
            id="try_catch",
            name="Try/Catch Block",
            description="Detects try/catch error handling",
            languages=["javascript", "typescript", "python", "java"],
            pattern="try { $$$ } catch ($_) { $$$ }",
            pattern_type="ast",
            confidence=1.0,
            tags=["error-handling", "exceptions"],
            examples=["try { risky() } catch (e) { handle(e) }"]
        ),
        
        # API Patterns
        PatternDefinition(
            id="fetch_api",
            name="Fetch API Usage",
            description="Detects fetch API calls",
            languages=["javascript", "typescript"],
            pattern="fetch($_)",
            pattern_type="ast",
            confidence=1.0,
            tags=["api", "http", "network"],
            examples=["await fetch('/api/data')"]
        ),
        
        # Python Patterns
        PatternDefinition(
            id="python_async",
            name="Python Async Function",
            description="Detects Python async functions",
            languages=["python"],
            pattern="async def $FUNC($_): $$$",
            pattern_type="ast",
            confidence=1.0,
            tags=["async", "python", "concurrency"],
            examples=["async def process(): await task()"]
        ),
        PatternDefinition(
            id="python_decorator",
            name="Python Decorator",
            description="Detects Python decorator usage",
            languages=["python"],
            pattern="@$DECORATOR",
            pattern_type="ast",
            confidence=0.9,
            tags=["python", "decorator", "metaprogramming"],
            examples=["@lru_cache", "@property"]
        ),
    ]
    
    def __init__(self, config_path: Optional[Path] = None):
        """Initialize the pattern registry"""
        self.config_path = config_path or Path("~/.claude-self-reflect/config/patterns.json").expanduser()
        self.patterns: Dict[str, PatternDefinition] = {}
        self.version = "1.0.0"
        self.last_updated = datetime.now().isoformat()
        
        # Load patterns
        self._load_patterns()
    
    def _load_patterns(self):
        """Load patterns from config file or use defaults"""
        if self.config_path.exists():
            try:
                with open(self.config_path, 'r') as f:
                    data = json.load(f)
                    self.version = data.get("version", "1.0.0")
                    self.last_updated = data.get("last_updated", datetime.now().isoformat())
                    
                    # Load patterns
                    for pattern_data in data.get("patterns", []):
                        pattern = PatternDefinition(**pattern_data)
                        self.patterns[pattern.id] = pattern
                    
                    logger.info(f"Loaded {len(self.patterns)} patterns from {self.config_path}")
            except Exception as e:
                logger.warning(f"Failed to load patterns from {self.config_path}: {e}")
                self._use_defaults()
        else:
            self._use_defaults()
    
    def _use_defaults(self):
        """Use default patterns"""
        for pattern in self.DEFAULT_PATTERNS:
            self.patterns[pattern.id] = pattern
        logger.info(f"Using {len(self.patterns)} default patterns")
    
    def save(self):
        """Save patterns to config file"""
        try:
            self.config_path.parent.mkdir(parents=True, exist_ok=True)
            
            data = {
                "version": self.version,
                "last_updated": datetime.now().isoformat(),
                "patterns": [p.to_dict() for p in self.patterns.values()]
            }
            
            with open(self.config_path, 'w') as f:
                json.dump(data, f, indent=2)
            
            logger.info(f"Saved {len(self.patterns)} patterns to {self.config_path}")
        except Exception as e:
            logger.error(f"Failed to save patterns: {e}")
    
    def get_patterns_for_language(self, language: str) -> List[PatternDefinition]:
        """Get all patterns applicable to a language"""
        patterns = []
        for pattern in self.patterns.values():
            if language.lower() in [lang.lower() for lang in pattern.languages]:
                patterns.append(pattern)
        return patterns
    
    def get_pattern_by_id(self, pattern_id: str) -> Optional[PatternDefinition]:
        """Get a specific pattern by ID"""
        return self.patterns.get(pattern_id)
    
    def add_pattern(self, pattern: PatternDefinition):
        """Add or update a pattern"""
        self.patterns[pattern.id] = pattern
        self.last_updated = datetime.now().isoformat()
    
    def remove_pattern(self, pattern_id: str) -> bool:
        """Remove a pattern by ID"""
        if pattern_id in self.patterns:
            del self.patterns[pattern_id]
            self.last_updated = datetime.now().isoformat()
            return True
        return False
    
    def get_categories(self) -> Dict[str, List[str]]:
        """Get patterns organized by category (tags)"""
        categories = {}
        for pattern in self.patterns.values():
            for tag in pattern.tags:
                if tag not in categories:
                    categories[tag] = []
                categories[tag].append(pattern.id)
        return categories
    
    def compile_patterns(self) -> Dict[str, Any]:
        """Compile patterns for efficient matching (precompile step)"""
        compiled = {
            "by_language": {},
            "by_category": self.get_categories(),
            "patterns": {}
        }
        
        for lang in ["javascript", "typescript", "python", "jsx", "tsx", "java", "go", "rust"]:
            compiled["by_language"][lang] = [
                p.id for p in self.get_patterns_for_language(lang)
            ]
        
        for pattern_id, pattern in self.patterns.items():
            compiled["patterns"][pattern_id] = {
                "name": pattern.name,
                "pattern": pattern.pattern,
                "type": pattern.pattern_type,
                "confidence": pattern.confidence
            }
        
        return compiled
    
    def get_stats(self) -> Dict[str, Any]:
        """Get registry statistics"""
        return {
            "version": self.version,
            "last_updated": self.last_updated,
            "total_patterns": len(self.patterns),
            "languages": len(set(lang for p in self.patterns.values() for lang in p.languages)),
            "categories": len(self.get_categories()),
            "pattern_types": {
                "ast": sum(1 for p in self.patterns.values() if p.pattern_type == "ast"),
                "regex": sum(1 for p in self.patterns.values() if p.pattern_type == "regex")
            }
        }


# Global registry instance
_registry = None

def get_registry() -> PatternRegistry:
    """Get the global pattern registry instance"""
    global _registry
    if _registry is None:
        _registry = PatternRegistry()
    return _registry


if __name__ == "__main__":
    # Test the registry
    registry = get_registry()
    
    print("Pattern Registry Stats:")
    print(json.dumps(registry.get_stats(), indent=2))
    
    print("\nPatterns for JavaScript:")
    for pattern in registry.get_patterns_for_language("javascript"):
        print(f"  - {pattern.name}: {pattern.description}")
    
    print("\nCategories:")
    for category, pattern_ids in registry.get_categories().items():
        print(f"  {category}: {len(pattern_ids)} patterns")
    
    # Save to disk
    registry.save()
    print(f"\nRegistry saved to: {registry.config_path}")