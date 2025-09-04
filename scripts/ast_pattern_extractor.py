#!/usr/bin/env python3
"""
AST Pattern Extractor for Claude Self-Reflect
Extracts structural code patterns from conversations using ast-grep-py
"""

import os
import time
import logging
import math
from typing import Dict, List, Set, Optional, Any
import re
import signal
from contextlib import contextmanager
from datetime import datetime, timezone

# Try to import ast-grep-py, but allow graceful fallback
try:
    from ast_grep_py import SgRoot  # type: ignore
    AST_GREP_AVAILABLE = True
except Exception:
    SgRoot = None  # type: ignore
    AST_GREP_AVAILABLE = False

logger = logging.getLogger(__name__)

# Environment configuration
AST_GREP_ENABLED = os.getenv("AST_GREP_ENABLED", "true").lower() == "true" and AST_GREP_AVAILABLE
AST_TIMEOUT_SECONDS = float(os.getenv("AST_TIMEOUT_SECONDS", "2.0"))
MAX_CODE_BYTES = int(os.getenv("AST_MAX_CODE_BYTES", "100000"))
MAX_CODE_LINES = int(os.getenv("AST_MAX_CODE_LINES", "2000"))

# Supported languages mapping
LANGUAGE_MAP = {
    "python": "python",
    "py": "python",
    "javascript": "javascript",
    "js": "javascript",
    "typescript": "typescript",
    "ts": "typescript",
    "tsx": "tsx",
    "jsx": "jsx",
    "go": "go",
    "rust": "rust",
    "java": "java",
    "cpp": "cpp",
    "c++": "cpp",
    "c": "c",
}

