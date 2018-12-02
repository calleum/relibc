use alloc::vec::Vec;
use core::ptr;

use header::{stdio, stdlib};
use platform;
use platform::types::*;

#[repr(C)]
pub struct Stack {
    argc: isize,
    argv0: *const c_char,
}

impl Stack {
    fn argc(&self) -> isize {
        self.argc
    }

    fn argv(&self) -> *const *const c_char {
        &self.argv0 as *const _
    }

    fn envp(&self) -> *const *const c_char {
        unsafe { self.argv().offset(self.argc() + 1) }
    }
}

#[inline(never)]
#[no_mangle]
pub unsafe extern "C" fn relibc_start(sp: &'static Stack) -> ! {
    extern "C" {
        static __preinit_array_start: extern "C" fn();
        static __preinit_array_end: extern "C" fn();
        static __init_array_start: extern "C" fn();
        static __init_array_end: extern "C" fn();

        fn _init();
        fn main(argc: isize, argv: *const *const c_char, envp: *const *const c_char) -> c_int;
    }

    let argc = sp.argc();
    let argv = sp.argv();

    let envp = sp.envp();
    let mut len = 0;
    while ! (*envp.add(len)).is_null() {
        len += 1;
    }
    platform::inner_environ = Vec::with_capacity(len + 1);
    for i in 0..len {
        let mut item = *envp.add(i);
        let mut len = 0;
        while *item.add(len) != 0 {
            len += 1;
        }

        let buf = platform::alloc(len + 1) as *mut c_char;
        for i in 0..=len {
            *buf.add(i) = *item.add(i);
        }
        platform::inner_environ.push(buf);
    }
    platform::inner_environ.push(ptr::null_mut());
    platform::environ = platform::inner_environ.as_mut_ptr();

    // Initialize stdin/stdout/stderr, see https://github.com/rust-lang/rust/issues/51718
    stdio::stdin = stdio::default_stdin.get();
    stdio::stdout = stdio::default_stdout.get();
    stdio::stderr = stdio::default_stderr.get();

    _init();

    // Look for the neighbor functions in memory until the end
    let mut f = &__preinit_array_start as *const _;
    while f < &__preinit_array_end {
        (*f)();
        f = f.offset(1);
    }
    f = &__init_array_start as *const _;
    while f < &__init_array_end {
        (*f)();
        f = f.offset(1);
    }

    stdlib::exit(main(
        argc,
        argv,
        // not envp, because programs like bash try to modify this *const*
        // pointer :|
        platform::environ as *const *const c_char,
    ));

    unreachable!();
}
