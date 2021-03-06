use super::start_service::*;
use crate::platform::EventFd;
use crate::units::*;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixDatagram;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Debug)]
pub struct ServiceRuntimeInfo {
    pub restarted: u64,
    pub up_since: Option<std::time::Instant>,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct PlatformSpecificServiceFields {
    pub cgroup_path: std::path::PathBuf,
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub struct PlatformSpecificServiceFields {}

#[derive(Debug)]
pub struct Service {
    pub pid: Option<nix::unistd::Pid>,
    pub service_config: ServiceConfig,

    pub socket_names: Vec<String>,

    pub status_msgs: Vec<String>,

    pub process_group: Option<nix::unistd::Pid>,

    pub runtime_info: ServiceRuntimeInfo,
    pub signaled_ready: bool,

    pub notifications: Option<Arc<Mutex<UnixDatagram>>>,
    pub notifications_path: Option<std::path::PathBuf>,

    pub stdout_dup: Option<(RawFd, RawFd)>,
    pub stderr_dup: Option<(RawFd, RawFd)>,
    pub notifications_buffer: String,
    pub stdout_buffer: Vec<u8>,
    pub stderr_buffer: Vec<u8>,
    pub uid: nix::unistd::Uid,
    pub gid: nix::unistd::Gid,
    pub supp_gids: Vec<nix::unistd::Gid>,

    pub platform_specific: PlatformSpecificServiceFields,
}

pub enum RunCmdError {
    Timeout(String, String),
    SpawnError(String, String),
    WaitError(String, String),
    BadExitCode(String, crate::signal_handler::ChildTermination),
    Generic(String),
}

impl std::fmt::Display for RunCmdError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        let msg = match self {
            RunCmdError::BadExitCode(cmd, exit) => format!("{} exited with: {:?}", cmd, exit),
            RunCmdError::SpawnError(cmd, err) => format!("{} failed to spawn with: {:?}", cmd, err),
            RunCmdError::WaitError(cmd, err) => {
                format!("{} could not be waited on because: {:?}", cmd, err)
            }
            RunCmdError::Timeout(cmd, err) => format!("{} reached its timeout: {:?}", cmd, err),
            RunCmdError::Generic(err) => format!("Generic error: {}", err),
        };
        fmt.write_str(format!("{}", msg).as_str())
    }
}

pub enum StartResult {
    Started,
    WaitingForSocket,
}

pub enum ServiceErrorReason {
    PrestartFailed(RunCmdError),
    PoststartFailed(RunCmdError),
    StartFailed(RunCmdError),
    PoststopFailed(RunCmdError),
    StopFailed(RunCmdError),

    PrestartAndPoststopFailed(RunCmdError, RunCmdError),
    PoststartAndPoststopFailed(RunCmdError, RunCmdError),
    StartAndPoststopFailed(RunCmdError, RunCmdError),
    StopAndPoststopFailed(RunCmdError, RunCmdError),
    PreparingFailed(String),
    Generic(String),
    AlreadyHasPID(nix::unistd::Pid),
    AlreadyHasPGID(nix::unistd::Pid),
}

impl std::fmt::Display for ServiceErrorReason {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        let msg = match self {
            // one failed
            ServiceErrorReason::PrestartFailed(e) => format!("Perstart failed: {}", e),
            ServiceErrorReason::PoststartFailed(e) => format!("Poststart failed: {}", e),
            ServiceErrorReason::StartFailed(e) => format!("Start failed: {}", e),
            ServiceErrorReason::StopFailed(e) => format!("Stop failed: {}", e),
            ServiceErrorReason::PoststopFailed(e) => format!("Poststop failed: {}", e),

            // Both failed
            ServiceErrorReason::PrestartAndPoststopFailed(e, e2) => {
                format!("Perstart failed: {} and Poststop failed too: {}", e, e2)
            }
            ServiceErrorReason::PoststartAndPoststopFailed(e, e2) => {
                format!("Poststart failed: {} and Poststop failed too: {}", e, e2)
            }
            ServiceErrorReason::StartAndPoststopFailed(e, e2) => {
                format!("Start failed: {} and Poststop failed too: {}", e, e2)
            }
            ServiceErrorReason::StopAndPoststopFailed(e, e2) => {
                format!("Stop failed: {} and Poststop failed too: {}", e, e2)
            }

            // other errors
            ServiceErrorReason::Generic(e) => format!("Service error: {}", e),
            ServiceErrorReason::AlreadyHasPID(e) => {
                format!("Tried to start already running service (PID: {})", e)
            }
            ServiceErrorReason::AlreadyHasPGID(e) => {
                format!("Tried to start already running service: (PGID: {})", e)
            }
            ServiceErrorReason::PreparingFailed(e) => {
                format!("Preparing of service failed because: {}", e)
            }
        };
        fmt.write_str(format!("{}", msg).as_str())
    }
}

