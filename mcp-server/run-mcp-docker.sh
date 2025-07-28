#!/bin/bash
# Run the MCP server in the Docker container with stdin attached
# Using python -u for unbuffered output
# Using server.py which supports local embeddings (not server_v2.py)
docker exec -i claude-reflection-mcp python -u -m src