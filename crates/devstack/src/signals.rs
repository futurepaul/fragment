//! Termination signals (a terminal's Ctrl-C, SIGTERM, a closed terminal's
//! SIGHUP), turned into work a thread does: a signal handler may do almost
//! nothing safely, so the one installed here writes the signal's number
//! to a pipe (write(2) is async-signal-safe), and a thread of the
//! process's own reads it and does what the process must before it ends.

use std::fs::File;
use std::io::Read;
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicI32, Ordering};

use anyhow::{ensure, Result};

/// The signals that end a run: Ctrl-C (SIGINT), SIGTERM, SIGHUP.
const TERMINATION: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// The pipe's write end, for the handler; -1 until `on_termination`.
static PIPE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn record(signal: libc::c_int) {
    let byte = u8::try_from(signal).unwrap_or(u8::MAX);
    // SAFETY: write(2) on a pipe this process opened, of one byte it owns:
    // async-signal-safe, which is all a handler may call
    unsafe { libc::write(PIPE.load(Ordering::Relaxed), (&raw const byte).cast(), 1) };
}

/// A signal's disposition now: whether this process was started with it
/// ignored (a background job's SIGINT, nohup's SIGHUP), which it keeps.
fn ignored(signal: libc::c_int) -> bool {
    // SAFETY: a zeroed sigaction is a valid out-parameter, and a null new
    // action only reads the current one
    let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
    let read = unsafe { libc::sigaction(signal, std::ptr::null(), &mut now) };
    assert_eq!(read, 0, "sigaction reads signal {signal}'s disposition");
    now.sa_sigaction == libc::SIG_IGN
}

/// Runs `then` with the signal's number on a thread of its own when the
/// process gets one of the termination signals it does not ignore, then
/// exits as that signal would have it (128 + its number). A second signal
/// meanwhile ends the process at once: the handlers are reset first.
/// Once per process.
pub fn on_termination(then: impl FnOnce(i32) + Send + 'static) -> Result<()> {
    let mut fds = [-1 as libc::c_int; 2];
    // SAFETY: pipe(2) writes two descriptors into the array it is given
    ensure!(unsafe { libc::pipe(fds.as_mut_ptr()) } == 0, "pipe: {}", std::io::Error::last_os_error());
    for fd in fds {
        // the processes it starts (wrangler, Chrome) inherit neither end
        // SAFETY: fcntl(2) on a descriptor just opened here
        ensure!(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0, "fcntl: {}", std::io::Error::last_os_error());
    }
    ensure!(PIPE.compare_exchange(-1, fds[1], Ordering::SeqCst, Ordering::SeqCst).is_ok(), "termination is handled once per process");
    // SAFETY: the read end is this process's, and owned by the File alone
    let mut read = unsafe { File::from_raw_fd(fds[0]) };
    std::thread::Builder::new().name("termination".into()).spawn(move || {
        let mut byte = [0u8; 1];
        // the pipe's write end is never closed: this returns on a signal
        if read.read_exact(&mut byte).is_err() {
            return;
        }
        for signal in TERMINATION {
            // SAFETY: restores the default action
            unsafe { libc::signal(signal, libc::SIG_DFL) };
        }
        let signal = i32::from(byte[0]);
        then(signal);
        std::process::exit(128 + signal);
    })?;
    for signal in TERMINATION {
        if ignored(signal) {
            continue;
        }
        // SAFETY: a zeroed sigaction with an empty mask, SA_RESTART (a
        // system call it interrupts carries on), and a handler that only
        // writes one byte to a pipe
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = record as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        ensure!(unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } == 0, "sigaction {signal}: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

/// This process ignores Ctrl-C from here on, so that it outlives a node in
/// the terminal's process group (which the same Ctrl-C stops) to clean up
/// after it. A process it starts afterwards inherits the ignore: call it
/// once the node has started.
pub fn outlive_interrupt() -> Result<()> {
    // SAFETY: SIG_IGN for SIGINT
    let previous = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    ensure!(previous != libc::SIG_ERR, "ignore SIGINT: {}", std::io::Error::last_os_error());
    Ok(())
}
