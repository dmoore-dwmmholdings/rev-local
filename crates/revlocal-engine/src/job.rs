//! Windows Job Objects, so a killed engine takes its children with it (SPEC §8.5).
//!
//! # Why this exists
//!
//! Unix gives a process group: one `kill(-pgid)` reaches everything the engine
//! spawned. Windows has no equivalent, and `TerminateProcess` reaches exactly one
//! process. That difference is not academic. An engine invoked through a `.cmd`
//! shim — which is how `claude` and `codex` are installed on Windows — is really
//! `cmd.exe` with `node` beneath it. Terminating the child kills `cmd.exe` and
//! leaves `node` running, holding the pipes rev-local is reading.
//!
//! The observed symptom was not a failing test. It was a **hang**: the read never
//! ended, so a cancellation test that should finish in three seconds ran past a
//! ten-second timeout, past the job's forty-five-minute bound, and stopped every
//! test binary queued behind it. Four CI runs in a row ended at that line.
//!
//! A Job Object is the supported answer. A process assigned to one is killed with
//! it, and so is everything it spawns.
//!
//! # `KILL_ON_JOB_CLOSE` is the part that survives us crashing
//!
//! `TerminateJobObject` handles the ordinary path. The limit flag handles the
//! other one: if rev-local dies, the handle closes with the process and Windows
//! reaps the tree. Without it, a crashed daemon leaves engines running with
//! nothing left that knows their PIDs — the exact orphan §12.1 has `kill --hard`
//! for, except nobody would be left to run it.
//!
//! # The assignment race, stated rather than hidden
//!
//! The job is created before the spawn and the child is assigned immediately
//! after, which leaves a window: a process that spawns a grandchild in its first
//! microseconds could have that grandchild escape. Closing it properly needs
//! `PROC_THREAD_ATTRIBUTE_JOB_LIST` and a hand-rolled `STARTUPINFOEX`, which means
//! not using `tokio::process` at all.
//!
//! That trade is deliberate. The window is a few microseconds against a `.cmd`
//! shim that takes milliseconds to reach `node`, and the alternative is
//! reimplementing process spawning. It is written down here because an
//! unmentioned race is one nobody can weigh.

#![cfg(windows)]
// The workspace forbids unsafe code and this crate denies it; this module is the
// single exemption, because §8.5's requirement has no safe expression. Every
// `unsafe` block below is a Win32 call with a SAFETY comment naming the invariant
// it relies on. `unsafe_is_confined_to_the_job_object` asserts this stays the only
// file with this attribute.
#![allow(unsafe_code)]

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// The exit code a job kill reports for its members.
///
/// `1` rather than `0`: these processes were killed, and a zero exit would say
/// they finished successfully. Something reading the code later — a fallback
/// ladder deciding whether output is trustworthy — would draw the wrong
/// conclusion from a lie this cheap to avoid.
pub const JOB_KILL_EXIT_CODE: u32 = 1;

/// A job object that kills everything in it when it is dropped.
///
/// Owns the handle. Not `Clone`, because two owners would mean two `CloseHandle`
/// calls and the second is a use-after-free.
#[derive(Debug)]
pub struct JobObject {
    handle: HANDLE,
}

// A raw HANDLE is a pointer-sized integer with no thread affinity; the Win32 job
// APIs are documented as callable from any thread. `HANDLE` is not `Send` only
// because it is a raw pointer type.
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}

impl JobObject {
    /// Create an unnamed job whose members die when the last handle closes.
    ///
    /// Unnamed deliberately: a named job could be opened by any process on the
    /// machine that guessed the name, and the name buys nothing here — the handle
    /// is passed to children by assignment, never by lookup.
    pub fn new() -> io::Result<Self> {
        // SAFETY: both arguments are null, which `CreateJobObjectW` documents as
        // "default security attributes, unnamed". The returned handle is checked
        // before use and owned by the returned value from here on.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let job = Self { handle };
        job.set_kill_on_close()?;
        Ok(job)
    }

    /// Ask Windows to kill the job's members when its last handle closes.
    fn set_kill_on_close(&self) -> io::Result<()> {
        // SAFETY: `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is a C struct of
        // integers and nested C structs of integers — no references, no
        // enums with invalid bit patterns, no `NonNull` — so all-zero is a valid
        // value of the type. It is also the documented starting point: every
        // field means "no limit" until a bit in `LimitFlags` says otherwise.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        // SAFETY: `self.handle` is a live job handle; the pointer and length
        // describe a `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` that outlives the
        // call, which is what `JobObjectExtendedLimitInformation` expects.
        let ok = unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .unwrap_or(0),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Put a child, and everything it goes on to spawn, in this job.
    ///
    /// Takes the `Child` rather than a raw handle, and is safe rather than
    /// `unsafe`, because the borrow is what makes it so: a borrowed `Child` has
    /// not been dropped or waited on, so its handle is open for the whole call.
    /// Handing the caller an `unsafe fn` taking a `HANDLE` would export that
    /// obligation to a file that is not allowed to reason about it — this module
    /// is the workspace's single `allow(unsafe_code)`, and a precondition that
    /// escapes it defeats the point of confining it.
    ///
    /// A child with no handle has already exited; there is nothing to assign and
    /// nothing to kill, so that is `Ok`.
    pub fn assign(&self, child: &tokio::process::Child) -> io::Result<()> {
        let Some(handle) = child.raw_handle() else {
            return Ok(());
        };

        // SAFETY: `handle` belongs to a child borrowed for this call, so it is
        // open throughout; `self.handle` is a live job handle for as long as
        // `self` is. The handle carries PROCESS_SET_QUOTA and PROCESS_TERMINATE
        // because this process created it, which is what the call requires.
        let ok = unsafe { AssignProcessToJobObject(self.handle, handle as HANDLE) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Kill every process in the job, now.
    ///
    /// This is the whole point of the type. It reaches grandchildren, which is
    /// what `TerminateProcess` on the direct child cannot do.
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: `self.handle` is a live job handle for as long as `self` is.
        let ok = unsafe { TerminateJobObject(self.handle, JOB_KILL_EXIT_CODE) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        // Closing the last handle is what triggers `KILL_ON_JOB_CLOSE`, so this is
        // not merely tidying up — it is the safety net for the path where
        // rev-local dies without terminating anything.
        //
        // The result is discarded because `drop` cannot report and there is
        // nothing to do about a failure here: the handle is being abandoned
        // either way, and the process is on its way out.
        //
        // SAFETY: `self.handle` was checked at construction and is closed exactly
        // once, here, because `JobObject` is neither `Clone` nor `Copy`.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
