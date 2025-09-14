# Claude Code Statusline Integration

## Overview
Claude Self-Reflect now includes automatic Claude Code statusline integration that displays real-time import status and code quality metrics in a compact format.

## Display Format
The statusline shows: `[import%][quality_icon:grade]`

Examples:
- `[100%][🟢:A+]` - Fully indexed with excellent code quality
- `[85%][🟡:B]` - 85% indexed with good code quality
- `[50%][🔴:D]` - Half indexed with poor code quality

## Automatic Installation
When you install claude-self-reflect via npm, the statusline integration is configured automatically:

```bash
npm install -g claude-self-reflect
```

The postinstall script will:
1. Install the global `csr-status` command
2. Patch your Claude Code statusline wrapper
3. Validate the integration

## Manual Installation
If automatic installation fails, you can manually configure:

```bash
# Install the package
npm install -g claude-self-reflect

# Run statusline setup
claude-self-reflect statusline
```

## Components

### csr-status Command
A lightweight Python script that:
- Shows import percentage from indexed conversations
- Displays code quality grade based on AST-GREP analysis
- Uses compact format by default
- Caches data for performance

### Quality Tracking
The system tracks code quality using:
- **AST-GREP patterns**: 77+ patterns for Python, TypeScript, JavaScript
- **Real-time analysis**: Analyzes files edited in current session
- **Quality grades**: A+ to F based on good/bad pattern matches

### Performance
- Import status cached for 60 seconds
- Quality data cached for 5 minutes
- AST patterns updated every 24 hours
- Minimal overhead (<100ms for status check)

## Configuration Options

### Command Line Options
```bash
csr-status              # Default compact format: [100%][🟢:A+]
csr-status --import     # Import status only: ✅ CSR: 100% indexed
csr-status --health     # Session health only: 🟢 Code: A+
csr-status --compact    # Explicit compact format
```

### Restore Original Statusline
If you want to remove the integration:

```bash
claude-self-reflect statusline --restore
```

## Troubleshooting

### Statusline Not Appearing
1. Restart Claude Code after installation
2. Check if `csr-status` command works:
   ```bash
   csr-status
   ```
3. Verify statusline wrapper is patched:
   ```bash
   grep "CSR" ~/.claude/statusline-wrapper.sh
   ```

### Quality Data Missing
1. Run session quality tracker:
   ```bash
   cd claude-self-reflect
   source venv/bin/activate
   python scripts/session_quality_tracker.py
   ```
2. Check cache file exists:
   ```bash
   ls ~/.claude-self-reflect/session_quality.json
   ```

### Import Percentage Wrong
1. Run status check:
   ```bash
   claude-self-reflect status
   ```
2. Force re-import if needed:
   ```bash
   cd claude-self-reflect
   source venv/bin/activate
   python scripts/import-conversations-unified.py
   ```

## Technical Details

### File Locations
- **Status script**: `scripts/csr-status`
- **Global symlink**: `/usr/local/bin/csr-status`
- **Quality cache**: `~/.claude-self-reflect/session_quality.json`
- **Import state**: `~/.claude-self-reflect/config/imported-files.json`

### Integration Points
- **Claude Code**: `~/.claude/statusline-wrapper.sh`
- **NPM postinstall**: `installer/postinstall.js`
- **Setup wizard**: `installer/statusline-setup.js`

### Quality Metrics
- **Good patterns**: Docstrings, type hints, error handling
- **Bad patterns**: Print statements, bare exceptions, console.log
- **Scoring**: Weighted by pattern severity and frequency
- **Grades**: A+ (90%+), A (80%+), B (70%+), C (60%+), D (50%+), F (<50%)

## Future Enhancements
- [ ] Project-specific quality thresholds
- [ ] Custom pattern definitions
- [ ] Historical quality trends
- [ ] Integration with CI/CD pipelines