# Pattern definitions for each category - Enhanced with catalog patterns
PATTERN_DEFINITIONS = {
    "react_hooks": [
        "use($$$)",  # React 19
        "useState($$$)",
        "useEffect($$$)",
        "useCallback($$$)",
        "useMemo($$$)",
        "useRef($$$)",
        "useContext($$$)",
        "useReducer($$$)",
        "useLayoutEffect($$$)",
        "useQuery($$$)",  # React Query
        "useMutation($$$)",
        "useSelector($$$)",  # Redux
        "useDispatch($$$)",
    ],
    "async_patterns": [
        "async function $FUNC($$$)",
        "async ($$$) => $$$",
        "await $EXPR",
        "Promise.$METHOD($$$)",
        "new Promise($$$)",
        ".then($$$)",
        ".catch($$$)",
        ".finally($$$)",
        "Promise.all($$$)",
        "Promise.race($$$)",
        "Promise.resolve($$$)",
        "async def $FUNC($$$)",  # Python async
        "await $FUNC($$$)",  # Generic await
    ],
    "error_handling": [
        "try { $$$ } catch",
        "try { $$$ } catch ($ERR) { $$$ }",
        "throw new $ERROR($$$)",
        "throw $EXPR",
        ".catch($$$)",
        "if (err) { $$$ }",
        "if (error) { $$$ }",
        "console.error($$$)",
        "logger.error($$$)",
        "except $EXCEPTION:",  # Python
        "raise $EXCEPTION",  # Python
    ],
    "import_patterns": [
        "import $NAME from '$MODULE'",  # ES6 default import
        "import {$$$NAMES} from '$MODULE'",  # ES6 named import
        "import * as $NAME from '$MODULE'",  # ES6 namespace import
        "const $VAR = require('$MODULE')",  # CommonJS
        "require('$MODULE')",  # CommonJS inline
        "from $MODULE import $$$NAMES",  # Python import
        "import $MODULE",  # Python simple import
        "dynamic import($MODULE)",  # Dynamic import
    ],
    "type_patterns": [
        "interface $NAME {$$$}",  # TypeScript interface
        "type $NAME = $$$",  # TypeScript type alias
        ": $TYPE",  # Type annotation
        "as $TYPE",  # Type assertion
        "<$TYPE>",  # Generic type
        "extends $TYPE",  # Type extension
        "implements $TYPE",  # Interface implementation
        "-> $TYPE",  # Python return type
        ": $TYPE",  # Python type hint
        "Optional[$TYPE]",  # Python optional
        "List[$TYPE]",  # Python list type
        "Dict[$KEY, $VALUE]",  # Python dict type
    ],
    "framework_patterns": [
        # React/Next.js
        "export default function $COMPONENT($$$)",
        "export const $COMPONENT = ($$$) =>",
        "<$COMPONENT $$$/>",  # JSX component
        "getServerSideProps($$$)",  # Next.js
        "getStaticProps($$$)",  # Next.js
        # Angular
        "@Component({$$$})",
        "@Injectable($$$)",
        "@NgModule({$$$})",
        # Vue
        "Vue.component($$$)",
        "export default {$$$}",  # Vue component
        # Express/Node
        "app.use($$$)",
        "app.listen($$$)",
        "router.$METHOD($PATH, $$$)",
        # Django/FastAPI
        "@app.$METHOD($PATH)",  # FastAPI
        "@router.$METHOD($PATH)",  # FastAPI
        "models.Model",  # Django
        "serializers.$TYPE",  # Django REST
    ],
    "database_patterns": [
        "SELECT $$$",
        "INSERT INTO $$$",
        "UPDATE $TABLE SET $$$",
        "DELETE FROM $$$",
        "JOIN $TABLE ON $$$",
        "WHERE $CONDITION",
        ".find($$$)",  # MongoDB
        ".findOne($$$)",  # MongoDB
        ".aggregate($$$)",  # MongoDB
        ".query($$$)",  # Generic query
        "db.session.$METHOD($$$)",  # SQLAlchemy
        "migrate($$$)",  # Migrations
        "schema.$METHOD($$$)",  # Schema operations
    ],
    "testing_patterns": [
        "describe($$$)",
        "it($$$)",
        "test($$$)",
        "expect($$$)",
        "assert($$$)",
        "beforeEach($$$)",
        "afterEach($$$)",
        "jest.$METHOD($$$)",
        "mock($$$)",
        "spy($$$)",
        "@pytest.$DECORATOR",  # Pytest
        "@fixture",  # Pytest fixture
        "def test_$NAME($$$)",  # Python test
        "@Test",  # JUnit
        "assertEquals($$$)",  # JUnit
    ],
    "auth_patterns": [
        "jwt.sign($$$)",
        "jwt.verify($$$)",
        "bcrypt.hash($$$)",
        "bcrypt.compare($$$)",
        "session.$METHOD($$$)",
        "passport.$METHOD($$$)",
        "Bearer $TOKEN",
        "Authorization: $$$",
        "@auth_required",  # Python decorator
        "login_required",  # Django
        "authenticate($$$)",
        "authorize($$$)",
    ],
    "state_management": [
        "setState($$$)",  # React class component
        "dispatch($$$)",  # Redux
        "commit($$$)",  # Vuex
        "store.$METHOD($$$)",  # Generic store
        "createSlice($$$)",  # Redux Toolkit
        "createStore($$$)",
        "useStore($$$)",
        "Provider value=$$$",  # Context provider
    ],
    "validation_patterns": [
        "z.object($$$)",  # Zod
        "yup.object($$$)",  # Yup
        "joi.object($$$)",  # Joi
        "validator.$METHOD($$$)",
        "sanitize($$$)",
        "escape($$$)",
        "validate($$$)",
        "is_valid($$$)",  # Python
        "clean_$FIELD($$$)",  # Django
    ],
    "api_patterns": [
        "fetch($URL)",
        "axios.$METHOD($$$)",
        "app.get($PATH, $$$)",
        "app.post($PATH, $$$)",
        "app.put($PATH, $$$)",
        "app.delete($PATH, $$$)",
        "router.$METHOD($$$)",
        "@Get($$$)",
        "@Post($$$)",
        "@Put($$$)",
        "@Delete($$$)",
        "REST$METHOD($$$)",
        "GraphQL($$$)",
        "webhook($$$)",
    ],
    "performance_patterns": [
        "useMemo($$$)",
        "useCallback($$$)",
        "memo($$$)",  # React.memo
        "lazy(() => $$$)",  # React lazy loading
        "Suspense",  # React Suspense
        "debounce($$$)",
        "throttle($$$)",
        "requestAnimationFrame($$$)",
        "requestIdleCallback($$$)",
        "cache.$METHOD($$$)",
        "memoize($$$)",
    ],
}

