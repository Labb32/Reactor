use std::{
    io,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(50);

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

    pub struct Containment {
        process_id: Cell<Option<u32>>,
    }

    pub fn prepare(command: &mut Command) {
        use std::os::unix::process::CommandExt;

        command.process_group(0);
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
                Some(process_id) => signal_group(process_id, SIGKILL),
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
            Threading::CREATE_NEW_PROCESS_GROUP,
        },
    };

    pub struct Containment {
        job: HANDLE,
    }

    pub fn prepare(command: &mut Command) {
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
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
                Ok(())
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
