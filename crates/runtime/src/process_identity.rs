//! Native PID + birth identity. Inspection failures never imply process absence.
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub birth: String,
    pub executable: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRecord {
    pub version: u32,
    pub owner: ProcessIdentity,
    pub child: ProcessIdentity,
    pub role: String,
}

#[cfg(target_os = "macos")]
pub fn inspect(pid: u32) -> io::Result<Option<ProcessIdentity>> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid PID"));
    }
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut _,
            size,
        )
    };
    if got != size {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(e);
    }
    if info.pbi_status == libc::SZOMB {
        return Ok(None);
    }
    let mut path = vec![0u8; 4096];
    let length =
        unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr() as *mut _, path.len() as u32) };
    if length <= 0 {
        return Err(io::Error::last_os_error());
    }
    let end = path.iter().position(|b| *b == 0).unwrap_or(path.len());
    use std::os::unix::ffi::OsStringExt;
    let executable = PathBuf::from(std::ffi::OsString::from_vec(path[..end].to_vec()));
    let mut verify: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let checked = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut verify as *mut _ as *mut _,
            size,
        )
    };
    if checked != size {
        return Err(io::Error::other(
            "process disappeared during identity inspection",
        ));
    }
    if (info.pbi_start_tvsec, info.pbi_start_tvusec)
        != (verify.pbi_start_tvsec, verify.pbi_start_tvusec)
    {
        return Err(io::Error::other(
            "process changed during identity inspection",
        ));
    }
    if verify.pbi_status == libc::SZOMB {
        return Ok(None);
    }
    Ok(Some(ProcessIdentity {
        pid,
        birth: format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        executable,
    }))
}
#[cfg(target_os = "linux")]
pub fn inspect(pid: u32) -> io::Result<Option<ProcessIdentity>> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let stat = match std::fs::read_to_string(root.join("stat")) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process stat"))?
        .1
        .split_whitespace()
        .collect();
    if fields.first() == Some(&"Z") {
        return Ok(None);
    }
    let tick = fields
        .get(19)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing birth"))?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    Ok(Some(ProcessIdentity {
        pid,
        birth: format!("{}:{tick}", boot.trim()),
        executable: std::fs::read_link(root.join("exe"))?,
    }))
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn inspect(_pid: u32) -> io::Result<Option<ProcessIdentity>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native process identity unavailable",
    ))
}

pub fn record(pid: u32, role: &str, dir: &Path) -> io::Result<PathBuf> {
    let owner = inspect(std::process::id())?.ok_or_else(|| io::Error::other("owner exited"))?;
    let child =
        inspect(pid)?.ok_or_else(|| io::Error::other("child exited before registration"))?;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let path = dir.join(format!("{pid}.pid"));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(
        &mut tmp,
        &ProcessRecord {
            version: 1,
            owner,
            child,
            role: role.into(),
        },
    )?;
    tmp.as_file().sync_all()?;
    tmp.persist(&path).map_err(|e| e.error)?;
    Ok(path)
}

pub fn record_in_environment(pid: u32, role: &str) -> io::Result<Option<PathBuf>> {
    std::env::var_os("SEASNAIL_DATA_DIR")
        .map(|root| record(pid, role, &PathBuf::from(root).join("sidecars")))
        .transpose()
}

pub fn clear_if_exited(path: &Path) -> io::Result<bool> {
    let record: ProcessRecord = serde_json::from_slice(&std::fs::read(path)?)?;
    if inspect(record.child.pid)?.as_ref() == Some(&record.child) {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_identity_distinguishes_birth_and_confirmed_exit() {
        let current = inspect(std::process::id()).unwrap().unwrap();
        assert!(!current.birth.is_empty());
        assert!(current.executable.is_absolute());
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("10")
            .spawn()
            .unwrap();
        let identity = inspect(child.id()).unwrap().unwrap();
        assert_ne!(identity, current);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(inspect(identity.pid).unwrap(), None);
    }
}

/// Durable unknown-ownership marker exists before any OS child can be created.
pub struct SpawnRecord {
    path: PathBuf,
    owner: ProcessIdentity,
    role: String,
}
impl SpawnRecord {
    pub fn before_spawn(role: &str) -> io::Result<Option<Self>> {
        let Some(root) = std::env::var_os("SEASNAIL_DATA_DIR") else {
            return Ok(None);
        };
        let dir = PathBuf::from(root).join("sidecars");
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let owner = inspect(std::process::id())?
            .ok_or_else(|| io::Error::other("owner identity unavailable"))?;
        let mut pending = tempfile::Builder::new()
            .prefix("pending-")
            .suffix(".pid")
            .tempfile_in(&dir)?;
        serde_json::to_writer(
            &mut pending,
            &serde_json::json!({"version":1,"state":"pending","owner":owner,"role":role}),
        )?;
        pending.as_file().sync_all()?;
        let (_, path) = pending.keep().map_err(|error| error.error)?;
        std::fs::File::open(dir)?.sync_all()?;
        Ok(Some(Self {
            path,
            owner,
            role: role.into(),
        }))
    }
    pub fn register(self, pid: u32) -> io::Result<PathBuf> {
        let child =
            inspect(pid)?.ok_or_else(|| io::Error::other("child exited before registration"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())?;
        serde_json::to_writer(
            &mut temporary,
            &ProcessRecord {
                version: 1,
                owner: self.owner,
                child,
                role: self.role,
            },
        )?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        Ok(self.path)
    }
    pub fn no_child(self) -> io::Result<()> {
        std::fs::remove_file(self.path)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}