impl Service {
    pub fn start(
        &mut self,
        id: UnitId,
        name: &str,
        fd_store: ArcMutFDStore,
        pid_table: ArcMutPidTable,
        notification_socket_path: std::path::PathBuf,
        eventfds: &[EventFd],
        allow_ignore: bool,
    ) -> Result<StartResult, ServiceErrorReason> {
        if let Some(pid) = self.pid {
            return Err(ServiceErrorReason::AlreadyHasPID(pid));
        }
        if let Some(pgid) = self.process_group {
            return Err(ServiceErrorReason::AlreadyHasPID(pgid));
        }
        if self.service_config.accept {
            return Err(ServiceErrorReason::Generic(
                "Inetd style activation is not supported".into(),
            ));
        }
        if !allow_ignore || self.socket_names.is_empty() {
            trace!("Start service {}", name);

            super::prepare_service::prepare_service(self, name, &notification_socket_path)
                .map_err(|e| ServiceErrorReason::PreparingFailed(e))?;
            self.run_prestart(id, name, pid_table.clone())
                .map_err(
                    |prestart_err| match self.run_poststop(id, name, pid_table.clone()) {
                        Ok(_) => ServiceErrorReason::PrestartFailed(prestart_err),
                        Err(poststop_err) => ServiceErrorReason::PrestartAndPoststopFailed(
                            prestart_err,
                            poststop_err,
                        ),
                    },
                )?;
            {
                let mut pid_table_locked = pid_table.lock().unwrap();
                // This mainly just forks the process. The waiting (if necessary) is done below
                // Doing it under the lock of the pid_table prevents races between processes exiting very
                // fast and inserting the new pid into the pid table
                start_service(self, name.clone(), &*fd_store.read().unwrap())
                    .map_err(|e| ServiceErrorReason::StartFailed(e))?;
                if let Some(new_pid) = self.pid {
                    pid_table_locked.insert(
                        new_pid,
                        PidEntry::Service(id, self.service_config.srcv_type),
                    );
                    crate::platform::notify_event_fds(&eventfds);
                }
            }
            if let Some(sock) = &self.notifications {
                let sock = sock.clone();
                super::fork_parent::wait_for_service(
                    self,
                    name,
                    &*sock.lock().unwrap(),
                    pid_table.clone(),
                )
                .map_err(|start_err| {
                    match self.run_poststop(id, name, pid_table.clone()) {
                        Ok(_) => ServiceErrorReason::StartFailed(start_err),
                        Err(poststop_err) => {
                            ServiceErrorReason::StartAndPoststopFailed(start_err, poststop_err)
                        }
                    }
                })?;
            }
            self.run_poststart(id, name, pid_table.clone())
                .map_err(
                    |poststart_err| match self.run_poststop(id, name, pid_table.clone()) {
                        Ok(_) => ServiceErrorReason::PrestartFailed(poststart_err),
                        Err(poststop_err) => ServiceErrorReason::PoststartAndPoststopFailed(
                            poststart_err,
                            poststop_err,
                        ),
                    },
                )?;
            Ok(StartResult::Started)
        } else {
            trace!(
                "Ignore service {} start, waiting for socket activation instead",
                name,
            );
            crate::platform::notify_event_fds(&eventfds);
            Ok(StartResult::WaitingForSocket)
        }
    }

    pub fn kill_all_remaining_processes(&mut self, name: &str) {
        if let Some(proc_group) = self.process_group {
            // TODO handle these errors
            match nix::sys::signal::kill(proc_group, nix::sys::signal::Signal::SIGKILL) {
                Ok(_) => trace!("Success killing process group for service {}", name,),
                Err(e) => error!("Error killing process group for service {}: {}", name, e,),
            }
        } else {
            trace!("Tried to kill service that didn't have a process-group. This might have resulted in orphan processes.");
        }
        match super::kill_os_specific::kill(self, nix::sys::signal::Signal::SIGKILL) {
            Ok(_) => trace!("Success killing process os specificly for service {}", name,),
            Err(e) => error!(
                "Error killing process os specificly for service {}: {}",
                name, e,
            ),
        }
    }

    fn stop(
        &mut self,
        id: UnitId,
        name: &str,
        pid_table: ArcMutPidTable,
    ) -> Result<(), RunCmdError> {
        let stop_res = self.run_stop_cmd(id, name, pid_table.clone());

        if self.service_config.srcv_type != ServiceType::OneShot {
            // already happened when the oneshot process exited in the exit handler
            self.kill_all_remaining_processes(name);
        }

        self.pid = None;
        self.process_group = None;
        stop_res
    }

