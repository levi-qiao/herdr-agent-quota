# Phase 1: Foundation - Detailed Checklist

## Prerequisites

### Install Rust on Windows
```powershell
# Download and run rustup-init.exe from https://rustup.rs/
# Or use winget:
winget install Rustlang.Rustup

# Verify installation
rustc --version
cargo --version
```

**Expected**: Rust 1.95.0 or higher (project requires 1.95+)

---

## Task 1: Update Cargo.toml Dependencies

### Add Windows-specific dependencies

**File**: `Cargo.toml`

**Changes**:
```toml
# Add after line 37 (existing [target.'cfg(unix)'.dependencies])

[target.'cfg(windows)'.dependencies]
windows = { version = "0.52", features = [
    "Win32_System_JobObjects",
    "Win32_Foundation",
    "Win32_System_Threading"
] }
```

**Optional**: Add cross-platform IPC library
```toml
# In [dependencies] section (around line 16)
interprocess = "2.2"  # For cross-platform named pipes/Unix sockets
```

---

## Task 2: Create Process Group Abstraction

### Create new module: `src/process_group.rs`

```rust
//! Cross-platform process group management
//!
//! On Unix: uses process groups (setpgid/killpg)
//! On Windows: uses JobObjects to manage process trees

use anyhow::Result;
use std::process::{Child, Command};

#[cfg(unix)]
mod unix_impl;
#[cfg(windows)]
mod windows_impl;

pub trait ProcessGroupExt {
    /// Spawn a command in a new process group/job
    fn spawn_in_group(&mut self) -> Result<Child>;
}

impl ProcessGroupExt for Command {
    fn spawn_in_group(&mut self) -> Result<Child> {
        #[cfg(unix)]
        return unix_impl::spawn_in_group(self);
        
        #[cfg(windows)]
        return windows_impl::spawn_in_group(self);
    }
}

pub fn kill_process_group(child: &mut Child) -> Result<()> {
    #[cfg(unix)]
    return unix_impl::kill_group(child);
    
    #[cfg(windows)]
    return windows_impl::kill_group(child);
}

#[cfg(unix)]
mod unix_impl {
    use super::*;
    use std::os::unix::process::CommandExt;
    
    pub fn spawn_in_group(cmd: &mut Command) -> Result<Child> {
        unsafe {
            cmd.pre_exec(|| {
                // Create new process group
                libc::setpgid(0, 0);
                Ok(())
            });
        }
        Ok(cmd.spawn()?)
    }
    
    pub fn kill_group(child: &mut Child) -> Result<()> {
        let pid = child.id() as i32;
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
        Ok(())
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use windows::Win32::Foundation::*;
    use windows::Win32::System::JobObjects::*;
    use windows::Win32::System::Threading::*;
    use std::os::windows::io::AsRawHandle;
    
    pub fn spawn_in_group(cmd: &mut Command) -> Result<Child> {
        // Spawn child first
        let mut child = cmd.spawn()?;
        
        // Create job object
        let job = unsafe {
            CreateJobObjectW(None, None)?
        };
        
        // Configure job to kill all processes when handle closes
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        
        unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
            
            // Assign process to job
            let process_handle = child.as_raw_handle() as isize;
            AssignProcessToJobObject(job, HANDLE(process_handle))?;
        }
        
        // Job handle will be dropped when this function returns,
        // but the job persists until the child process exits
        // We store it in the Child's metadata (need custom wrapper)
        
        Ok(child)
    }
    
    pub fn kill_group(child: &mut Child) -> Result<()> {
        // On Windows, killing the child in a job kills the whole job
        child.kill()?;
        Ok(())
    }
}
```

**Status**: 🔴 TODO

---

## Task 3: Fix HOME Environment Variable Usage

### Update all files using `std::env::var("HOME")`

**Helper function** - Add to `src/lib.rs`:

```rust
use directories::BaseDirs;
use anyhow::{Result, Context};
use std::path::PathBuf;

/// Get the user's home directory in a cross-platform way
pub fn home_dir() -> Result<PathBuf> {
    BaseDirs::new()
        .context("Cannot determine home directory")?
        .home_dir()
        .to_path_buf()
        .pipe(Ok)
}
```

### Files to update:

#### 1. `src/opencode.rs:93-106`
**Before**:
```rust
let home = std::env::var("HOME")?;
```

**After**:
```rust
let home = crate::home_dir()?;
```

#### 2. `src/configure/grok.rs:58`
Same replacement

#### 3. `src/configure/herdr.rs:201`
Same replacement

#### 4. `src/providers/grok.rs:66`
Same replacement

#### 5. `src/providers/codex.rs:676`
Same replacement

**Status**: 🔴 TODO

---

## Task 4: Shell Command Platform Detection

### Update `src/process.rs`

**Add helper functions** (around line 20):

```rust
#[cfg(unix)]
pub fn shell_command(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script);
    cmd
}

#[cfg(windows)]
pub fn shell_command(script: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(script);
    cmd
}
```

**Find and replace** all instances of:
```rust
Command::new("sh")
    .arg("-c")
```

With:
```rust
shell_command(
```

**Status**: 🔴 TODO

---

## Task 5: Fix Unix-only File Permissions

### Update `src/providers/omp.rs:594`

**Before**:
```rust
use std::os::unix::fs::PermissionsExt;
// ... later ...
perms.set_mode(0o755);
```

**After**:
```rust
#[cfg(unix)]
fn set_executable_permission(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(windows)]
fn set_executable_permission(_path: &Path) -> Result<()> {
    // Windows doesn't use Unix executable bits
    // .exe files are executable by extension
    Ok(())
}
```

Then replace the permission-setting code with:
```rust
set_executable_permission(&path)?;
```

**Status**: 🔴 TODO

---

## Task 6: Test Compilation

### Build the project

```powershell
cd D:\work\project\herdr-agent-quota\.claude\worktrees\fervent-hertz-92be3d

# Clean build
cargo clean

# Check for errors (faster than full build)
cargo check --all-targets

# Full release build
cargo build --release --locked

# Run tests
cargo test --locked
```

**Expected issues**:
- Compilation errors → fix with conditional compilation
- Missing Windows APIs → add to Cargo.toml features
- Path-related test failures → update tests for Windows paths

**Status**: 🔴 TODO

---

## Task 7: Update Documentation

### Update `README.md`

**Change line 24**:

**Before**:
```markdown
Requires Herdr 0.8.0+, Rust 1.95+, macOS or Linux, and at least one supported agent CLI.
```

**After**:
```markdown
Requires Herdr 0.8.0+, Rust 1.95+, Windows 10+/macOS/Linux, and at least one supported agent CLI.
```

**Add Windows installation section** (after line 38):

```markdown
### Windows

```powershell
git clone https://github.com/levi-qiao/herdr-agent-quota.git
cd herdr-agent-quota
.\install.ps1
```

Restart already-running agent panes once.
```

**Status**: 🔴 TODO

---

## Validation Checklist

- [ ] Rust toolchain installed (rustc 1.95.0+)
- [ ] `Cargo.toml` updated with Windows dependencies
- [ ] `src/process_group.rs` created with trait abstraction
- [ ] All `HOME` references replaced with `home_dir()`
- [ ] Shell command detection added
- [ ] File permission code made conditional
- [ ] `cargo check` passes without errors
- [ ] `cargo build --release --locked` succeeds
- [ ] `cargo test --locked` passes (or failures documented)
- [ ] README.md updated with Windows support mention

---

## Next: Phase 2

Once Phase 1 is complete and compilation succeeds, move to Phase 2:
- Implement Windows JobObjects properly
- Handle IPC communication
- Fix remaining runtime issues