@contextmanager
def timeout(seconds: float):
    """Context manager for timeout protection (best-effort on Unix only)."""
    import threading
    
    # Check if we can use SIGALRM (Unix, main thread only)
    if (not hasattr(signal, "SIGALRM") 
        or os.name == "nt"
        or threading.current_thread() is not threading.main_thread()):
        # Unsupported platform or not main thread; no-op to avoid crashes
        # The asyncio timeout in streaming-watcher will handle timeouts
        yield
        return
    
    def timeout_handler(signum, frame):
        raise TimeoutError(f"Operation timed out after {seconds} seconds")
    
    # Set the signal handler and alarm
    old_handler = signal.signal(signal.SIGALRM, timeout_handler)
    signal.alarm(math.ceil(seconds))
    
    try:
        yield
    finally:
        # Restore the original handler and cancel the alarm
        signal.alarm(0)
        signal.signal(signal.SIGALRM, old_handler)

def extract_code_blocks(text: str) -> List[Dict[str, str]]:
    """Extract code blocks from conversation text"""
    code_blocks = []
    seen_blocks = set()  # To avoid duplicates
    
    # Clean up conversation prefixes that might interfere
    # Remove "A: ```" patterns but keep the backticks
    text = re.sub(r'A:\s*```', '```', text)
    
    # More flexible pattern - handles variations in formatting
    # Optional language specifier, optional newline, no newline requirement before closing
    pattern = r'```(?:([^\n`]*?)\n)?(.*?)```'
    matches = re.findall(pattern, text, re.DOTALL)
    
    for info, code in matches:
        # Skip if we've seen this exact block before
        block_hash = hash((info, code))
        if block_hash in seen_blocks:
            continue
        seen_blocks.add(block_hash)
        
        lang = None
        if info:
            # Take the first token before whitespace as language
            parts = info.strip().split()
            if parts:
                lang = parts[0].strip().lower()
        if not lang:
            # Try to detect language from content
            lang = detect_language(code)
        
        if lang and code.strip():
            code_blocks.append({
                "language": lang.lower(),
                "code": code
            })
    
    # Skip inline code extraction - it's too noisy and rarely contains full patterns
    # Only extract triple-backtick code blocks for AST parsing
    
    return code_blocks

def detect_language(code: str) -> Optional[str]:
    """Simple language detection based on patterns"""
    # Enhanced React detection - check for common React patterns
    react_indicators = [
        r'<[A-Za-z][A-Za-z0-9]*[\s/>]',  # Any JSX tag
        r'</[A-Za-z]+>',  # Closing tags
        r'\buseState\s*\(',  # Hooks
        r'\buseEffect\s*\(',
        r'\buseCallback\s*\(',
        r'\buseMemo\s*\(',
        r'\buseRef\s*\(',
        r'export\s+default\s+function',  # Component patterns
        r'const\s+\w+\s*=\s*\([^)]*\)\s*=>', # Arrow function components
        r'return\s*\(',  # Return with JSX
        r'return\s*<',  # Return JSX directly
        r'className=',  # React-specific prop
        r'onClick=',  # React event handlers
        r'onChange=',
        r'from\s+[\'"]react',  # React imports
    ]
    
    if any(re.search(pattern, code) for pattern in react_indicators):
        # Determine if it's TSX or JSX based on TypeScript features
        if re.search(r"(interface |type |enum |:\s*(string|number|boolean|any)\b)", code):
            return "tsx"
        return "jsx"
    
    patterns = {
        "typescript": r"(interface |type |enum |const.*:.*=|let.*:.*=)",
        "javascript": r"(const |let |var |function |console\.|require\()",
        "python": r"(import |from |def |class |if __name__|print\()",
        "go": r"(package |func |import \(|fmt\.|defer )",
        "rust": r"(fn |let mut |impl |pub fn |use |mod )",
        "java": r"(public class |private |protected |import java\.|System\.out)",
        "cpp": r"(#include\s*<|std::|::std|template\s*<)",
        "c": r"(#include\s*<stdio|#include\s*<stdlib|#include\s*<string)",
    }
    
    for lang, pattern in patterns.items():
        if re.search(pattern, code):
            return lang
    
    return None

