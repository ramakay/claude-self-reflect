#!/bin/bash
# Run the MCP server in the Docker container with stdin attached
# Using python -u for unbuffered output
docker exec -i claude-reflection-mcp python -u -m src.server_v2