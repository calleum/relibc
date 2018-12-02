use core::mem;

use super::super::types::*;
use super::super::PalSignal;
use super::{e, Sys};
use header::signal::{sigaction, sigset_t};

impl Sys {
    fn sigprocmask(how: c_int, set: *const sigset_t, oset: *mut sigset_t) -> c_int {
        e(unsafe { syscall!(RT_SIGPROCMASK, how, set, oset, mem::size_of::<sigset_t>()) }) as c_int
    }
}

impl PalSignal for Sys {
    fn kill(pid: pid_t, sig: c_int) -> c_int {
        e(unsafe { syscall!(KILL, pid, sig) }) as c_int
    }

    fn killpg(pgrp: pid_t, sig: c_int) -> c_int {
        e(unsafe { syscall!(KILL, -(pgrp as isize) as pid_t, sig) }) as c_int
    }

    fn raise(sig: c_int) -> c_int {
        let tid = e(unsafe { syscall!(GETTID) }) as pid_t;
        if tid == !0 {
            -1
        } else {
            e(unsafe { syscall!(TKILL, tid, sig) }) as c_int
        }
    }

    unsafe fn sigaction(sig: c_int, act: *const sigaction, oact: *mut sigaction) -> c_int {
        e(syscall!(
            RT_SIGACTION,
            sig,
            act,
            oact,
            mem::size_of::<sigset_t>()
        )) as c_int
    }
}