# Category to language mapping - restrict patterns to relevant languages
CATEGORY_LANGS = {
    "react_hooks": {"javascript", "typescript", "tsx", "jsx"},
    "async_patterns": {"javascript", "typescript", "tsx", "jsx", "python", "go", "rust"},
    "error_handling": {"javascript", "typescript", "tsx", "jsx", "python", "java", "cpp", "c", "go", "rust"},
    "import_patterns": {"javascript", "typescript", "tsx", "jsx", "python", "java", "go", "rust"},
    "type_patterns": {"typescript", "tsx", "python", "java", "go", "rust"},
    "framework_patterns": {"javascript", "typescript", "tsx", "jsx", "python", "java"},
    "database_patterns": {"javascript", "typescript", "tsx", "jsx", "python", "java", "go"},
    "api_patterns": {"javascript", "typescript", "tsx", "jsx", "python", "java", "go"},
    "auth_patterns": {"javascript", "typescript", "tsx", "jsx", "python", "java"},
    "state_management": {"javascript", "typescript", "tsx", "jsx"},
    "validation_patterns": {"javascript", "typescript", "tsx", "jsx", "python"},
    "testing_patterns": {"javascript", "typescript", "tsx", "jsx", "python", "java", "go", "rust"},
    "performance_patterns": {"javascript", "typescript", "tsx", "jsx"},
}

def extract_patterns_from_code(code: str, language: str) -> Dict[str, Set[str]]:
    """Extract AST patterns from a code block"""
    if not AST_GREP_ENABLED:
        return {}
    
    # Normalize language name
    language = LANGUAGE_MAP.get(language.lower(), language.lower())
    
    if language not in ["python", "javascript", "typescript", "tsx", "jsx", "go", "rust", "java", "c", "cpp"]:
        logger.debug(f"Unsupported language for AST parsing: {language}")
        return {}
    
    patterns_found = {}
    
    try:
        with timeout(AST_TIMEOUT_SECONDS):
            # Parse the code
            root = SgRoot(code, language)
            tree = root.root()
            
            # Check each pattern category
            for category, patterns in PATTERN_DEFINITIONS.items():
                # Skip categories not relevant for this language
                if language not in CATEGORY_LANGS.get(category, {language}):
                    continue
                found_patterns = set()
                
                # Handle nested patterns (like design_patterns)
                if isinstance(patterns, dict):
                    for subcategory, subpatterns in patterns.items():
                        for pattern in subpatterns:
                            try:
                                if tree.find(pattern=pattern):
                                    found_patterns.add(subcategory)
                                    break  # One match is enough for the subcategory
                            except Exception as e:
                                logger.debug(f"Pattern matching error: {e}")
                                continue
                else:
                    # Simple pattern list
                    for pattern in patterns:
                        try:
                            matches = tree.find_all(pattern=pattern)
                            if matches:
                                # Extract the pattern name, not the matched text
                                # For patterns like "useState($$$)", extract "useState"
                                pattern_name = pattern.split('(')[0].strip()
                                # Remove meta-variables like $FUNC
                                pattern_name = pattern_name.replace('$FUNC', '').replace('$VAR', '').strip()
                                if pattern_name:
                                    found_patterns.add(pattern_name)
                        except Exception as e:
                            logger.debug(f"Pattern matching error: {e}")
                            continue
                
                if found_patterns:
                    patterns_found[category] = found_patterns
    
    except TimeoutError:
        logger.warning(f"AST parsing timed out for {language} code")
        return {}
    except Exception as e:
        logger.debug(f"AST parsing error: {e}")
        return {}
    
    return patterns_found

