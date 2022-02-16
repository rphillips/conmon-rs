//! Child process reaping and management.

use crate::child::Child;
use anyhow::{bail, format_err, Result};
use getset::Getters;
use log::{debug, error};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::Pid;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output, Stdio};
use std::{collections::HashMap, fs::File, io::Write, sync::Arc, sync::Mutex};

impl ChildReaper {
    pub async fn create_child<P, I, S>(&self, cmd: P, args: I) -> Result<ExitStatus>
    where
        P: AsRef<std::ffi::OsStr>,
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut cmd = tokio::process::Command::new(cmd);
        cmd.args(args);
        cmd.spawn()
            .map_err(|e| format_err!("spawn child process: {}", e))?
            .wait()
            .await
            .map_err(|e| format_err!("wait for child process: {}", e))
    }

    pub async fn exec_sync(
        &self,
        command: &Path,
        args: Vec<String>,
        timeout: i32,
    ) -> Result<Output> {
        let child = tokio::process::Command::new(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .args(args)
            .spawn()
            .map_err(|e| format_err!("spawn child process: {}", e))?;

        let delay = tokio::time::sleep(tokio::time::Duration::from_secs(timeout as u64));
        tokio::pin!(delay);
        tokio::select! {
            _ = &mut delay => {
                debug!("timeout");
                return Err(format_err!("timeout error"))
            },
            status = child.wait_with_output() => {
                 let output = status.expect("status expected");
                 Ok(output)
            }
        }
    }

    pub fn watch_grandchild(&self, child: &Child) -> Result<()> {
        let locked_grandchildren = Arc::clone(&self.grandchildren);
        let mut map = locked_grandchildren
            .lock()
            .map_err(|e| format_err!("lock grandchildren: {}", e))?;
        let reapable_grandchild = ReapableChild::from_child(child);
        reapable_grandchild.watch();
        if map.insert(child.pid, true).is_some() {
            bail!("Repeat PID {} for container found", child.pid);
        }

        Ok(())
    }

    /*
    fn forget_grandchild(
        locked_map: Arc<Mutex<HashMap<i32, ReapableChild>>>,
        grandchild_pid: Pid,
    ) -> Result<()> {
        let mut map = locked_map
            .lock()
            .map_err(|e| format_err!("lock grandchildren: {}", e))?;
        map.remove(&(i32::from(grandchild_pid)));
        Ok(())
    }
    */

    pub fn kill_grandchildren(&self, s: Signal) -> Result<()> {
        for (pid, _) in Arc::clone(&self.grandchildren)
            .lock()
            .map_err(|e| format_err!("lock grandchildren: {}", e))?
            .iter()
        {
            debug!("killing pid {}", pid);
            kill(Pid::from_raw(*pid), s)?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct ChildReaper {
    grandchildren: Arc<Mutex<HashMap<i32, bool>>>,
}

#[derive(Debug, Getters)]
pub struct ReapableChild {
    #[getset(get)]
    exit_paths: Vec<PathBuf>,
    #[getset(get)]
    pid: i32,
}

impl ReapableChild {
    pub fn from_child(child: &Child) -> Self {
        Self {
            pid: child.pid,
            exit_paths: child.exit_paths.clone(),
        }
    }

    fn watch(self) {
        let exit_paths = self.exit_paths().clone();
        let pid = *self.pid();
        std::thread::spawn(move || loop {
            let wait_status = waitpid(Pid::from_raw(pid), None);
            match wait_status {
                Ok(status) => {
                    if let WaitStatus::Exited(_, exit_status) = status {
                        let _ = self.write_to_exit_paths(exit_status, &exit_paths);
                    }
                }
                Err(err) => {
                    if err != nix::errno::Errno::ECHILD {
                        error!("caught error in reading for sigchld {}", err);
                    }
                }
            }
        });
    }

    fn write_to_exit_paths(&self, code: i32, paths: &Vec<PathBuf>) -> Result<()> {
        let code_str = format!("{}", code);
        for path in paths {
            debug!("writing exit code {} to {}", code, path.display());
            File::create(path)?.write_all(code_str.as_bytes())?;
        }
        Ok(())
    }
}
