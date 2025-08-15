"""Main entry point for claude-reflect MCP server."""

import argparse

def main():
    """Main entry point for the claude-reflect script."""
    # Parse the command-line arguments to determine the transport protocol.
    parser = argparse.ArgumentParser(description="claude-reflect")
    parser.add_argument(
        "--transport",
        choices=["stdio", "sse"],
        default="stdio",
        help="Transport protocol for MCP server (default: stdio)"
    )
    parser.add_argument(
        "--status",
        action="store_true",
        help="Get indexing status as JSON with overall and per-project breakdown"
    )
    args = parser.parse_args()
    
    # Handle status request with early exit (avoid loading heavy MCP dependencies)
    if args.status:
        from .status import get_status
        import json
        print(json.dumps(get_status()))
        return
    
    # Import is done here to make sure environment variables are loaded
    from .server import mcp
    
    # Run the server with the specified transport
    # Disable FastMCP banner to prevent JSON output interference
    mcp.run(transport=args.transport, show_banner=False)

if __name__ == "__main__":
    main()