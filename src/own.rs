//! Ownership safety net: every spawned child's process group is registered
//! here and SIGKILLed on ANY exit — normal end, panic (Drop guard), SIGINT or
//! SIGTERM (signal handler). If probatum dies, nothing it started survives.

use std::sync::atomic::{AtomicI32, Ordering};

// ponytail: fixed 64 slots, lock-free — a Mutex is not async-signal-safe,
// atomics are. 64 concurrent children per run is far above any real config.
const MAX: usize = 64;
#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicI32 = AtomicI32::new(0);
static PGIDS: [AtomicI32; MAX] = [ZERO; MAX];

/// Register a child's process group (spawned with `process_group(0)`, so pgid == pid).
pub fn register(pid: u32) {
    for slot in PGIDS.iter() {
        if slot
            .compare_exchange(0, pid as i32, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return;
        }
    }
    // Over MAX children the overflow only gets the runner's normal teardown.
}

/// SIGKILL every registered process group. Only raw syscalls: non-panicking,
/// async-signal-safe, harmless on already-dead groups (ESRCH).
pub fn kill_all() {
    for slot in PGIDS.iter() {
        let pg = slot.load(Ordering::SeqCst);
        if pg > 0 {
            unsafe {
                libc::kill(-pg, libc::SIGKILL); // negative pid = whole group
            }
        }
    }
}

/// Drop guard: lives on the run's stack frame, so panic unwinding runs it.
pub struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        kill_all();
    }
}

extern "C" fn on_signal(_sig: libc::c_int) {
    kill_all();
    unsafe { libc::_exit(130) };
}

/// Ctrl-C / kill must not leave orphans either. Services run in their own
/// process groups, so a terminal SIGINT never reaches them natively — we must
/// forward the kill ourselves.
pub fn install_signal_handlers() {
    let h = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, h);
        libc::signal(libc::SIGTERM, h);
    }
}