def extract_key_from_match(match_text: str, category: str) -> Optional[str]:
    """Extract the key identifier from a matched pattern"""
    # For React hooks, extract the hook name
    if category == "react_hooks":
        match = re.match(r'(use\w+)', match_text)
        if match:
            return match.group(1)
    
    # For async patterns, extract the type
    elif category == "async_patterns":
        if "async" in match_text:
            return "async/await"
        elif "Promise" in match_text:
            return "Promise"
        elif ".then" in match_text:
            return "promise-chain"
    
    # For error handling
    elif category == "error_handling":
        if "try" in match_text:
            return "try/catch"
        elif "throw" in match_text:
            return "throw"
        elif ".catch" in match_text:
            return "catch-handler"
    
    # For API patterns
    elif category == "api_patterns":
        if "fetch" in match_text:
            return "fetch"
        elif "axios" in match_text:
            return "axios"
        elif "@" in match_text:
            # Decorator-based API
            match = re.match(r'@(\w+)', match_text)
            if match:
                return f"@{match.group(1)}"
        else:
            # Express-style routes
            match = re.search(r'\.(get|post|put|delete|patch)', match_text)
            if match:
                return match.group(1).upper()
    
    # For security patterns
    elif category == "security_patterns":
        for keyword in ["sanitize", "escape", "validate", "bcrypt", "jwt", "crypto"]:
            if keyword in match_text.lower():
                return keyword
    
    # For testing patterns
    elif category == "testing_patterns":
        for keyword in ["describe", "test", "it", "expect", "assert"]:
            if keyword in match_text:
                return keyword
    
    return None

def extract_code_metrics(code: str, language: str) -> Dict[str, int]:
    """
    Extract code metrics from a code block using ast-grep
    
    Args:
        code: The code string to analyze
        language: The programming language
    
    Returns:
        Dictionary with code metrics (loc, functions, classes, imports)
    """
    metrics = {
        "loc": len(code.splitlines()),
        "functions": 0,
        "classes": 0,
        "imports": 0,
    }
    
    if not AST_GREP_ENABLED:
        return metrics
    
    # Normalize language name
    language = LANGUAGE_MAP.get(language.lower(), language.lower())
    
    # Skip unsupported languages
    supported_languages = ["python", "javascript", "typescript", "tsx", "jsx", "go", "rust", "java", "c", "cpp"]
    if language not in supported_languages:
        return metrics
    
    try:
        # Parse the code
        root = SgRoot(code, language)
        tree = root.root()
        
        # Count based on language
        if language == "python":
            metrics["functions"] = len(tree.find_all(kind="function_definition"))
            metrics["classes"] = len(tree.find_all(kind="class_definition"))
            # Python has two import types
            metrics["imports"] = len(tree.find_all(kind="import_statement")) + \
                                len(tree.find_all(kind="import_from_statement"))
        
        elif language in ["javascript", "typescript", "jsx", "tsx"]:
            # JavaScript/TypeScript functions
            metrics["functions"] = len(tree.find_all(kind="function_declaration")) + \
                                  len(tree.find_all(kind="arrow_function")) + \
                                  len(tree.find_all(kind="function_expression"))
            metrics["classes"] = len(tree.find_all(kind="class_declaration"))
            metrics["imports"] = len(tree.find_all(kind="import_statement"))
        
        elif language == "go":
            metrics["functions"] = len(tree.find_all(kind="function_declaration"))
            metrics["classes"] = len(tree.find_all(kind="type_spec"))  # Go uses type specs
            metrics["imports"] = len(tree.find_all(kind="import_declaration"))
        
        elif language == "java":
            metrics["functions"] = len(tree.find_all(kind="method_declaration"))
            metrics["classes"] = len(tree.find_all(kind="class_declaration"))
            metrics["imports"] = len(tree.find_all(kind="import_declaration"))
            
    except Exception as e:
        logger.debug(f"Error extracting metrics: {e}")
    
    return metrics

