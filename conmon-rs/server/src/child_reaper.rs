//! Child process reaping and management.
use crate::child::Child;
use crate::console::Console;
use anyhow::{format_err, Context, Result};
use getset::Getters;
use log::{debug, error};
use multimap::MultiMap;
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::Pid;
use std::io::{Error as IOError, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Mutex;
use std::{fs::File, io::Write, sync::Arc};

#[derive(Debug, Default)]
pub struct ChildReaper {
    grandchildren: Arc<Mutex<MultiMap<String, ReapableChild>>>,
}

impl ChildReaper {
    pub fn exists(&self, id: String) -> bool {
        let locked_grandchildren = Arc::clone(&self.grandchildren);
        let lock = locked_grandchildren.lock().unwrap();
        let child = lock.get(id.as_str());
        child.is_some()
    }

    pub async fn create_child<P, I, S>(
        &self,
        cmd: P,
        args: I,
        console: Option<Console>,
        pidfile: PathBuf,
    ) -> Result<i32>
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
            .map_err(|e| format_err!("wait for child process: {}", e))?;

        if let Some(console) = console {
            let _ = console
                .wait_connected()
                .context("wait for console socket connection");
        }

        let grandchild_pid = tokio::fs::read_to_string(pidfile)
            .await?
            .parse::<i32>()
            .map_err(|e| {
                IOError::new(
                    ErrorKind::Other,
                    format!("grandchild pid parse error {}", e),
                )
            })?;

        Ok(grandchild_pid)
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

    pub fn watch_grandchild(&self, child: Child) -> Result<()> {
        let locked_grandchildren = Arc::clone(&self.grandchildren);
        let mut map = locked_grandchildren
            .lock()
            .map_err(|e| format_err!("lock grandchildren: {}", e))?;
        let reapable_grandchild = ReapableChild::from_child(&child);
        let killed_channel = reapable_grandchild.watch();
        map.insert(child.id, reapable_grandchild);
        let cleanup_grandchildren = locked_grandchildren.clone();
        let pid = child.pid;
        tokio::task::spawn(async move {
            killed_channel.await.expect("no error on channel");
            if let Err(e) = Self::forget_grandchild(&cleanup_grandchildren, pid) {
                error!("error forgetting grandchild {}", e);
            }
        });
        Ok(())
    }

    fn forget_grandchild(
        locked_grandchildren: &Arc<Mutex<MultiMap<String, ReapableChild>>>,
        grandchild_pid: i32,
    ) -> Result<()> {
        let mut map = locked_grandchildren
            .lock()
            .map_err(|e| format_err!("lock grandchildren: {}", e))?;
        map.retain(|_, v| v.pid == grandchild_pid);
        Ok(())
    }

    pub fn kill_grandchildren(&self, s: Signal) -> Result<()> {
        for (_, grandchild) in Arc::clone(&self.grandchildren)
            .lock()
            .map_err(|e| format_err!("lock grandchildren: {}", e))?
            .iter()
        {
            debug!("killing pid {}", grandchild.pid());
            kill(Pid::from_raw(*grandchild.pid()), s)?;
        }
        Ok(())
    }
}

#[derive(Default, Debug, Getters)]
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

    fn watch(&self) -> tokio::sync::oneshot::Receiver<()> {
        let exit_paths = self.exit_paths().clone();
        let pid = *self.pid();
        let (tx, rx) = tokio::sync::oneshot::channel();

        tokio::task::spawn_blocking(move || {
            let wait_status = waitpid(Pid::from_raw(pid), None);
            match wait_status {
                Ok(status) => {
                    if let WaitStatus::Exited(_, exit_status) = status {
                        let _ = write_to_exit_paths(exit_status, &exit_paths);
                    }
                }
                Err(err) => {
                    if err != nix::errno::Errno::ECHILD {
                        error!("caught error in reading for sigchld {}", err);
                    }
                }
            };
            if tx.send(()).is_err() {
                error!("the receiver dropped the watch")
            }
        });
        rx
    }
}

fn write_to_exit_paths(code: i32, paths: &[PathBuf]) -> Result<()> {
    let code_str = format!("{}", code);
    for path in paths {
        debug!("writing exit code {} to {}", code, path.display());
        File::create(path)?.write_all(code_str.as_bytes())?;
    }
    Ok(())
}
