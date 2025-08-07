# ⚠️ Important Disclaimers

## Tool Operation
- **Resource Usage**: The import process can be CPU and memory intensive, especially during initial import of large conversation histories
- **Data Processing**: This tool reads and indexes your Claude conversation files. Ensure you have adequate disk space
- **No Warranty**: This software is provided "AS IS" under the MIT License, without warranty of any kind
- **Data Responsibility**: You are responsible for your conversation data and any API keys used

## Limitations
- **Not Official**: This is a community tool, not officially supported by Anthropic
- **Experimental Features**: Some features like memory decay are experimental and may change
- **Import Delays**: Large conversation histories may take significant time to import initially
- **Docker Dependency**: Requires Docker to be running, which uses system resources

## Best Practices
- **Backup Your Data**: Always maintain backups of important conversations
- **Monitor Resources**: Check Docker resource usage if you experience system slowdowns
- **Test First**: Try with a small subset of conversations before full import
- **Review Logs**: Check import logs if conversations seem missing

## Known Issues
- **Large Files**: Files over 100MB may require the streaming importer
- **Memory Spikes**: Initial import can temporarily use up to 2GB RAM
- **Search Latency**: Cross-project searches add ~100ms overhead
- **Windows Paths**: Native Windows requires special Docker path configuration

## Compatibility
- **Claude Desktop**: Fully supported
- **Claude Code**: Fully supported
- **Claude API**: Not applicable (this tool is for Desktop/Code only)
- **Operating Systems**: macOS, Linux, Windows (via WSL recommended)

By using this tool, you acknowledge these disclaimers and limitations.