    pub fn kill(
        &mut self,
        id: UnitId,
        name: &str,
        pid_table: ArcMutPidTable,
    ) -> Result<(), ServiceErrorReason> {
        self.stop(id, name, pid_table.clone()).map_err(|stop_err| {
            match self.run_poststop(id, name, pid_table.clone()) {
                Ok(_) => ServiceErrorReason::PrestartFailed(stop_err),
                Err(poststop_err) => {
                    ServiceErrorReason::StopAndPoststopFailed(stop_err, poststop_err)
                }
            }
        })
    }

    pub fn get_start_timeout(&self) -> Option<std::time::Duration> {
        if let Some(timeout) = &self.service_config.starttimeout {
            match timeout {
                Timeout::Duration(dur) => Some(*dur),
                Timeout::Infinity => None,
            }
        } else {
            if let Some(timeout) = &self.service_config.generaltimeout {
                match timeout {
                    Timeout::Duration(dur) => Some(*dur),
                    Timeout::Infinity => None,
                }
            } else {
                // TODO add default timeout if neither starttimeout nor generaltimeout was set
                None
            }
        }
    }

    fn get_stop_timeout(&self) -> Option<std::time::Duration> {
        if let Some(timeout) = &self.service_config.stoptimeout {
            match timeout {
                Timeout::Duration(dur) => Some(*dur),
                Timeout::Infinity => None,
            }
        } else {
            if let Some(timeout) = &self.service_config.generaltimeout {
                match timeout {
                    Timeout::Duration(dur) => Some(*dur),
                    Timeout::Infinity => None,
                }
            } else {
                // TODO add default timeout if neither starttimeout nor generaltimeout was set
                None
            }
        }
    }

    fn run_cmd(
        &mut self,
        cmd_str: &str,
        id: UnitId,
        name: &str,
        timeout: Option<std::time::Duration>,
        pid_table: ArcMutPidTable,
    ) -> Result<(), RunCmdError> {
        let split = cmd_str.split(' ').collect::<Vec<_>>();
        let mut cmd = Command::new(split[0]);
        for part in &split[1..] {
            cmd.arg(part);
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());
        trace!("Run {} for service: {}", cmd_str, name);
        let spawn_result = {
            let mut pid_table_locked = pid_table.lock().unwrap();
            let res = cmd.spawn();
            if let Ok(child) = &res {
                pid_table_locked.insert(
                    nix::unistd::Pid::from_raw(child.id() as i32),
                    PidEntry::Helper(id, name.to_string()),
                );
            }
            res
        };
        match spawn_result {
            Ok(mut child) => {
                trace!("Wait for {} for service: {}", cmd_str, name);
                let wait_result: Result<(), RunCmdError> =
                    match wait_for_helper_child(&mut child, pid_table.clone(), timeout) {
                        WaitResult::InTime(Err(e)) => {
                            return Err(RunCmdError::WaitError(
                                cmd_str.to_owned(),
                                format!("{}", e),
                            ));
                        }
                        WaitResult::InTime(Ok(exitstatus)) => {
                            trace!("success running {} for service: {}", cmd_str, name);
                            if exitstatus.success() {
                                Ok(())
                            } else {
                                Err(RunCmdError::BadExitCode(cmd_str.to_owned(), exitstatus))
                            }
                        }
                        WaitResult::TimedOut => {
                            trace!("Timeout running {} for service: {}", cmd_str, name);
                            let _ = child.kill();
                            Err(RunCmdError::Timeout(
                                cmd_str.to_owned(),
                                format!("Timeout ({:?}) reached", timeout),
                            ))
                        }
                    };
                {
                    use std::io::Read;
                    if let Some(stream) = &mut child.stderr {
                        let mut buf = Vec::new();
                        let _bytes = stream.read_to_end(&mut buf).unwrap();
                        self.stderr_buffer.extend(buf);
                    }
                    if let Some(stream) = &mut child.stdout {
                        let mut buf = Vec::new();
                        let _bytes = stream.read_to_end(&mut buf).unwrap();
                        self.stdout_buffer.extend(buf);
                    }
                }

                pid_table
                    .lock()
                    .unwrap()
                    .remove(&nix::unistd::Pid::from_raw(child.id() as i32));
                wait_result
            }
            Err(e) => Err(RunCmdError::SpawnError(
                cmd_str.to_owned(),
                format!("{}", e),
            )),
        }
    }

