# Windows Support Implementation Plan

**Status**: Planning
**Target**: Full Windows 10/11 compatibility for herdr-agent-quota
**Estimated Effort**: 3-5 days

## Overview

Add complete Windows support to enable the plugin to run on Windows 10/11 alongside the existing macOS/Linux support.

## Critical Blockers (Must Fix)

### 1. IPC: Unix Domain Socket → Cross-platform Alternative

**File**: `src/herdr.rs:227`

**Current**:
```rust
use std::os::unix::net::UnixStream;
```

**Strategy**: 
- **Option A** (Recommended): Use `interprocess` crate for cross-platform named pipes/Unix sockets
- **Option B**: Conditional compilation with `std::os::windows::io` for Windows named pipes
- **Option C**: Fall back to TCP localhost sockets (less secure but simplest)

**Implementation**:
1. Add dependency: `interprocess = "1.2"` (supports both Unix sockets and Windows named pipes)
2. Replace `UnixStream` with `interprocess::local_socket::LocalSocketStream`
3. Test connection to Herdr on both platforms

**Priority**: 🔴 CRITICAL (blocks all Herdr communication)

---

### 2. Process Group Management → Windows JobObjects

**Files**: 
- `src/process.rs` (lines 30-100)
- `src/providers/codex.rs` (lines 154-159, 197-199)

**Current**:
```rust
#[cfg(unix)]
use libc::{setpgid, killpg, setsid, SIGKILL};
```

**Strategy**: 
- Keep Unix implementation as-is
- Add Windows JobObjects for process tree management
- Abstract behind a cross-platform `ProcessGroup` trait

**Implementation**:
1. Create `src/process_group.rs` module with trait:
   ```rust
   pub trait ProcessGroup {
       fn spawn_in_group(cmd: &mut Command) -> Result<Child>;
       fn kill_group(child: &Child) -> Result<()>;
   }
   ```
2. Unix implementation using existing `libc` code
3. Windows implementation using `windows` crate with JobObjects:
   ```rust
   use windows::Win32::System::JobObjects::*;
   ```
4. Replace direct `libc` calls with trait usage

**Dependencies to add**:
```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.52", features = ["Win32_System_JobObjects", "Win32_Foundation"] }
```

**Priority**: 🔴 CRITICAL (prevents proper process cleanup)

---

### 3. Shell Command Execution → Platform Detection

**File**: `src/process.rs`

**Current**:
```rust
Command::new("sh")
    .arg("-c")
    .arg(command)
```

**Strategy**: Detect platform and use appropriate shell

**Implementation**:
```rust
#[cfg(unix)]
fn get_shell_command(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script);
    cmd
}

#[cfg(windows)]
fn get_shell_command(script: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(script);
    cmd
}
```

**Priority**: 🔴 CRITICAL (blocks any shell command execution)

---

## Major Issues (Important)

### 4. HOME Environment Variable → Cross-platform Home Resolution

**Files**: 
- `src/opencode.rs:93-106`
- `src/configure/grok.rs:58`
- `src/configure/herdr.rs:201`
- `src/providers/grok.rs:66`
- `src/providers/codex.rs:676`

**Current**:
```rust
std::env::var("HOME")?
```

**Strategy**: Use `directories` crate consistently (already a dependency)

**Implementation**:
```rust
use directories::BaseDirs;

fn get_home_dir() -> Result<PathBuf> {
    BaseDirs::new()
        .ok_or_else(|| anyhow!("Cannot determine home directory"))?
        .home_dir()
        .to_path_buf()
}
```

**Priority**: 🟡 MAJOR (causes runtime failures on Windows)

---

### 5. Unix File Permissions → Conditional Compilation

**File**: `src/providers/omp.rs:594`

**Current**:
```rust
use std::os::unix::fs::PermissionsExt;
```

**Strategy**: Conditional compilation, skip on Windows or use Windows ACLs

**Implementation**:
```rust
#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(windows)]
fn set_executable(_path: &Path) -> Result<()> {
    // Windows doesn't use Unix-style executable bits
    Ok(())
}
```

**Priority**: 🟡 MAJOR (minor feature, but causes compilation failure)

---

## Build System

### 6. Install Script → PowerShell Port

