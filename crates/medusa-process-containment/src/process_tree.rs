use std::{io, process::{Child, Command, ExitStatus}};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
use crate::WindowsJob;

#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// Owns a spawned process and every descendant that remains in its platform containment group.
///
/// Unix children are placed in a dedicated process group before exec. Windows children are created
/// suspended, assigned to a kill-on-close Job Object, and only then resumed. This removes the race
/// where user code could spawn descendants before Medusa established ownership.
#[derive(Debug)]
pub struct OwnedProcessTree {
    child: Child,
    #[cfg(unix)]
    process_group: i32,
    #[cfg(windows)]
    job: WindowsJob,
}

impl OwnedProcessTree {
    /// Spawns a command under process-tree ownership before any child user code can escape it.
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            command.process_group(0);
            let child = command.spawn()?;
            let process_group = i32::try_from(child.id()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "child PID does not fit process-group ID")
            })?;
            Ok(Self {
                child,
                process_group,
            })
        }
        #[cfg(windows)]
        {
            command.creation_flags(CREATE_SUSPENDED);
            let mut child = command.spawn()?;
            let job = match WindowsJob::assign(&child) {
                Ok(job) => job,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            };
            if let Err(error) = job.resume(&child) {
                let _ = job.terminate();
                let _ = child.wait();
                return Err(error);
            }
            Ok(Self { child, job })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                child: command.spawn()?,
            })
        }
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    /// Force-terminates the owned process tree. Missing/already-exited groups are treated as success.
    pub fn terminate(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            // SAFETY: the child was created in a dedicated process group whose ID is the child's PID.
            // Passing the negated group ID to kill targets that group only. ESRCH means it already exited.
            let result = unsafe { libc::kill(-self.process_group, libc::SIGKILL) };
            if result == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }
        #[cfg(windows)]
        {
            self.job.terminate()
        }
        #[cfg(not(any(unix, windows)))]
        {
            self.child.kill()
        }
    }
}

impl Drop for OwnedProcessTree {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.terminate();
            let _ = self.child.wait();
        }
    }
}
