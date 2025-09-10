# Release Notes - v3.2.3

## Summary
Version 3.2.3 is a critical fix for the CLI status command that was broken in global npm installations. This patch ensures the statusline integration works correctly again and improves virtual environment compatibility.

## Changes

### Bug Fixes
- **CLI Status Command Repair**: Fixed broken `claude-self-reflect status` command in global installations
  - **Root Cause**: Command was incorrectly calling `python -m src --status` instead of the dedicated status script
  - **Impact**: Status command failed completely, breaking statusline integration in Claude Code
  - **Solution**: Modified CLI to directly execute `status.py` script with proper path resolution
  - **Files Modified**: `installer/cli.js` - Updated status command execution logic

### Enhancements
- **Virtual Environment Compatibility**: Added fallback support for different venv naming conventions
  - Checks for both `venv/` and `.venv/` directories automatically
  - Prevents failures when projects use different virtual environment setups
  - Maintains backward compatibility with existing installations

### Technical Details
The status command architecture has been corrected:

```javascript
// Before (v3.2.2 and earlier - BROKEN):
spawn(venvPython, ['-m', 'src', '--status'], { cwd: mcpServerPath })

// After (v3.2.3):
spawn(venvPython, [statusScript], { cwd: mcpServerPath })
```

Where `statusScript` points to `mcp-server/src/status.py` directly.

## Backward Compatibility
- **Fully Compatible**: All existing functionality preserved
- **Configuration Unchanged**: No MCP server configuration changes required  
- **Installation Seamless**: Standard npm update process applies
- **Virtual Environment**: Enhanced compatibility, no migration needed

## Installation
```bash
npm install -g claude-self-reflect@3.2.3
```

## Verification
After updating, verify the status command works:
```bash
claude-self-reflect status
```
Should return JSON output with indexing statistics.

## Impact Assessment
- **CLI Users**: Status command now works reliably
- **Statusline Integration**: Claude Code statusline functionality restored
- **Global Installations**: Npm global installs now function correctly
- **Development Tools**: External tools can now query status properly

## Contributors
- **Claude Code**: Implementation of CLI status command fix
- **Community**: Feedback and testing of global npm installation issues

## Migration Notes
No migration required - this is an automatic fix:
1. Update package: `npm install -g claude-self-reflect@3.2.3`
2. Test status command: `claude-self-reflect status`
3. Statusline integration should work immediately

## Related Issues
- Resolves: CLI status command failure in global npm installations
- Improves: Virtual environment directory detection reliability  
- Enhances: Statusline integration compatibility with Claude Code
- Maintains: All existing search and reflection capabilities from v3.2.0-v3.2.2

## Next Steps
1. Update to v3.2.3 via npm
2. Verify status command functionality
3. No configuration changes required
4. Statusline integration should work seamlessly

This release maintains all the enhanced search modes and pagination support from v3.2.0 while ensuring the CLI tools function properly for all installation methods.