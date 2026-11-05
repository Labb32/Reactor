use std::{
    io,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Fail closed: a PID/group that is alive or inaccessible is not safe to recover.
pub fn process_absent(process_id: u32) -> bool {
    platform::process_absent(process_id)
}

pub fn process_identity(process_id: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        let stat = std::fs::read_to_string(format!("/proc/{process_id}/stat")).ok()?;
        let fields = stat.rsplit_once(')')?.1.split_whitespace().collect::<Vec<_>>();
        // /proc stat fields 5 (group) and 22 (kernel start ticks).
        return Some(format!("linux:{}:{}:{}:{}", boot.trim(), process_id, fields.get(2)?, fields.get(19)?));
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{Foundation::{CloseHandle, FILETIME}, System::Threading::{OpenProcess, GetProcessTimes, PROCESS_QUERY_LIMITED_INFORMATION}};
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if handle.is_null() { return None; }
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let ok = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } != 0;
        unsafe { CloseHandle(handle); }
        return ok.then(|| format!("windows:{process_id}:{}:{}", created.dwHighDateTime, created.dwLowDateTime));
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    { let _ = process_id; None }
}

pub struct ProcessSupervisor {
    child: Child,
    containment: platform::Containment,
    finished: bool,
}

impl ProcessSupervisor {
    pub fn spawn(mut command: Command) -> io::Result<Self> {
        platform::prepare(&mut command);
        let containment = platform::Containment::new()?;
        let mut child = command.spawn()?;
        if let Err(error) = containment.attach(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self {
            child,
            containment,
            finished: false,
        })
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn wait_for(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(Some(self.complete(status)));
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Ok(None);
            }
            thread::sleep(POLL_INTERVAL.min(timeout - elapsed));
        }
    }

    pub fn stop(&mut self, grace_period: Duration) -> io::Result<ExitStatus> {
        if let Some(status) = self.child.try_wait()? {
            return Ok(self.complete(status));
        }
        if self.containment.request_stop(self.child.id()).is_ok()
            && let Some(status) = self.wait_for(grace_period)?
        {
            return Ok(status);
        }
        self.containment.force_stop()?;
        let status = self.child.wait()?;
        self.finished = true;
        Ok(status)
    }

    fn complete(&mut self, status: ExitStatus) -> ExitStatus {
        let _ = self.containment.force_stop();
        self.finished = true;
        status
    }
}

impl Drop for ProcessSupervisor {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = self.containment.force_stop();
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.finished = true;
    }
}

#[cfg(unix)]
mod platform {
    use std::{cell::Cell, io, process::Command};

    use libc::{ESRCH, SIGKILL, SIGTERM, kill};

    pub fn process_absent(process_id: u32) -> bool {
        let Ok(pid) = i32::try_from(process_id) else { return false; };
        if pid <= 0 { return false; }
        // SAFETY: Signal zero probes existence without changing either process/group.
        for target in [pid, -pid] {
            if unsafe { kill(target, 0) } == 0
                || io::Error::last_os_error().raw_os_error() != Some(ESRCH) {
                return false;
            }
        }
        true
    }

    pub struct Containment {
        process_id: Cell<Option<u32>>,
    }

