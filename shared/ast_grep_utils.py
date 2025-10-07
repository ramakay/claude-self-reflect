"""
AST-Grep Utilities
Gracefully handles AST-Grep availability for code quality features
"""
import shutil
import subprocess
from functools import wraps
from typing import Optional

def is_ast_grep_installed() -> bool:
    """Check if ast-grep or sg (npm install) is available in PATH"""
    return shutil.which('ast-grep') is not None or shutil.which('sg') is not None

def get_ast_grep_command() -> Optional[str]:
    """Get the available AST-Grep command (ast-grep or sg)"""
    if shutil.which('ast-grep'):
        return 'ast-grep'
    elif shutil.which('sg'):
        return 'sg'
    return None

def get_ast_grep_version() -> Optional[str]:
    """Get installed AST-Grep version"""
    cmd = get_ast_grep_command()
    if not cmd:
        return None

    try:
        result = subprocess.run(
            [cmd, '--version'],
            capture_output=True,
            text=True,
            timeout=5
        )
        if result.returncode == 0:
            return result.stdout.strip()
    except Exception:
        pass

    return None

def check_ast_grep_or_warn(feature_name: str = "Quality analysis") -> bool:
    """
    Check if AST-Grep is installed, warn if not

    Returns:
        True if installed, False otherwise
    """
    if is_ast_grep_installed():
        return True

    print(f"⚠️  {feature_name} requires AST-Grep")
    print("   This is an optional feature for advanced code quality analysis")
    print()
    print("   To install:")
    print("   • macOS:  brew install ast-grep")
    print("   • Linux:  npm install -g @ast-grep/cli")
    print("   • Or:     Download from https://github.com/ast-grep/ast-grep/releases")
    print()
    print("   ✅ Core MCP functionality works fine without it!")
    print()

    return False

def ast_grep_required(func):
    """Decorator to mark functions that require AST-Grep"""
    @wraps(func)
    def wrapper(*args, **kwargs):
        if not check_ast_grep_or_warn(func.__name__):
            return None
        return func(*args, **kwargs)

    return wrapper

# Example usage in scripts:
# from shared.ast_grep_utils import ast_grep_required, check_ast_grep_or_warn
#
# @ast_grep_required
# def run_quality_analysis():
#     # AST-Grep code here
#     pass
#
# if __name__ == "__main__":
#     if check_ast_grep_or_warn("Code Quality Scanner"):
#         # Run AST-Grep features
#         pass
#     else:
#         # Fallback to basic analysis
#         print("Running basic quality checks without AST-Grep...")
