use std::{
    io,
    process::{Child, Command, ExitStatus},
};

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
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "child PID does not fit process-group ID",
                )
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

#[cfg(test)]
mod tests {
    use std::{
        fs, thread,
        time::{Duration, Instant},
    };

    use super::*;

    const ROLE_ENV: &str = "MEDUSA_PROCESS_TREE_TEST_ROLE";
    const PID_FILE_ENV: &str = "MEDUSA_PROCESS_TREE_TEST_PID_FILE";

    #[test]
    fn process_tree_helper() {
        let Ok(role) = std::env::var(ROLE_ENV) else {
            return;
        };
        if role == "child" {
            let pid_file = std::env::var(PID_FILE_ENV).expect("pid file");
            let grandchild = Command::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "process_tree::tests::process_tree_helper",
                    "--nocapture",
                ])
                .env(ROLE_ENV, "grandchild")
                .spawn()
                .expect("grandchild");
            fs::write(pid_file, grandchild.id().to_string()).expect("record grandchild pid");
            thread::sleep(Duration::from_secs(30));
        } else if role == "grandchild" {
            thread::sleep(Duration::from_secs(30));
        }
    }

    #[test]
    fn terminate_kills_descendant_process_tree() {
        let directory = tempfile::tempdir().expect("directory");
        let pid_file = directory.path().join("grandchild.pid");
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "process_tree::tests::process_tree_helper",
                "--nocapture",
            ])
            .env(ROLE_ENV, "child")
            .env(PID_FILE_ENV, &pid_file);
        let mut tree = OwnedProcessTree::spawn(&mut command).expect("owned process tree");

        let deadline = Instant::now() + Duration::from_secs(5);
        while !pid_file.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let grandchild_pid = fs::read_to_string(&pid_file)
            .expect("grandchild pid")
            .trim()
            .parse::<u32>()
            .expect("numeric pid");
        assert!(process_alive(grandchild_pid));

        tree.terminate().expect("terminate tree");
        let _ = tree.wait().expect("wait tree");
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_alive(grandchild_pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!process_alive(grandchild_pid));
    }

    #[cfg(unix)]
    fn process_alive(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 performs existence/permission probing without sending a signal.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(windows)]
    fn process_alive(pid: u32) -> bool {
        crate::process_is_alive(pid)
    }

    #[cfg(not(any(unix, windows)))]
    fn process_alive(_pid: u32) -> bool {
        false
    }
}
