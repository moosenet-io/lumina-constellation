//! HARDEN-02: Process hardening — core dumps, memory locking, ptrace restriction
//!
//! Call `hardening::init()` as the very first action in main(), before vault
//! decryption or any secret handling. Linux-only via cfg guards; no-op elsewhere.

use crate::error::Result;

/// Harden the process against memory forensics attacks.
///
/// 1. Disable core dumps (RLIMIT_CORE = 0) — prevents crash dump files from
///    capturing conversation content.
/// 2. Set PR_SET_DUMPABLE = 0 — prevents /proc/pid/mem reads by non-root.
/// 3. Attempt mlockall(MCL_CURRENT | MCL_FUTURE) — locks pages to prevent
///    conversation data from being paged to swap. Requires CAP_IPC_LOCK; failure
///    is logged as a warning, not an error.
pub fn init() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        disable_core_dumps();
        set_non_dumpable();
        attempt_memory_lock();
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn disable_core_dumps() {
    use libc::{rlimit, setrlimit, RLIMIT_CORE};
    let zero = rlimit { rlim_cur: 0, rlim_max: 0 };
    let ret = unsafe { setrlimit(RLIMIT_CORE, &zero) };
    if ret == 0 {
        eprintln!("lumina-core: core dumps disabled");
    } else {
        eprintln!("lumina-core: warning — could not disable core dumps (errno {})", ret);
    }
}

#[cfg(target_os = "linux")]
fn set_non_dumpable() {
    use libc::{prctl, PR_SET_DUMPABLE};
    let ret = unsafe { prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) };
    if ret == 0 {
        eprintln!("lumina-core: process marked non-dumpable");
    } else {
        eprintln!("lumina-core: warning — could not set non-dumpable flag (errno {})", ret);
    }
}

#[cfg(target_os = "linux")]
fn attempt_memory_lock() {
    use libc::{mlockall, MCL_CURRENT, MCL_FUTURE};
    let ret = unsafe { mlockall(MCL_CURRENT | MCL_FUTURE) };
    if ret == 0 {
        eprintln!("lumina-core: memory locked (swap exposure prevented)");
    } else {
        eprintln!(
            "lumina-core: warning — could not lock memory — swap exposure possible. \
             Run with CAP_IPC_LOCK for full protection."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_succeeds() {
        // Must not panic or return Err regardless of privilege level
        assert!(init().is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_core_dump_rlimit_is_zero() {
        use libc::{getrlimit, rlimit, RLIMIT_CORE};
        disable_core_dumps();
        let mut rl = rlimit { rlim_cur: 1, rlim_max: 1 };
        unsafe { getrlimit(RLIMIT_CORE, &mut rl) };
        assert_eq!(rl.rlim_cur, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_non_dumpable_flag_set() {
        use libc::{prctl, PR_GET_DUMPABLE};
        set_non_dumpable();
        let dumpable = unsafe { prctl(PR_GET_DUMPABLE, 0, 0, 0, 0) };
        assert_eq!(dumpable, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_mlock_failure_is_not_fatal() {
        // mlockall may fail without CAP_IPC_LOCK — must not panic or return Err
        attempt_memory_lock();
    }
}
