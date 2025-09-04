#!/usr/bin/env python3
"""
Semantic Exemplar Extractor
Extracts representative code implementations with rich semantic context
instead of generic AST patterns
"""

import re
import json
import logging
from typing import Dict, List, Any, Optional, Set
from dataclasses import dataclass, field, asdict
from collections import defaultdict, Counter
import hashlib

logger = logging.getLogger(__name__)

@dataclass
class SemanticExemplar:
    """A representative code example with semantic context"""
    pattern_type: str  # animation, state, styling, component, logic
    exemplar: str      # The actual code snippet
    semantic_context: Dict[str, Any]
    location: Optional[Dict[str, Any]] = None
    related_patterns: List[str] = field(default_factory=list)
    confidence: float = 1.0
    frequency: int = 1
    
    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)
    
    def signature(self) -> str:
        """Generate a signature for deduplication"""
        return hashlib.md5(self.exemplar.encode()).hexdigest()

class SemanticExemplarExtractor:
    """Extract semantic exemplars from conversations"""
    
    # Pattern categories with detection rules
    PATTERN_CATEGORIES = {
        "animation": {
            "triggers": ["animate=", "motion.", "transition=", "initial=", "variants="],
            "extract_values": True,
            "semantic_keys": ["duration", "easing", "trigger", "values_used"]
        },
        "state": {
            "triggers": ["useState", "useReducer", "setState", "this.state"],
            "extract_values": True,
            "semantic_keys": ["state_type", "initial_value", "update_pattern"]
        },
        "styling": {
            "triggers": ["className=", "style=", "bg-", "text-", "border-", "hover:", "backdrop-"],
            "extract_values": True,
            "semantic_keys": ["style_family", "responsive", "theme", "effects"]
        },
        "component": {
            "triggers": ["<", "/>", "props.", "children"],
            "extract_values": True,
            "semantic_keys": ["component_role", "props_used", "composition_depth"]
        },
        "api": {
            "triggers": ["fetch", "axios", "api.", "async", "await", ".then(", ".catch("],
            "extract_values": True,
            "semantic_keys": ["endpoint", "method", "error_handling", "data_flow"]
        },
        "routing": {
            "triggers": ["router.", "navigation.", "Link", "href=", "push(", "params."],
            "extract_values": True,
            "semantic_keys": ["route_type", "navigation_trigger", "params_used"]
        }
    }
    
    def __init__(self):
        self.exemplars = defaultdict(list)
        self.pattern_frequencies = Counter()
        
    def extract_from_conversation(self, conversation_text: str) -> Dict[str, List[SemanticExemplar]]:
        """Extract semantic exemplars from a conversation"""
        code_blocks = self._extract_code_blocks(conversation_text)
        
        for lang, code in code_blocks:
            self._process_code_block(code, lang)
        
        # Select top exemplars per category
        result = {}
        for category, exemplar_list in self.exemplars.items():
            # Deduplicate and sort by frequency/confidence
            unique_exemplars = self._deduplicate_exemplars(exemplar_list)
            # Select top 5 most representative
            top_exemplars = sorted(unique_exemplars, 
                                  key=lambda x: (x.frequency, x.confidence), 
                                  reverse=True)[:5]
            if top_exemplars:
                result[category] = top_exemplars
        
        return result
    
    def _extract_code_blocks(self, text: str) -> List[tuple]:
        """Extract code blocks from markdown text"""
        code_blocks = []
        pattern = r'```(?:([^\n`]*?)\n)?(.*?)```'
        
        for match in re.finditer(pattern, text, re.DOTALL):
            lang = match.group(1) or ""
            code = match.group(2)
            code_blocks.append((lang.lower(), code))
        
        return code_blocks
    
    def _process_code_block(self, code: str, lang: str):
        """Process a code block to extract exemplars"""
        lines = code.split('\n')
        
        for i, line in enumerate(lines):
            line = line.strip()
            if not line:
                continue
                
            # Determine pattern category
            for category, config in self.PATTERN_CATEGORIES.items():
                if any(trigger in line for trigger in config["triggers"]):
                    # Extract the pattern with context
                    exemplar_code = self._extract_pattern_with_context(lines, i)
                    
                    # Extract semantic context
                    semantic_context = self._extract_semantic_context(
                        exemplar_code, category, config
                    )
                    
                    # Create exemplar
                    exemplar = SemanticExemplar(
                        pattern_type=category,
                        exemplar=exemplar_code,
                        semantic_context=semantic_context,
                        location={"line": i, "language": lang}
                    )
                    
                    # Track frequency
                    sig = exemplar.signature()
                    self.pattern_frequencies[sig] += 1
                    exemplar.frequency = self.pattern_frequencies[sig]
                    
                    self.exemplars[category].append(exemplar)
                    break  # Only categorize once per line
    
    def _extract_pattern_with_context(self, lines: List[str], index: int, context_lines: int = 2) -> str:
        """Extract pattern with surrounding context"""
        start = max(0, index - context_lines)
        end = min(len(lines), index + context_lines + 1)
        
        # Get the main line and essential context
        pattern_lines = []
        
        # Include full statement/expression
        main_line = lines[index]
        pattern_lines.append(main_line)
        
        # Check if this is part of a multi-line expression
        if index > 0:
            prev_line = lines[index - 1].rstrip()
            if prev_line.endswith((',', '{', '[', '(')) or 'return' in prev_line:
                pattern_lines.insert(0, prev_line)
        
        if index < len(lines) - 1:
            next_line = lines[index + 1].rstrip()
            if main_line.rstrip().endswith((',', '{', '[', '(')):
                pattern_lines.append(next_line)
        
        return '\n'.join(pattern_lines).strip()
    
    def _extract_semantic_context(self, code: str, category: str, config: Dict) -> Dict[str, Any]:
        """Extract semantic context from code"""
        context = {
            "category": category,
            "code_length": len(code)
        }
        
        # Extract values based on category
        if category == "animation":
            # Extract animation values
            duration_match = re.search(r'duration:\s*([0-9.]+)', code)
            if duration_match:
                context["duration"] = float(duration_match.group(1))
            
            opacity_match = re.search(r'opacity:\s*([0-9.]+|\w+)', code)
            if opacity_match:
                context["opacity_values"] = opacity_match.group(1)
            
            # Check for conditional animations
            if '?' in code and ':' in code:
                context["conditional"] = True
                context["trigger"] = "conditional_expression"
        
        elif category == "styling":
            # Extract styling patterns
            classes = re.findall(r'className=["\']([^"\']+)["\']', code)
            if classes:
                context["classes"] = classes[0].split()
                
                # Detect glassmorphism
                if any('backdrop-blur' in c for c in context["classes"]):
                    context["style_family"] = "glassmorphism"
                # Detect dark mode
                elif any('dark:' in c for c in context["classes"]):
                    context["theme"] = "dark_mode_aware"
        
        elif category == "state":
            # Extract state patterns
            state_match = re.search(r'useState\(([^)]+)\)', code)
            if state_match:
                initial = state_match.group(1)
                context["initial_value"] = initial
                
                # Determine state type
                if initial in ['true', 'false']:
                    context["state_type"] = "boolean"
                elif initial.startswith('['):
                    context["state_type"] = "array"
                elif initial.startswith('{'):
                    context["state_type"] = "object"
                elif initial.isdigit():
                    context["state_type"] = "number"
                else:
                    context["state_type"] = "string"
        
        elif category == "component":
            # Extract component patterns
            component_match = re.search(r'<(\w+)', code)
            if component_match:
                context["component_name"] = component_match.group(1)
            
            # Check for props
            props = re.findall(r'(\w+)=', code)
            if props:
                context["props_used"] = props
            
            # Check for children
            if 'children' in code or '>{' in code:
                context["has_children"] = True
        
        elif category == "api":
            # Extract API patterns
            if 'fetch' in code:
                context["api_method"] = "fetch"
                url_match = re.search(r'[\'"`]([/\w\-\.]+)[\'"`]', code)
                if url_match:
                    context["endpoint"] = url_match.group(1)
            
            if 'async' in code:
                context["async_pattern"] = "async/await"
            elif '.then(' in code:
                context["async_pattern"] = "promise_chain"
            
            if 'catch' in code or 'try' in code:
                context["error_handling"] = True
        
        return context
    
    def _deduplicate_exemplars(self, exemplar_list: List[SemanticExemplar]) -> List[SemanticExemplar]:
        """Deduplicate exemplars while preserving the best examples"""
        seen_signatures = {}
        deduplicated = []
        
        for exemplar in exemplar_list:
            sig = exemplar.signature()
            if sig not in seen_signatures:
                seen_signatures[sig] = exemplar
                deduplicated.append(exemplar)
            else:
                # Update frequency
                seen_signatures[sig].frequency += 1
        
        return deduplicated
    
    def get_summary(self) -> Dict[str, Any]:
        """Get extraction summary"""
        return {
            "categories_found": list(self.exemplars.keys()),
            "total_exemplars": sum(len(v) for v in self.exemplars.values()),
            "pattern_frequencies": dict(self.pattern_frequencies.most_common(10))
        }


