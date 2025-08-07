# Windows Configuration

## Recommended: Use WSL
For the best experience on Windows, we recommend using WSL (Windows Subsystem for Linux) which provides native Linux compatibility for Docker operations.

### Installing WSL

1. **Enable WSL** (requires Windows 10 version 2004+ or Windows 11):
```powershell
# Run as Administrator in PowerShell
wsl --install
```

2. **Install Ubuntu** (or your preferred Linux distribution):
```powershell
wsl --install -d Ubuntu
```

3. **Set up Docker Desktop** to use WSL 2 backend:
   - Open Docker Desktop Settings
   - Go to General
   - Enable "Use the WSL 2 based engine"
   - Go to Resources → WSL Integration
   - Enable integration with your installed distros

4. **Run claude-self-reflect inside WSL**:
```bash
# Open WSL terminal
wsl

# Install Node.js in WSL
curl -fsSL https://deb.nodesource.com/setup_lts.x | sudo -E bash -
sudo apt-get install -y nodejs

# Install claude-self-reflect
npm install -g claude-self-reflect
claude-self-reflect setup
```

## Alternative: Native Windows

If using Docker Desktop on native Windows (not recommended), you need to adjust paths for Docker compatibility:

### Path Configuration

1. **Update CONFIG_PATH in .env file**:
```bash
# Replace USERNAME with your Windows username
CONFIG_PATH=/c/Users/USERNAME/.claude-self-reflect/config
```

2. **Convert Windows paths to Docker format**:
   - `C:\Users\Username` → `/c/Users/Username`
   - `D:\Projects` → `/d/Projects`
   - Backslashes (`\`) → Forward slashes (`/`)

### Common Issues on Native Windows

1. **Path not found errors**:
   - Ensure paths use forward slashes
   - Use lowercase drive letters (`/c/` not `/C/`)
   - No spaces in paths (use quotes if needed)

2. **Permission errors**:
   - Run Docker Desktop as Administrator
   - Ensure your user has full permissions to the config directory

3. **Line ending issues**:
   - Configure Git to use LF endings: `git config --global core.autocrlf false`
   - Use an editor that preserves Unix line endings

### Docker Desktop Settings for Windows

Recommended Docker Desktop configuration:
- **Memory**: At least 4GB allocated to Docker
- **CPU**: At least 2 CPUs
- **Disk**: At least 20GB for Docker images and volumes

## Troubleshooting Windows Issues

### WSL-Specific Issues

1. **WSL not starting**:
```powershell
# Restart WSL
wsl --shutdown
wsl
```

2. **Docker not accessible from WSL**:
   - Ensure Docker Desktop WSL integration is enabled
   - Restart Docker Desktop
   - Check WSL version: `wsl -l -v` (should show version 2)

### Native Windows Issues

1. **"Cannot connect to Docker daemon"**:
   - Ensure Docker Desktop is running
   - Check if Docker service is started
   - Try: `docker version` to verify connection

2. **"Mount denied" errors**:
   - Check Docker Desktop → Settings → Resources → File Sharing
   - Add your project directory to shared folders

3. **Slow performance**:
   - WSL 2 is significantly faster than native Windows for Docker
   - Consider moving to WSL if performance is critical

## Best Practices for Windows

1. **Use WSL 2** - It's faster and more compatible
2. **Keep paths short** - Avoid deep nesting to prevent path length issues
3. **Use Git Bash or WSL terminal** - Better Unix command compatibility
4. **Regular updates** - Keep Docker Desktop and WSL updated
5. **Antivirus exclusions** - Add Docker and project directories to exclusions for better performance