def _empty_result(method: str, languages=None, blocks=0, time_s=None, metrics=None) -> Dict[str, Any]:
    """Return a consistent empty result structure"""
    return {
        "code_patterns": {},
        "languages_detected": list(languages or []),
        "extraction_method": method,
        "extraction_time": round(time_s, 3) if time_s is not None else None,
        "blocks_processed": blocks,
        "code_metrics": metrics or {},
    }

def extract_code_patterns(conversation_text: str, max_blocks: int = 10) -> Dict[str, Any]:
    """
    Main function to extract code patterns from conversation text
    
    Args:
        conversation_text: The full conversation text
        max_blocks: Maximum number of code blocks to process
    
    Returns:
        Dictionary with extracted patterns and metadata
    """
    if not AST_GREP_ENABLED:
        return _empty_result("disabled")
    
    start_time = time.time()
    
    # Extract code blocks from the conversation
    code_blocks = extract_code_blocks(conversation_text)[:max_blocks]
    
    if not code_blocks:
        return _empty_result("no_code_found", time_s=time.time() - start_time)
    
    # Aggregate patterns and metrics across all code blocks
    aggregated_patterns = {}
    languages_detected = set()
    blocks_processed = 0
    
    # Initialize aggregated metrics
    aggregated_metrics = {
        "total_loc": 0,
        "language_loc": {},
        "total_functions": 0,
        "total_classes": 0,
        "total_imports": 0,
        "block_count": len(code_blocks)
    }
    
    for block in code_blocks:
        language = block["language"]
        code = block["code"]
        
        # Skip oversized blocks
        if len(code.encode("utf-8")) > MAX_CODE_BYTES or code.count("\n") > MAX_CODE_LINES:
            logger.debug("Skipping oversized code block")
            continue
        
        blocks_processed += 1
        
        # Normalize language for consistency
        normalized_lang = LANGUAGE_MAP.get(language.lower(), language.lower())
        languages_detected.add(normalized_lang)
        
        # Extract patterns from this block
        patterns = extract_patterns_from_code(code, language)
        
        # Extract metrics from this block
        metrics = extract_code_metrics(code, language)
        
        # Aggregate metrics
        aggregated_metrics["total_loc"] += metrics["loc"]
        aggregated_metrics["total_functions"] += metrics["functions"]
        aggregated_metrics["total_classes"] += metrics["classes"]
        aggregated_metrics["total_imports"] += metrics["imports"]
        
        # Track LoC per language
        if normalized_lang not in aggregated_metrics["language_loc"]:
            aggregated_metrics["language_loc"][normalized_lang] = 0
        aggregated_metrics["language_loc"][normalized_lang] += metrics["loc"]
        
        # Merge patterns
        for category, found_patterns in patterns.items():
            if category not in aggregated_patterns:
                aggregated_patterns[category] = set()
            aggregated_patterns[category].update(found_patterns)
    
    # Convert sets to lists for JSON serialization
    for category in aggregated_patterns:
        aggregated_patterns[category] = list(aggregated_patterns[category])
    
    elapsed_time = time.time() - start_time
    
    return {
        "code_patterns": aggregated_patterns,
        "code_metrics": aggregated_metrics,
        "languages_detected": list(languages_detected),
        "extraction_method": "ast-grep",
        "extraction_time": round(elapsed_time, 3),
        "blocks_processed": blocks_processed
    }

