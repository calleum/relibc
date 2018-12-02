//! assert implementation for Redox, following http://pubs.opengroup.org/onlinepubs/7908799/xsh/assert.h.html

use c_str::CStr;
use core::fmt::Write;
use header::{stdio, stdlib};
use platform::types::*;

#[no_mangle]
pub unsafe extern "C" fn __assert(
    func: *const c_char,
    file: *const c_char,
    line: c_int,
    cond: *const c_char,
) {
    let func = CStr::from_ptr(func).to_str().unwrap();
    let file = CStr::from_ptr(file).to_str().unwrap();
    let cond = CStr::from_ptr(cond).to_str().unwrap();

    writeln!(
        *stdio::stderr,
        "{}: {}:{}: Assertion `{}` failed.",
        func,
        file,
        line,
        cond
    )
    .unwrap();
    stdlib::abort();
}
