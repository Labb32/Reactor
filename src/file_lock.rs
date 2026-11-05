pub(crate) use platform::{try_lock, try_lock_mode, unlock};

#[cfg(unix)]
mod platform {
    use std::{fs::File, io, os::fd::AsRawFd};

    pub fn try_lock(file: &File) -> io::Result<bool> {
        try_lock_mode(file, true)
    }

    pub fn try_lock_mode(file: &File, exclusive: bool) -> io::Result<bool> {
        // SAFETY: flock only reads the valid descriptor owned by file.
        let mode = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        if unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            Ok(false)
        } else {
            Err(error)
        }
    }

    pub fn unlock(file: &File) -> io::Result<()> {
        // SAFETY: flock only reads the valid descriptor owned by file.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::{fs::File, io, mem::zeroed, os::windows::io::AsRawHandle};

    use windows_sys::Win32::{
        Foundation::{ERROR_LOCK_VIOLATION, GetLastError, HANDLE},
        Storage::FileSystem::{
            LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, UnlockFileEx,
        },
        System::IO::OVERLAPPED,
    };

    pub fn try_lock(file: &File) -> io::Result<bool> {
        try_lock_mode(file, true)
    }

    pub fn try_lock_mode(file: &File, exclusive: bool) -> io::Result<bool> {
        // SAFETY: OVERLAPPED is initialized and the file handle remains valid.
        let mut overlapped = unsafe { zeroed::<OVERLAPPED>() };
        let result = unsafe {
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                (if exclusive {
                    LOCKFILE_EXCLUSIVE_LOCK
                } else {
                    0
                }) | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if result != 0 {
            return Ok(true);
        }
        // SAFETY: GetLastError has no preconditions.
        if unsafe { GetLastError() } == ERROR_LOCK_VIOLATION {
            Ok(false)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn unlock(file: &File) -> io::Result<()> {
        // SAFETY: OVERLAPPED matches the range locked above and the handle is valid.
        let mut overlapped = unsafe { zeroed::<OVERLAPPED>() };
        if unsafe { UnlockFileEx(file.as_raw_handle() as HANDLE, 0, 1, 0, &mut overlapped) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}