def extract_code_patterns_with_fallback(conversation_text: str) -> Dict[str, Any]:
    """
    Extract code patterns with fallback to regex if AST parsing fails
    """
    try:
        # Try AST-based extraction first
        result = extract_code_patterns(conversation_text)
        
        # If AST extraction failed or was disabled, fall back to regex
        if not result.get("code_patterns") and result.get("extraction_method") != "no_code_found":
            result = extract_patterns_with_regex(conversation_text)
            result["extraction_method"] = "regex_fallback"
        
        return result
    
    except Exception as e:
        logger.error(f"Pattern extraction failed: {e}")
        # Final fallback
        return extract_patterns_with_regex(conversation_text)

def extract_patterns_with_regex(text: str) -> Dict[str, Any]:
    """Fallback regex-based pattern extraction"""
    patterns = {}
    
    # Simple regex patterns for fallback
    regex_patterns = {
        "react_hooks": r'\b(use[A-Z]\w+)\s*\(',
        "async_patterns": r'\b(async|await|Promise|\.then|\.catch|\.finally)\b',
        "error_handling": r'\b(try|catch|throw|finally)\b',
        "api_patterns": r'\b(fetch|axios|app\.(get|post|put|delete|patch)|@(Get|Post|Put|Delete|Patch))\b',
        "security_patterns": r'\b(sanitize|escape|validate|bcrypt|jwt|crypto)\b',
        "testing_patterns": r'\b(describe|test|it|expect|assert|beforeEach|afterEach)\b',
    }
    
    for category, pattern in regex_patterns.items():
        matches = re.findall(pattern, text, re.IGNORECASE)
        if matches:
            # Flatten tuples and filter unique
            flat_matches = []
            for match in matches:
                if isinstance(match, tuple):
                    flat_matches.extend([m for m in match if m])
                else:
                    flat_matches.append(match)
            
            unique_matches = list(set(flat_matches))[:10]  # Limit to 10 unique patterns
            if unique_matches:
                patterns[category] = unique_matches
    
    return {
        "code_patterns": patterns,
        "languages_detected": [],
        "extraction_method": "regex_fallback"
    }

def update_qdrant_with_patterns(client, collection_name: str, point_id: str, patterns: Dict) -> bool:
    """Update a Qdrant point with extracted patterns"""
    try:
        if not patterns.get("code_patterns"):
            return False
            
        # Prepare metadata update
        metadata = {
            "code_patterns": patterns["code_patterns"],
            "languages_detected": patterns.get("languages_detected", []),
            "extraction_method": patterns.get("extraction_method", "unknown"),
            "patterns_extracted_at": datetime.now(timezone.utc).isoformat()
        }
        
        # Use set_payload to update just the metadata
        client.set_payload(
            collection_name=collection_name,
            payload=metadata,
            points=[point_id],
            wait=False
        )
        
        return True
        
    except Exception as e:
        print(f"Error updating point {point_id}: {e}")
        return False

