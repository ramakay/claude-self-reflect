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
    )
    args = parser.parse_args()
    
    # Import is done here to make sure environment variables are loaded
    from .server import mcp
    
    # Run the server with the specified transport
    # Disable FastMCP banner to prevent JSON output interference
    mcp.run(transport=args.transport, show_banner=False)

if __name__ == "__main__":
    main()