use crate::{child::Child, console::Console, iostreams::IOStreams, version::Version, Server};
use anyhow::Context;
use capnp::{capability::Promise, Error};
use capnp_rpc::pry;
use conmon_common::conmon_capnp::conmon;
use log::debug;
use std::{path::PathBuf, sync::Arc};

macro_rules! pry_err {
    ($x:expr) => {
        pry!($x.map_err(|e| Error::failed(format!("{:#}", e))))
    };
}

impl conmon::Server for Server {
    fn version(
        &mut self,
        _: conmon::VersionParams,
        mut results: conmon::VersionResults,
    ) -> Promise<(), capnp::Error> {
        debug!("Got a version request");
        let mut response = results.get().init_response();
        let version = Version::new();
        response.set_version(version.version());
        response.set_tag(version.tag());
        response.set_commit(version.commit());
        response.set_build_date(version.build_date());
        response.set_rust_version(version.rust_version());
        Promise::ok(())
    }

    fn create_container(
        &mut self,
        params: conmon::CreateContainerParams,
        mut results: conmon::CreateContainerResults,
    ) -> Promise<(), capnp::Error> {
        use std::io::{Error as IOError, ErrorKind};
        let req = pry!(pry!(params.get()).get_request());
        debug!(
            "Got a create container request for id {}",
            pry!(req.get_id())
        );

        let maybe_console = if req.get_terminal() {
            pry_err!(Console::new()).into()
        } else {
            pry_err!(pry_err!(IOStreams::new()).start());
            None
        };

        let pidfile = pry!(pidfile_from_params(&params));
        let child_reaper = Arc::clone(self.reaper());
        let children = Arc::clone(self.children());
        let args = pry_err!(self.generate_runtime_args(&params, &maybe_console, &pidfile));
        let runtime = self.config().runtime().clone();
        let id = req.get_id().unwrap().to_string();
        let exit_paths = pry!(path_vec_from_text_list(pry!(req.get_exit_paths())));

        Promise::from_future(async move {
            let status = child_reaper
                .create_child(runtime, args)
                .await
                .map_err(|e| IOError::new(ErrorKind::Other, format!("Error {}", e)))?;

            debug!("Status for container ID {} is {}", id, status);

            if let Some(console) = maybe_console {
                let _ = console
                    .wait_connected()
                    .context("wait for console socket connection");
            }

            let pid = tokio::fs::read_to_string(pidfile)
                .await?
                .parse::<i32>()
                .map_err(|e| IOError::new(ErrorKind::Other, format!("pid parse error {}", e)))?;

            // register child with server
            let child = Child::new(id, pid, exit_paths);
            let _ = child_reaper.watch_grandchild(&child);
            children.write().unwrap().insert(child.id.clone(), child);

            // TODO FIXME why convert?
            results.get().init_response().set_container_pid(pid as u32);
            Ok(())
        })
    }

    fn exec_sync_container(
        &mut self,
        params: conmon::ExecSyncContainerParams,
        mut results: conmon::ExecSyncContainerResults,
    ) -> Promise<(), capnp::Error> {
        use std::io::{Error, ErrorKind};
        let req = pry!(pry!(params.get()).get_request());
        let id = pry!(req.get_id());
        let timeout = req.get_timeout();
        let command = self.generate_exec_sync_args(&params).unwrap();
        let runtime = self.config.runtime().clone();
        debug!(
            "Got exec sync container request for id {} with timeout {} : {}",
            id,
            timeout,
            command.join(" ")
        );
        let children = self.children.read().unwrap();
        let child = children.get(id);
        if child.is_none() {
            let mut resp = results.get().init_response();
            resp.set_exit_code(-1);
            return Promise::ok(());
        }
        debug!("found child with id {}", child.unwrap().id);
        let child_reaper = Arc::clone(self.reaper());
        Promise::from_future(async move {
            let (exit_status, stdout, stderr) = child_reaper
                .exec_sync(&runtime, command, timeout)
                .await
                .map_err(|e| Error::new(ErrorKind::Other, format!("Error {}", e)))?;
            let mut resp = results.get().init_response();
            resp.set_exit_code(exit_status.code().unwrap());
            resp.set_stdout(stdout.as_str());
            resp.set_stderr(stderr.as_str());
            Ok(())
        })
    }
}

fn pidfile_from_params(params: &conmon::CreateContainerParams) -> capnp::Result<PathBuf> {
    let mut pidfile_pathbuf = PathBuf::from(params.get()?.get_request()?.get_bundle_path()?);
    pidfile_pathbuf.push("pidfile");

    debug!("pidfile is {}", pidfile_pathbuf.display());
    Ok(pidfile_pathbuf)
}

fn path_vec_from_text_list(tl: capnp::text_list::Reader) -> Result<Vec<PathBuf>, capnp::Error> {
    tl.iter().map(|r| r.map(PathBuf::from)).collect()
}
