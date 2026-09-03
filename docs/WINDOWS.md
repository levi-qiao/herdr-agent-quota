# Windows Support for herdr-agent-quota

This document describes the Windows support implementation for the herdr-agent-quota plugin.

## Status

✅ **Windows support is now available** (as of 2026-09-03)

The plugin has been updated to work on Windows 10 1803+ alongside macOS and Linux.

## Requirements

### System Requirements
- **Windows 10 version 1803 or later** (for Unix domain socket support)
- Windows 11 (recommended)

### Build Requirements
- **Rust 1.95.0+** with MSVC toolchain
  - Install from: https://rustup.rs/
  - The installer will set up the MSVC toolchain automatically
- **Herdr** installed and in PATH
  - Download from: https://herdr.dev

## Installation

### Quick Install

```powershell
# Clone or download the repository
git clone https://github.com/YOUR-USERNAME/herdr-agent-quota.git
cd herdr-agent-quota

# Run the Windows installer
.\install.ps1
```

### Manual Installation

```powershell
# 1. Build the plugin
cargo build --release --locked

# 2. Link the plugin
herdr plugin link .

# 3. Enable the plugin
herdr plugin enable herdr-agent-quota

# 4. Configure agents (optional)
$configDir = herdr plugin config-dir herdr-agent-quota
New-Item -ItemType Directory -Path $configDir -Force
Set-Content "$configDir\agents.json" -Value @'
{
  "herdr-agent-quota": {
    "agents": ["codex", "grok", "agy", "opencode", "pi", "omp"]
  }
}
'@

# 5. Trigger initial refresh
herdr action invoke herdr-agent-quota refresh
```

## Uninstallation

```powershell
.\install.ps1 -Uninstall
```

Or manually:
```powershell
herdr plugin disable herdr-agent-quota
herdr plugin unlink herdr-agent-quota
```

## Implementation Details

### Cross-Platform Compatibility

The following changes were made to support Windows:

1. **Home Directory Resolution** (`src/lib.rs`)
   - Added `home_dir()` helper using the `directories` crate
   - Works with Windows `USERPROFILE`, macOS/Linux `HOME`

2. **Process Group Management** (`src/process_group.rs`)
   - Unix: Uses `libc::setpgid()` and `libc::killpg()`
   - Windows: Uses JobObjects API for process tree management
   - Ensures child processes are properly terminated

3. **Shell Command Execution** (`src/process.rs`)
   - Unix: Uses `sh -c`
   - Windows: Uses `cmd /C`
   - Platform detection at runtime

4. **Unix Domain Sockets** (`src/herdr.rs`)
   - Windows 10 1803+ supports Unix domain sockets
   - No changes needed - `std::os::unix::net::UnixStream` works on Windows

5. **Installation Script**
   - Created `install.ps1` (PowerShell) alongside `install.sh` (Bash)
   - Feature parity with the Unix installer

### Toolchain Configuration

The project uses the MSVC toolchain on Windows:

```toml
# rust-toolchain.toml
[toolchain]
channel = "1.95.0-x86_64-pc-windows-msvc"
profile = "minimal"
components = ["rustfmt", "clippy"]
```

### Windows-Specific Dependencies

```toml
# Cargo.toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.52", features = [
    "Win32_System_JobObjects",
    "Win32_Foundation",
    "Win32_System_Threading"
] }
```

## Platform Differences

### Process Management

**Unix**: Uses process groups (PGID) to manage child process trees.

**Windows**: Uses JobObjects to manage process trees. When a parent process terminates, all child processes in the JobObject are automatically terminated.

### File Paths

All file path operations use cross-platform abstractions:
- `PathBuf::join()` for path construction
- `directories` crate for user directories
- No hardcoded path separators

### Environment Variables

| Unix | Windows | Usage |
|------|---------|-------|
| `HOME` | `USERPROFILE` | User home directory (abstracted via `directories` crate) |
| Same | Same | All other env vars (`HERDR_SOCKET_PATH`, `CODEX_HOME`, etc.) |

## Known Limitations

1. **Unix Domain Socket Support**
   - Requires Windows 10 version 1803 or later
   - Older Windows versions are not supported

2. **Process Tree Management**
   - JobObjects provide similar but not identical semantics to Unix process groups
   - In rare cases, zombie processes may persist (same as Unix `sh -c` limitation)

3. **Shell Scripts**
   - The `install.sh` script requires Git Bash or WSL on Windows
   - Use `install.ps1` for native Windows experience

## Testing

Run the test suite:

```powershell
cargo test --locked
```

Key tests to verify Windows support:
- `test_home_dir()` - Home directory resolution
- `test_process_group_termination()` - JobObjects functionality
- `test_shell_command()` - Platform-specific shell detection

## Troubleshooting

### "Unix domain sockets not supported"

**Cause**: Windows version older than 1803

**Solution**: Upgrade to Windows 10 1803+ or Windows 11

### "failed to find tool gcc.exe"

**Cause**: Using GNU toolchain instead of MSVC

**Solution**: 
```powershell
rustup default stable-x86_64-pc-windows-msvc
cargo clean
cargo build --release
```

### "HERDR_SOCKET_PATH not set"

**Cause**: Running outside of a Herdr session

**Solution**: Start Herdr and run commands within a Herdr terminal

### Plugin doesn't appear in statusline

**Solution**:
```powershell
# Check if plugin is enabled
herdr plugin list

# Re-enable if needed
herdr plugin enable herdr-agent-quota

# Trigger manual refresh
herdr action invoke herdr-agent-quota refresh
```

## Contributing

When making changes that affect Windows support:

1. Test on both Windows and Unix platforms
2. Use `#[cfg(windows)]` and `#[cfg(unix)]` for platform-specific code
3. Update this README with any new platform differences
4. Add tests for new platform-specific functionality

## License

Same as the main project.

## See Also

- [Main README](../README.md)
- [Phase 1 Implementation Progress](phase-1-progress.md)
- [Windows Support Implementation Plan](windows-support-implementation.md)