    fn run_all_cmds(
        &mut self,
        cmds: &Vec<String>,
        id: UnitId,
        name: &str,
        timeout: Option<std::time::Duration>,
        pid_table: ArcMutPidTable,
    ) -> Result<(), RunCmdError> {
        for cmd in cmds {
            self.run_cmd(cmd, id, name, timeout, pid_table.clone())?;
        }
        Ok(())
    }

    fn run_stop_cmd(
        &mut self,
        id: UnitId,
        name: &str,
        pid_table: ArcMutPidTable,
    ) -> Result<(), RunCmdError> {
        if self.service_config.stop.is_empty() {
            return Ok(());
        }
        let timeout = self.get_stop_timeout();
        let cmds = self.service_config.stop.clone();
        self.run_all_cmds(&cmds, id, name, timeout, pid_table.clone())
    }
    fn run_prestart(
        &mut self,
        id: UnitId,
        name: &str,
        pid_table: ArcMutPidTable,
    ) -> Result<(), RunCmdError> {
        if self.service_config.startpre.is_empty() {
            return Ok(());
        }
        let timeout = self.get_start_timeout();
        let cmds = self.service_config.startpre.clone();
        self.run_all_cmds(&cmds, id, name, timeout, pid_table.clone())
    }
    fn run_poststart(
        &mut self,
        id: UnitId,
        name: &str,
        pid_table: ArcMutPidTable,
    ) -> Result<(), RunCmdError> {
        if self.service_config.startpost.is_empty() {
            return Ok(());
        }
        let timeout = self.get_start_timeout();
        let cmds = self.service_config.startpost.clone();
        self.run_all_cmds(&cmds, id, name, timeout, pid_table.clone())
    }

    fn run_poststop(
        &mut self,
        id: UnitId,
        name: &str,
        pid_table: ArcMutPidTable,
    ) -> Result<(), RunCmdError> {
        if self.service_config.startpost.is_empty() {
            return Ok(());
        }
        let timeout = self.get_start_timeout();
        let cmds = self.service_config.stoppost.clone();
        self.run_all_cmds(&cmds, id, name, timeout, pid_table.clone())
    }
}

enum WaitResult {
    TimedOut,
    InTime(std::io::Result<crate::signal_handler::ChildTermination>),
}

/// Wait for the termination of a subprocess, with an optional timeout.
/// An error does not mean that the waiting actually failed.
/// This might also happen because it was collected by the signal_handler.
/// This could be fixed by using the waitid() with WNOWAIT in the signal handler but
/// that has not been ported to rust
fn wait_for_helper_child(
    child: &mut std::process::Child,
    pid_table: ArcMutPidTable,
    time_out: Option<std::time::Duration>,
) -> WaitResult {
    let pid = nix::unistd::Pid::from_raw(child.id() as i32);
    let mut counter = 1u64;
    let start_time = std::time::Instant::now();
    loop {
        if let Some(time_out) = time_out {
            if start_time.elapsed() >= time_out {
                return WaitResult::TimedOut;
            }
        }
        {
            let mut pid_table_locked = pid_table.lock().unwrap();
            match pid_table_locked.get(&pid) {
                Some(entry) => {
                    match entry {
                        PidEntry::OneshotExited(_) => {
                            // Should never happen
                            unreachable!(
                            "Was waiting on helper process but pid got saved as PidEntry::OneshotExited"
                        );
                        }
                        PidEntry::Service(_, _) => {
                            // Should never happen
                            unreachable!(
                            "Was waiting on helper process but pid got saved as PidEntry::Service"
                        );
                        }
                        PidEntry::Helper(_, _) => {
                            // Need to wait longer
                        }
                        PidEntry::HelperExited(_) => {
                            let entry_owned = pid_table_locked.remove(&pid).unwrap();
                            if let PidEntry::HelperExited(termination_owned) = entry_owned {
                                return WaitResult::InTime(Ok(termination_owned));
                            }
                        }
                    }
                }
                None => {
                    // Should not happen. Either there is an Helper entry oder a Exited entry
                    unreachable!("No entry for child found")
                }
            }
        }
        // exponential backoff to get low latencies for fast processes
        // but not hog the cpu for too long
        // start at 0.05 ms
        // capped to 10 ms to not introduce too big latencies
        // TODO review those numbers
        let sleep_dur = std::time::Duration::from_micros(counter * 50);
        let sleep_cap = std::time::Duration::from_millis(10);
        let sleep_dur = sleep_dur.min(sleep_cap);
        if sleep_dur < sleep_cap {
            counter = counter * 2;
        }
        std::thread::sleep(sleep_dur);
    }
}
