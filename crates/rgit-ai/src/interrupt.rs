use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the SIGINT and SIGTERM handler while a [`Guard`] is alive.
pub(crate) static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Catches SIGINT and SIGTERM for as long as it lives, then restores the old handlers.
#[cfg(unix)]
pub(crate) struct Guard {
    old: [libc::sigaction; 2],
}

#[cfg(unix)]
const SIGNALS: [libc::c_int; 2] = [libc::SIGINT, libc::SIGTERM];

#[cfg(unix)]
extern "C" fn on_signal(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
impl Guard {
    pub(crate) fn install() -> Guard {
        INTERRUPTED.store(false, Ordering::SeqCst);
        let mut old: [libc::sigaction; 2] = unsafe { std::mem::zeroed() };
        for (slot, signal) in old.iter_mut().zip(SIGNALS) {
            // SAFETY: plain sigaction calls with a handler that only sets an atomic.
            unsafe {
                let mut new: libc::sigaction = std::mem::zeroed();
                new.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
                libc::sigemptyset(&mut new.sa_mask);
                libc::sigaction(signal, &new, slot);
            }
        }
        Guard { old }
    }
}

#[cfg(unix)]
impl Drop for Guard {
    fn drop(&mut self) {
        for (slot, signal) in self.old.iter().zip(SIGNALS) {
            // SAFETY: restores the handler saved in `install`.
            unsafe { libc::sigaction(signal, slot, std::ptr::null_mut()) };
        }
    }
}

#[cfg(not(unix))]
pub(crate) struct Guard;

#[cfg(not(unix))]
impl Guard {
    pub(crate) fn install() -> Guard {
        Guard
    }
}
