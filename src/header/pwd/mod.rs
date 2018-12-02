//! pwd implementation for relibc

use core::ptr;

use fs::File;
use header::{errno, fcntl};
use io::{BufRead, BufReader};
use platform;
use platform::types::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "redox")]
mod redox;

#[cfg(target_os = "linux")]
use self::linux as sys;
#[cfg(target_os = "redox")]
use self::redox as sys;

#[repr(C)]
pub struct passwd {
    pw_name: *mut c_char,
    pw_passwd: *mut c_char,
    pw_uid: uid_t,
    pw_gid: gid_t,
    pw_gecos: *mut c_char,
    pw_dir: *mut c_char,
    pw_shell: *mut c_char,
}

static mut PASSWD_BUF: *mut c_char = ptr::null_mut();
static mut PASSWD: passwd = passwd {
    pw_name: ptr::null_mut(),
    pw_passwd: ptr::null_mut(),
    pw_uid: 0,
    pw_gid: 0,
    pw_gecos: ptr::null_mut(),
    pw_dir: ptr::null_mut(),
    pw_shell: ptr::null_mut(),
};

enum OptionPasswd {
    Error,
    NotFound,
    Found(*mut c_char),
}

fn pwd_lookup<F>(
    out: *mut passwd,
    alloc: Option<(*mut c_char, size_t)>,
    mut callback: F,
) -> OptionPasswd
where
    // TODO F: FnMut(impl Iterator<Item = &[u8]>) -> bool
    F: FnMut(&[&[u8]]) -> bool,
{
    let file = match File::open(c_str!("/etc/passwd"), fcntl::O_RDONLY) {
        Ok(file) => file,
        Err(_) => return OptionPasswd::Error,
    };

    let file = BufReader::new(file);

    for line in file.split(b'\n') {
        let line = match line {
            Ok(line) => line,
            Err(err) => unsafe {
                platform::errno = errno::EIO;
                return OptionPasswd::Error;
            },
        };

        // Parse into passwd
        let mut parts: [&[u8]; 7] = sys::split(&line);

        if !callback(&parts) {
            continue;
        }

        let len = parts
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 2 && *i != 3)
            .map(|(_, part)| part.len() + 1)
            .sum();

        if alloc.map(|(_, s)| len > s as usize).unwrap_or(false) {
            unsafe {
                platform::errno = errno::ERANGE;
            }
            return OptionPasswd::Error;
        }

        let alloc = match alloc {
            Some((alloc, _)) => alloc,
            None => unsafe { platform::alloc(len) as *mut c_char },
        };
        // _ prefix so it won't complain about the trailing
        // _off += <thing>
        // in the macro that is never read
        let mut _off = 0;

        let mut parts = parts.iter();

        macro_rules! copy_into {
            ($entry:expr) => {
                debug_assert!(_off as usize <= len);

                let src = parts.next().unwrap_or(&(&[] as &[u8])); // this is madness
                let dst = unsafe { alloc.offset(_off) };

                for (i, c) in src.iter().enumerate() {
                    unsafe {
                        *dst.add(i) = *c as c_char;
                    }
                }
                unsafe {
                    *dst.add(src.len()) = 0;

                    $entry = dst;
                }
                _off += src.len() as isize + 1;
            };
            ($entry:expr,parse) => {
                unsafe {
                    $entry = parts
                        .next()
                        .and_then(|part| core::str::from_utf8(part).ok())
                        .and_then(|part| part.parse().ok())
                        .unwrap_or(0);
                }
            };
        }

        copy_into!((*out).pw_name);
        copy_into!((*out).pw_passwd);
        copy_into!((*out).pw_uid, parse);
        copy_into!((*out).pw_gid, parse);
        copy_into!((*out).pw_gecos);
        copy_into!((*out).pw_dir);
        copy_into!((*out).pw_shell);

        return OptionPasswd::Found(alloc);
    }
    OptionPasswd::NotFound
}

#[no_mangle]
pub unsafe extern "C" fn getpwnam_r(
    name: *const c_char,
    out: *mut passwd,
    buf: *mut c_char,
    size: size_t,
    result: *mut *mut passwd,
) -> c_int {
    match pwd_lookup(out, Some((buf, size)), |parts| {
        let part = parts.get(0).unwrap_or(&(&[] as &[u8]));
        for (i, c) in part.iter().enumerate() {
            // /etc/passwd should not contain any NUL bytes in the middle
            // of entries, but if this happens, it can't possibly match the
            // search query since it's NUL terminated.
            if *c == 0 || *name.add(i) != *c as c_char {
                return false;
            }
        }
        true
    }) {
        OptionPasswd::Error => {
            *result = ptr::null_mut();
            -1
        },
        OptionPasswd::NotFound => {
            *result = ptr::null_mut();
            0
        },
        OptionPasswd::Found(_) => {
            *result = out;
            0
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn getpwuid_r(
    uid: uid_t,
    out: *mut passwd,
    buf: *mut c_char,
    size: size_t,
    result: *mut *mut passwd,
) -> c_int {
    match pwd_lookup(out, Some((buf, size)), |parts| {
        let part = parts
            .get(2)
            .and_then(|part| core::str::from_utf8(part).ok())
            .and_then(|part| part.parse().ok());
        part == Some(uid)
    }) {
        OptionPasswd::Error => {
            *result = ptr::null_mut();
            -1
        },
        OptionPasswd::NotFound => {
            *result = ptr::null_mut();
            0
        },
        OptionPasswd::Found(_) => {
            *result = out;
            0
        },
    }
}

#[no_mangle]
pub extern "C" fn getpwnam(name: *const c_char) -> *mut passwd {
    match pwd_lookup(unsafe { &mut PASSWD }, None, |parts| {
        let part = parts.get(0).unwrap_or(&(&[] as &[u8]));
        for (i, c) in part.iter().enumerate() {
            // /etc/passwd should not contain any NUL bytes in the middle
            // of entries, but if this happens, it can't possibly match the
            // search query since it's NUL terminated.
            if *c == 0 || unsafe { *name.add(i) } != *c as c_char {
                return false;
            }
        }
        true
    }) {
        OptionPasswd::Error => ptr::null_mut(),
        OptionPasswd::NotFound => ptr::null_mut(),
        OptionPasswd::Found(buf) => unsafe {
            PASSWD_BUF = buf;
            &mut PASSWD
        },
    }
}

#[no_mangle]
pub extern "C" fn getpwuid(uid: uid_t) -> *mut passwd {
    match pwd_lookup(unsafe { &mut PASSWD }, None, |parts| {
        let part = parts
            .get(2)
            .and_then(|part| core::str::from_utf8(part).ok())
            .and_then(|part| part.parse().ok());
        part == Some(uid)
    }) {
        OptionPasswd::Error => ptr::null_mut(),
        OptionPasswd::NotFound => ptr::null_mut(),
        OptionPasswd::Found(buf) => unsafe {
            if !PASSWD_BUF.is_null() {
                platform::free(PASSWD_BUF as *mut c_void);
            }
            PASSWD_BUF = buf;
            &mut PASSWD
        },
    }
}
