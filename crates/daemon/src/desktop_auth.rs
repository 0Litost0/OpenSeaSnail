//! Desktop-only capability. Never returned in HTTP responses or WebView IPC.
use rand::RngCore;
use std::{
    io::{self, Read, Write},
    path::Path,
};
const FILE: &str = ".desktop-auth-capability";
pub fn rotate_capability(home: &Path) -> io::Result<String> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let key: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut file = tempfile::NamedTempFile::new_in(home)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(key.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(home.join(FILE)).map_err(|error| error.error)?;
    Ok(key)
}
pub fn read_capability(home: &Path) -> io::Result<String> {
    let path = home.join(FILE);
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != 64 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    let mut key = String::new();
    file.take(65).read_to_string(&mut key)?;
    if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(key)
}
