//! Signal handling with no extra crate: the C library is already linked, so `signal(2)`
//! is declared directly. SIGTERM/SIGINT only set a flag; the event loop notices it and
//! leaves through the same restore path as `q`.
//!
//! SIGKILL cannot be caught by any process; after a `kill -9` the shell has to `reset`.

#[cfg(unix)]
mod unix {
    use std::sync::atomic::{AtomicBool, Ordering};

    static TERMINATED: AtomicBool = AtomicBool::new(false);

    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;

    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }

    extern "C" fn on_signal(_signum: i32) {
        // Async-signal-safe: a plain atomic store, nothing else.
        TERMINATED.store(true, Ordering::SeqCst);
    }

    pub fn install() {
        // SAFETY: `signal` is called with the two valid signal numbers and a function
        // pointer matching the C `void (*)(int)` handler ABI; the return value is ignored.
        unsafe {
            signal(SIGINT, on_signal as *const () as usize);
            signal(SIGTERM, on_signal as *const () as usize);
        }
    }

    pub fn terminated() -> bool {
        TERMINATED.load(Ordering::SeqCst)
    }
}

#[cfg(not(unix))]
mod unix {
    pub fn install() {}
    pub fn terminated() -> bool {
        false
    }
}

pub use unix::{install, terminated};
