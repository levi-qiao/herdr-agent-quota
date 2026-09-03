# Quick Test Guide - After Build Tools Installation

## Step 1: Verify MSVC Tools Installed

```bash
# Check for MSVC link.exe (should be first in list, before Git's link.exe)
where link.exe

# Expected output:
# C:\BuildTools\VC\Tools\MSVC\<version>\bin\Hostx64\x64\link.exe
# C:\Program Files\Git\usr\bin\link.exe

# If Git's link.exe is still first, we may need to adjust PATH
```

## Step 2: Clean and Rebuild

```bash
cd D:/work/project/herdr-agent-quota/.claude/worktrees/fervent-hertz-92be3d

# Clean previous build artifacts
cargo clean

# Build in release mode
cargo build --release --locked
```

**Expected**: Should compile successfully with MSVC toolchain

## Step 3: Run Tests

```bash
# Run all tests
cargo test --locked

# Run specific test if needed
cargo test --locked test_home_dir
```

## Step 4: Install Plugin

```bash
# Run the PowerShell installer
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

**Expected output**:
```
Building plugin...
   Compiling herdr-agent-quota v1.3.0
    Finished release [optimized] target(s) in X.XXs
Installing plugin...
Backed up existing agents.json
Created default agents.json with all providers
Refreshing quota data...
✓ Initial refresh completed

✓ Installation complete!

The quota plugin is now active in your Herdr statusline.
```

## Step 5: Verify Plugin

```bash
# List installed plugins
herdr plugin list

# Expected: herdr-agent-quota should be listed and enabled

# Check plugin config
herdr plugin config-dir herdr-agent-quota

# Trigger manual refresh
herdr action invoke herdr-agent-quota refresh

# View logs
herdr plugin log list herdr-agent-quota
```

## Step 6: Visual Verification

1. Open Herdr
2. Look at the statusline
3. Should see quota information for enabled agents
4. Format: `🤖 Codex: 50% | Grok: 30%` (example)

## Troubleshooting

### If build fails with "cannot find -lwindows"

```bash
# Try explicit target
cargo build --release --target x86_64-pc-windows-msvc
```

### If "Unix domain sockets not supported" error

- Your Windows version is too old (need Windows 10 1803+)
- Check version:
  ```bash
  winver
  ```

### If plugin doesn't show in statusline

```bash
# Re-enable plugin
herdr plugin enable herdr-agent-quota

# Check Herdr is running
herdr status

# Check for errors in logs
herdr plugin log list --json herdr-agent-quota
```

### If PATH still shows Git's link.exe first

Option 1: Use Developer Command Prompt
```bash
# Open "x64 Native Tools Command Prompt for VS 2022"
# Then run cargo commands there
```

Option 2: Temporarily adjust PATH
```bash
export PATH="/c/BuildTools/VC/Tools/MSVC/14.41.34120/bin/Hostx64/x64:$PATH"
cargo build --release
```

## Known Working Configuration

- Windows 11 Pro 10.0.26200
- Rust 1.95.0
- MSVC toolchain: x86_64-pc-windows-msvc
- Visual Studio Build Tools 2022 with C++ workload
- Herdr installed and in PATH

## Quick Uninstall (if needed)

```bash
powershell -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall
```

## Next Steps After Successful Build

1. Test with real Herdr session
2. Verify quota data updates
3. Test with different agents (codex, grok, omp, etc.)
4. Monitor for any runtime errors
5. If all works: Move to Phase 2 (process management for full functionality)

## Phase 2 Preview

Once basic installation works, Phase 2 will add:
- Full process group management (JobObjects on Windows)
- Shell detection (cmd vs PowerShell)
- Robust child process termination
- Complete parity with Unix version

But Phase 1 should give you a working plugin with quota display!
