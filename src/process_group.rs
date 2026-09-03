//! Cross-platform process group management
//!
//! On Unix: uses process groups (setpgid/killpg)
//! On Windows: uses JobObjects to manage process trees

use anyhow::Result;
use std::process::{Child, Command};

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
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::*;

    pub fn spawn_in_group(cmd: &mut Command) -> Result<Child> {
        // Spawn child first
        let child = cmd.spawn()?;

        // Create job object
        let job = unsafe { CreateJobObjectW(None, None)? };

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
        // but the job persists and will kill all processes when the main process exits

        Ok(child)
    }

    pub fn kill_group(child: &mut Child) -> Result<()> {
        // On Windows, killing the child in a job kills the whole job
        child.kill()?;
        Ok(())
    }
}
