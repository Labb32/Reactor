use crate::error::{ReactorError, Result, io_error};
use std::{fs, io::Write, path::Path};

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| ReactorError::InvalidInput("file has no parent".to_owned()))?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    let temporary = parent.join(format!(".{}.tmp", crate::ids::new_id()));
    let mut file = crate::permissions::create_private_new_file(&temporary)
        .map_err(|error| io_error(&temporary, error))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| io_error(&temporary, error))?;
    drop(file);
    replace_file(&temporary, path)
}

pub fn replace_file(temporary: &Path, path: &Path) -> Result<()> {
    #[cfg(not(windows))]
    fs::rename(temporary, path).map_err(|error| io_error(path, error))?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let from = temporary
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let to = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        if unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(io_error(path, std::io::Error::last_os_error()));
        }
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|error| io_error(parent, error))?;
    }
    Ok(())
}
