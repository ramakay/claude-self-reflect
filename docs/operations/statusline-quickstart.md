# Claude Code Statusline - Quick Start

## Installation Complete! ✅

The `csr-status` command is now globally available at `/usr/local/bin/csr-status`

## How to Add to Claude Code Statusline

1. Open Claude Code settings
2. Find the statusline configuration section
3. Add this command:
   ```
   csr-status
   ```

That's it! The statusline will now show:

## What You'll See

The statusline cycles between two views every 5 seconds:

### Import Status
- `✅ CSR: 100% indexed` - All conversations searchable
- `🔄 CSR: 67% indexed` - Import in progress
- `⏳ CSR: 10% indexed` - Just started importing

### Session Health (when available)
- `🟢 Code: A+` - Excellent quality
- `🟡 Code: B (5)` - Good with 5 issues
- `🔴 Code: D (15)` - Poor quality, 15 issues

## Testing the Command

```bash
# Test default (cycles automatically)
csr-status

# Force import view
csr-status --import

# Force health view
csr-status --health
```

## Troubleshooting

If the command isn't found in Claude Code:
1. Make sure `/usr/local/bin` is in your PATH
2. Try using the full path: `/usr/local/bin/csr-status`
3. Or use the direct script: `/Users/YOUR_USERNAME/projects/claude-self-reflect/scripts/csr-status`

## Features

- **No dependencies**: Standalone Python script
- **Fast**: <50ms response time
- **Smart cycling**: Alternates between metrics
- **Graceful fallback**: Shows import status if no session data
- **Compact display**: Designed for statusline space constraints