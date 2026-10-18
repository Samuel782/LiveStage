//! Real-time scheduling for the show thread.
//!
//! * **macOS**: Mach time-constraint policy (the class CoreAudio uses), with
//!   a 1 ms period, ~150 µs computation and 500 µs constraint. Works
//!   unprivileged.
//! * **Linux**: `SCHED_FIFO` priority 40 (needs `CAP_SYS_NICE` or an
//!   rtprio limit), falling back to nice -10, then to normal.
//! * **Windows**: `THREAD_PRIORITY_TIME_CRITICAL`.
//!
//! Failures are not fatal: the thread keeps running at normal priority and
//! the outcome is reported in the status bar.

use std::time::Duration;

/// Applies real-time scheduling to the calling thread. Returns a short
/// description of the class actually obtained.
pub fn make_current_thread_realtime(period: Duration) -> String {
    imp::apply(period)
}

#[cfg(target_os = "macos")]
mod imp {
    use mach2::kern_return::KERN_SUCCESS;
    use mach2::mach_time::{mach_timebase_info, mach_timebase_info_data_t};
    use mach2::thread_policy::{
        thread_policy_set, thread_time_constraint_policy, THREAD_TIME_CONSTRAINT_POLICY,
        THREAD_TIME_CONSTRAINT_POLICY_COUNT,
    };
    use std::time::Duration;

    pub fn apply(period: Duration) -> String {
        let mut tb = mach_timebase_info_data_t::default();
        if unsafe { mach_timebase_info(&mut tb) } != KERN_SUCCESS || tb.numer == 0 {
            return "normal (timebase unavailable)".into();
        }
        let abs = |d: Duration| (d.as_nanos() as u64 * tb.denom as u64 / tb.numer as u64).min(u32::MAX as u64) as u32;
        let mut policy = thread_time_constraint_policy {
            period: abs(period),
            computation: abs(period.mul_f32(0.15)),
            constraint: abs(period / 2),
            preemptible: 1,
        };
        let ret = unsafe {
            let thread = mach2::mach_init::mach_thread_self();
            let r = thread_policy_set(
                thread,
                THREAD_TIME_CONSTRAINT_POLICY,
                // thread_policy_t is `*mut integer_t`.
                &mut policy as *mut thread_time_constraint_policy as *mut _,
                THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            );
            mach2::mach_port::mach_port_deallocate(mach2::traps::mach_task_self(), thread);
            r
        };
        if ret == KERN_SUCCESS {
            "real-time (Mach time-constraint)".into()
        } else {
            format!("normal (thread_policy_set failed: {ret})")
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::time::Duration;

    pub fn apply(_period: Duration) -> String {
        unsafe {
            let param = libc::sched_param { sched_priority: 40 };
            if libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) == 0 {
                return "real-time (SCHED_FIFO 40)".into();
            }
            if libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, -10) == 0 {
                return "elevated (nice -10; grant CAP_SYS_NICE for SCHED_FIFO)".into();
            }
        }
        "normal (no permission for real-time scheduling)".into()
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::time::Duration;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> *mut core::ffi::c_void;
        fn SetThreadPriority(thread: *mut core::ffi::c_void, priority: i32) -> i32;
    }
    const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;

    pub fn apply(_period: Duration) -> String {
        if unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) } != 0 {
            "real-time (TIME_CRITICAL)".into()
        } else {
            "normal (SetThreadPriority failed)".into()
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
    pub fn apply(_period: std::time::Duration) -> String {
        "normal (unsupported platform)".into()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn applies_on_a_scratch_thread() {
        let got = std::thread::spawn(|| super::make_current_thread_realtime(std::time::Duration::from_millis(1)))
            .join()
            .unwrap();
        assert!(!got.is_empty());
        #[cfg(target_os = "macos")]
        assert!(got.starts_with("real-time"), "{got}");
    }
}
