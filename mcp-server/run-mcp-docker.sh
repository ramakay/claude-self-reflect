#!/bin/bash
# Run the MCP server in the Docker container with stdin attached
# Using python -u for unbuffered output
# Using the main module which properly supports local embeddings
docker exec -i claude-reflection-mcp python -u -m src
