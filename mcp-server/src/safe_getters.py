"""Safe getter utilities for handling None values consistently."""

import logging
from typing import Any, Dict, List, Optional, Set, Union

logger = logging.getLogger(__name__)


def safe_get_list(
    data: Optional[Dict[str, Any]],
    key: str,
    default: Optional[List] = None
) -> List[Any]:
    """
    Safely get a list field from a dictionary, handling None and non-list values.

    Args:
        data: Dictionary to get value from (can be None)
        key: Key to retrieve
        default: Default value if key not found or value is None

    Returns:
        A list, either the value, converted value, or default/empty list
    """
    if data is None:
        return default if default is not None else []

    value = data.get(key)

    if value is None:
        return default if default is not None else []

    # Handle sets and tuples by converting to list
    if isinstance(value, (set, tuple)):
        return list(value)

    # If it's already a list, return it
    if isinstance(value, list):
        return value

    # If it's not a list-like type, log warning and return empty list
    logger.warning(
        f"Expected list-like type for key '{key}', got {type(value).__name__}. "
        f"Value: {repr(value)[:100]}"
    )
    return default if default is not None else []


def safe_get_str(
    data: Optional[Dict[str, Any]],
    key: str,
    default: str = ""
) -> str:
    """
    Safely get a string field from a dictionary.

    Args:
        data: Dictionary to get value from (can be None)
        key: Key to retrieve
        default: Default value if key not found or value is None

    Returns:
        A string, either the value or the default
    """
    if data is None:
        return default

    value = data.get(key)

    if value is None:
        return default

    # Convert to string if needed
    return str(value)


def safe_get_dict(
    data: Optional[Dict[str, Any]],
    key: str,
    default: Optional[Dict] = None
) -> Dict[str, Any]:
    """
    Safely get a dictionary field from another dictionary.

    Args:
        data: Dictionary to get value from (can be None)
        key: Key to retrieve
        default: Default value if key not found or value is None

    Returns:
        A dictionary, either the value or the default/empty dict
    """
    if data is None:
        return default if default is not None else {}

    value = data.get(key)

    if value is None:
        return default if default is not None else {}

    if isinstance(value, dict):
        return value

    logger.warning(
        f"Expected dict for key '{key}', got {type(value).__name__}. "
        f"Value: {repr(value)[:100]}"
    )
    return default if default is not None else {}


def safe_get_float(
    data: Optional[Dict[str, Any]],
    key: str,
    default: float = 0.0
) -> float:
    """
    Safely get a float field from a dictionary.

    Args:
        data: Dictionary to get value from (can be None)
        key: Key to retrieve
        default: Default value if key not found or value is None/non-numeric

    Returns:
        A float, either the converted value or the default
    """
    if data is None:
        return default

    value = data.get(key)

    if value is None:
        return default

    try:
        return float(value)
    except (TypeError, ValueError) as e:
        logger.warning(
            f"Could not convert key '{key}' value to float: {repr(value)[:100]}. "
            f"Error: {e}"
        )
        return default


def safe_get_int(
    data: Optional[Dict[str, Any]],
    key: str,
    default: int = 0
) -> int:
    """
    Safely get an integer field from a dictionary.

    Args:
        data: Dictionary to get value from (can be None)
        key: Key to retrieve
        default: Default value if key not found or value is None/non-numeric

    Returns:
        An integer, either the converted value or the default
    """
    if data is None:
        return default

    value = data.get(key)

    if value is None:
        return default

    try:
        return int(value)
    except (TypeError, ValueError) as e:
        logger.warning(
            f"Could not convert key '{key}' value to int: {repr(value)[:100]}. "
            f"Error: {e}"
        )
        return default


def safe_get_bool(
    data: Optional[Dict[str, Any]],
    key: str,
    default: bool = False
) -> bool:
    """
    Safely get a boolean field from a dictionary.

    Args:
        data: Dictionary to get value from (can be None)
        key: Key to retrieve
        default: Default value if key not found or value is None

    Returns:
        A boolean, either the value or the default
    """
    if data is None:
        return default

    value = data.get(key)

    if value is None:
        return default

    if isinstance(value, bool):
        return value

    # Handle string booleans
    if isinstance(value, str):
        return value.lower() in ('true', '1', 'yes', 'on')

    # Handle numeric booleans
    try:
        return bool(int(value))
    except (TypeError, ValueError):
        logger.warning(
            f"Could not convert key '{key}' value to bool: {repr(value)[:100]}"
        )
        return default