def process_collection_patterns(collection_name: str, project_filter: str = None, limit: int = None):
    """Process a collection and update patterns in Qdrant"""
    from qdrant_client import QdrantClient
    import os
    
    # Connect to Qdrant
    client = QdrantClient(
        url=os.getenv("QDRANT_URL", "http://localhost:6333"),
        api_key=os.getenv("QDRANT_API_KEY")
    )
    
    try:
        # Get collection info
        collection_info = client.get_collection(collection_name)
        total_points = collection_info.points_count
        
        if total_points == 0:
            print(f"Collection {collection_name} is empty")
            return
            
        # Scroll through points
        offset = None
        processed = 0
        updated = 0
        batch_size = 100
        
        print(f"Processing collection {collection_name} with {total_points} points")
        if project_filter:
            print(f"Filtering for project: {project_filter}")
        
        while True:
            # Get batch of points
            result = client.scroll(
                collection_name=collection_name,
                limit=batch_size,
                offset=offset,
                with_payload=True,
                with_vectors=False
            )
            
            points, next_offset = result
            
            if not points:
                break
            
            print(f"Processing batch of {len(points)} points")
                
            for point in points:
                # Check project filter if specified
                if project_filter:
                    point_project = point.payload.get("project", "")
                    if project_filter not in point_project:
                        print(f"Skipping point - project '{point_project}' doesn't match filter '{project_filter}'")
                        continue
                
                # Get conversation content (try both 'content' and 'text' fields)
                content = point.payload.get("content", "") or point.payload.get("text", "")
                if not content:
                    print(f"Skipping point {point.id} - no content or text")
                    continue
                
                print(f"Processing point {point.id} with {len(content)} chars")
                    
                # Extract patterns  
                patterns = extract_code_patterns_with_fallback(content)
                
                # Update if patterns found
                if patterns and patterns.get("code_patterns"):
                    if update_qdrant_with_patterns(client, collection_name, point.id, patterns):
                        updated += 1
                        print(f"Updated point {point.id} with patterns: {list(patterns['code_patterns'].keys())}")
                
                processed += 1
                
                if limit and processed >= limit:
                    break
            
            if limit and processed >= limit:
                break
                
            offset = next_offset
            if offset is None:
                break
        
        print(f"\nProcessed {processed} points, updated {updated} with patterns")
        
    except Exception as e:
        print(f"Error processing collection {collection_name}: {e}")
        import traceback
        traceback.print_exc()

if __name__ == "__main__":
    import argparse
    
    parser = argparse.ArgumentParser(description="Extract and update AST patterns in Qdrant")
    parser.add_argument("--collection", help="Specific collection to process")
    parser.add_argument("--project", help="Filter by project name")
    parser.add_argument("--limit", type=int, help="Limit number of points to process")
    parser.add_argument("--test", action="store_true", help="Run test extraction")
    
    args = parser.parse_args()
    
    if args.test:
        # Test the extractor
        test_conversation = """
    Let me help you create a React component with hooks:
    
    ```javascript
    import React, { useState, useEffect } from 'react';
    
    function UserProfile({ userId }) {
        const [user, setUser] = useState(null);
        const [loading, setLoading] = useState(true);
        
        useEffect(() => {
            async function fetchUser() {
                try {
                    const response = await fetch(`/api/users/${userId}`);
                    const data = await response.json();
                    setUser(data);
                } catch (error) {
                    console.error('Failed to fetch user:', error);
                } finally {
                    setLoading(false);
                }
            }
            
            fetchUser();
        }, [userId]);
        
        return <div>{loading ? 'Loading...' : user?.name}</div>;
    }
    ```
    
    This component uses React hooks and async/await for data fetching.
    """
        
        result = extract_code_patterns_with_fallback(test_conversation)
        print("Extracted patterns:")
        for category, patterns in result["code_patterns"].items():
            print(f"  {category}: {patterns}")
        print(f"Languages: {result['languages_detected']}")
        print(f"Method: {result['extraction_method']}")
        print(f"Time: {result.get('extraction_time', 'N/A')}s")
    
    elif args.collection:
        # Process specific collection - don't filter by project when processing specific collection
        process_collection_patterns(args.collection, None, args.limit)
    
    elif args.project:
        # Process all collections for a project
        from qdrant_client import QdrantClient
        import os
        
        client = QdrantClient(
            url=os.getenv("QDRANT_URL", "http://localhost:6333"),
            api_key=os.getenv("QDRANT_API_KEY")
        )
        
        # Get all collections
        collections = client.get_collections().collections
        project_collections = []
        
        for collection in collections:
            # Check if collection might contain project data
            if args.project.replace('-', '_') in collection.name or args.project in collection.name:
                project_collections.append(collection.name)
        
        if not project_collections:
            print(f"No collections found for project: {args.project}")
        else:
            print(f"Found {len(project_collections)} collections for project {args.project}")
            for collection_name in project_collections:
                print(f"\nProcessing collection: {collection_name}")
                process_collection_patterns(collection_name, args.project, args.limit)
    
    else:
        print("Please specify --test, --collection, or --project")
        parser.print_help()