**Files**: `install.sh`, `uninstall.sh`

**Strategy**: Create parallel `install.ps1` and `uninstall.ps1` scripts

**Implementation Plan**:
1. Create `install.ps1` with equivalent logic:
   - Check for `cargo` and `herdr` in PATH
   - Build with `cargo build --release --locked`
   - Call `herdr plugin link`, `enable`, `disable`, `unlink`
   - Write config files to plugin directory
2. Handle command-line arguments with PowerShell param blocks
3. Add error handling with try/catch

**Key PowerShell Equivalents**:
- `command -v` → `Get-Command -ErrorAction SilentlyContinue`
- `mkdir -p` → `New-Item -ItemType Directory -Force`
- `sed/grep/tr` → `-replace` operator and `Select-String`
- `trap` → `try {} finally {}`

**Priority**: 🟡 MAJOR (users need to install the plugin)

---

## Testing Strategy

### Phase 1: Compilation
- [ ] Add Windows dependencies to `Cargo.toml`
- [ ] Add conditional compilation attributes
- [ ] Ensure `cargo build --release` succeeds on Windows

### Phase 2: Unit Tests
- [ ] Run existing tests: `cargo test --all-targets`
- [ ] Add Windows-specific test cases for process management
- [ ] Mock Herdr IPC for testing

### Phase 3: Integration Tests
- [ ] Test install.ps1 script on clean Windows machine
- [ ] Test Herdr plugin registration
- [ ] Test IPC communication with actual Herdr process
- [ ] Test process group cleanup (Codex server termination)

### Phase 4: Cross-platform CI
- [ ] Update `.github/workflows/ci.yml` to test on `windows-latest`
- [ ] Ensure tests pass on Ubuntu, macOS, and Windows

---

## Implementation Phases

### Phase 1: Foundation (Day 1)
1. ✅ Analysis complete
2. Add Windows dependencies to Cargo.toml
3. Create `src/process_group.rs` with trait abstraction
4. Fix HOME variable usage with `directories` crate
5. Ensure compilation succeeds

**Goal**: `cargo build --release` works on Windows

---

### Phase 2: Core Functionality (Day 2)
1. Implement Windows JobObjects for process management
2. Add shell command platform detection
3. Fix file permission handling
4. Run and fix unit tests

**Goal**: `cargo test` passes on Windows

---

### Phase 3: IPC Integration (Day 3)
1. Add `interprocess` crate or Windows named pipe implementation
2. Update `src/herdr.rs` IPC connection code
3. Test with actual Herdr process on Windows
4. Handle edge cases and errors

**Goal**: Plugin can communicate with Herdr

---

### Phase 4: Installation & Polish (Day 4)
1. Write `install.ps1` script
2. Write `uninstall.ps1` script
3. Update README with Windows installation instructions
4. Test full installation flow

**Goal**: Users can install and use the plugin

---

### Phase 5: CI & Documentation (Day 5)
1. Add Windows runner to GitHub Actions
2. Update documentation
3. Add troubleshooting section for Windows
4. Create PR to upstream

**Goal**: Ready for production use

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Herdr uses Unix-only IPC on Windows | Medium | High | Test Herdr's Windows implementation first; may need Herdr changes |
| JobObjects don't behave like process groups | Low | Medium | Extensive testing; fallback to simple Child::kill() |
| Path handling edge cases | Medium | Low | Use `std::path::PathBuf` consistently |
| CI setup complexity | Low | Low | GitHub Actions has good Windows support |

---

## Success Criteria

- [ ] `cargo build --release --locked` succeeds on Windows 10/11
- [ ] `cargo test --all-targets --locked` passes on Windows
- [ ] `install.ps1` successfully installs plugin
- [ ] Plugin displays quota data in Herdr on Windows
- [ ] Process cleanup works (no orphaned processes)
- [ ] CI passes on Ubuntu, macOS, and Windows
- [ ] README updated with Windows instructions

---

## Next Steps

**Immediate actions**:
1. Set up Rust toolchain on your Windows machine
2. Start with Phase 1: Foundation work
3. Test compilation and fix basic compatibility issues

**Ready to proceed?** I can help you:
- Install Rust on Windows
- Start implementing Phase 1 changes
- Set up the development environment