    pub fn prepare(command: &mut Command) {
        use std::os::unix::process::CommandExt;

        command.process_group(0);
        #[cfg(target_os = "linux")]
        {
            let parent = std::process::id() as libc::pid_t;
            // SAFETY: Only async-signal-safe syscalls are used between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, SIGKILL) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        return Err(io::Error::other("supervisor exited during spawn"));
                    }
                    Ok(())
                });
            }
        }
    }

    impl Containment {
        pub fn new() -> io::Result<Self> {
            Ok(Self {
                process_id: Cell::new(None),
            })
        }

        pub fn attach(&self, child: &std::process::Child) -> io::Result<()> {
            self.process_id.set(Some(child.id()));
            Ok(())
        }

        pub fn request_stop(&self, process_id: u32) -> io::Result<()> {
            signal_group(process_id, SIGTERM)
        }

        pub fn force_stop(&self) -> io::Result<()> {
            match self.process_id.get() {
                Some(process_id) => {
                    signal_group(process_id, SIGKILL)?;
                    // Do not signal a recycled process-group ID when this handle drops later.
                    self.process_id.set(None);
                    Ok(())
                }
                None => Ok(()),
            }
        }
    }

    impl Drop for Containment {
        fn drop(&mut self) {
            // Remove descendants that outlive the supervised root process.
            let _ = self.force_stop();
        }
    }

    fn signal_group(process_id: u32, signal: i32) -> io::Result<()> {
        let process_id = i32::try_from(process_id)
            .map_err(|_| io::Error::other("process ID exceeds platform range"))?;
        // SAFETY: The range-checked negative ID targets only the isolated child group.
        if unsafe { kill(-process_id, signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        ffi::c_void,
        io,
        mem::size_of,
        os::windows::{io::AsRawHandle, process::CommandExt},
        process::Command,
        ptr,
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent},
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject,
            },
            Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED},
        },
    };

    pub struct Containment {
        job: HANDLE,
    }

    pub fn process_absent(process_id: u32) -> bool {
        use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
        // SAFETY: Only synchronization access is requested; no process is signalled.
        let handle = unsafe { OpenProcess(0x00100000, 0, process_id) };
        if handle.is_null() {
            return io::Error::last_os_error().raw_os_error() == Some(87);
        }
        let stopped = unsafe { WaitForSingleObject(handle, 0) } == 0;
        unsafe { CloseHandle(handle); }
        stopped
    }

    pub fn prepare(command: &mut Command) {
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED);
    }

    impl Containment {
        pub fn new() -> io::Result<Self> {
            // SAFETY: Null arguments create an unnamed job with default security attributes.
            let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: The information pointer is valid for the duration of the call.
            let configured = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const information).cast::<c_void>(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                let error = io::Error::last_os_error();
                // SAFETY: The handle was created above and has not been closed.
                unsafe {
                    CloseHandle(job);
                }
                return Err(error);
            }
            Ok(Self { job })
        }

        pub fn attach(&self, child: &std::process::Child) -> io::Result<()> {
            let process = child.as_raw_handle() as HANDLE;
            // SAFETY: Both handles remain owned and valid for the duration of the call.
            if unsafe { AssignProcessToJobObject(self.job, process) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                resume_suspended_child(child.id())
            }
        }

        pub fn request_stop(&self, process_id: u32) -> io::Result<()> {
            // SAFETY: The child was created as a process group with this identifier.
            if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, process_id) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }

        pub fn force_stop(&self) -> io::Result<()> {
            // SAFETY: The job handle remains valid until Containment is dropped.
            if unsafe { TerminateJobObject(self.job, 1) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    fn resume_suspended_child(process_id: u32) -> io::Result<()> {
        use windows_sys::Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            System::{
                Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD},
                Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
            },
        };
        // The child starts suspended: no user code runs before Job assignment.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()); }
        let mut entry = THREADENTRY32::default();
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut found = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        let mut result = Err(io::Error::other("suspended child thread not found"));
        while found {
            if entry.th32OwnerProcessID == process_id {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() { result = Err(io::Error::last_os_error()); }
                else {
                    let previous = unsafe { ResumeThread(thread) };
                    result = if previous == 1 { Ok(()) } else { Err(io::Error::other("unexpected child suspension state")) };
                    unsafe { CloseHandle(thread); }
                }
                break;
            }
            found = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot); }
        result
    }

    impl Drop for Containment {
        fn drop(&mut self) {
            // SAFETY: Containment owns the handle and closes it exactly once.
            unsafe {
                CloseHandle(self.job);
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::{io, process::Command};

    pub struct Containment;

    pub fn process_absent(_process_id: u32) -> bool { false }

    pub fn prepare(_command: &mut Command) {}

    impl Containment {
        pub fn new() -> io::Result<Self> {
            Ok(Self)
        }

        pub fn attach(&self, _child: &std::process::Child) -> io::Result<()> {
            Ok(())
        }

        pub fn request_stop(&self, _process_id: u32) -> io::Result<()> {
            Ok(())
        }

        pub fn force_stop(&self) -> io::Result<()> {
            Ok(())
        }
    }
}
