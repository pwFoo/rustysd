use crate::services::Service;

#[cfg(feature = "cgroups")]
use crate::platform::cgroups;

pub fn kill(srvc: &mut Service, sig: nix::sys::signal::Signal) -> Result<(), String> {
    #[cfg(feature = "cgroups")]
    {
        if nix::unistd::getuid().is_root() {
            cgroups::freeze_kill_thaw_cgroup(&srvc.platform_specific.cgroup_path, sig)
                .map_err(|e| format!("{}", e))?;
        }
    }
    let _ = srvc;
    let _ = sig;
    Ok(())
}