if __name__ == "__main__":
    # Test with example conversation
    test_conversation = '''
    Here's the animation pattern we use:
    
    ```tsx
    <motion.div
      animate={{ 
        opacity: currentSection === 3 ? 1 : 0,
        pointerEvents: currentSection === 3 ? 'auto' : 'none'
      }}
      transition={{ duration: 0.5 }}
    >
      <ContentSection />
    </motion.div>
    ```
    
    And for styling, we consistently use glassmorphism:
    
    ```tsx
    <Card className="bg-black/20 backdrop-blur-xl border border-white/10 rounded-2xl p-6">
      {children}
    </Card>
    ```
    
    For state management:
    
    ```tsx
    const [isOpen, setIsOpen] = useState(false);
    const [data, setData] = useState({ items: [], loading: true });
    ```
    
    API calls follow this pattern:
    
    ```typescript
    const fetchData = async () => {
      try {
        const response = await fetch('/api/articles');
        const json = await response.json();
        setData(json);
      } catch (error) {
        console.error('Failed to fetch:', error);
      }
    };
    ```
    '''
    
    extractor = SemanticExemplarExtractor()
    exemplars = extractor.extract_from_conversation(test_conversation)
    
    print("Semantic Exemplars Extracted:")
    print("="*50)
    for category, exemplar_list in exemplars.items():
        print(f"\n{category.upper()}:")
        for i, exemplar in enumerate(exemplar_list, 1):
            print(f"\n  Exemplar {i}:")
            print(f"    Code: {exemplar.exemplar[:100]}...")
            print(f"    Context: {json.dumps(exemplar.semantic_context, indent=6)}")
            print(f"    Frequency: {exemplar.frequency}")
    
    print("\n" + "="*50)
    print("Summary:", extractor.get_summary())