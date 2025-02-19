use crate::c_str::{CStr, CString};
use crate::core_io::{BufReader, prelude::*, SeekFrom};
use crate::fs::File;
use crate::header::{fcntl, string::strlen};
use crate::platform::{sys::{S_ISUID, S_ISGID}, types::*};

use syscall::data::Stat;
use syscall::flag::*;
use syscall::error::*;
use redox_exec::{FdGuard, ExtraInfo, FexecResult};

fn fexec_impl(file: File, path: &[u8], args: &[&[u8]], envs: &[&[u8]], total_args_envs_size: usize, extrainfo: &ExtraInfo, interp_override: Option<redox_exec::InterpOverride>) -> Result<usize> {
    let fd = *file;
    core::mem::forget(file);
    let image_file = FdGuard::new(fd as usize);

    let open_via_dup = FdGuard::new(syscall::open("thisproc:current/open_via_dup", 0)?);
    let memory = FdGuard::new(syscall::open("memory:", 0)?);

    let addrspace_selection_fd = match redox_exec::fexec_impl(image_file, open_via_dup, &memory, path, args.iter().rev(), envs.iter().rev(), total_args_envs_size, extrainfo, interp_override)? {
        FexecResult::Normal { addrspace_handle } => addrspace_handle,
        FexecResult::Interp { image_file, open_via_dup, path, interp_override: new_interp_override } => {
            drop(image_file);
            drop(open_via_dup);
            drop(memory);

            // According to elf(5), PT_INTERP requires that the interpreter path be
            // null-terminated. Violating this should therefore give the "format error" ENOEXEC.
            let path_cstr = CStr::from_bytes_with_nul(&path).map_err(|_| Error::new(ENOEXEC))?;

            return execve(path_cstr, ArgEnv::Parsed { total_args_envs_size, args, envs }, Some(new_interp_override));
        }
    };
    drop(memory);

    // Dropping this FD will cause the address space switch.
    drop(addrspace_selection_fd);

    unreachable!();
}
pub enum ArgEnv<'a> {
    C { argv: *const *mut c_char, envp: *const *mut c_char },
    Parsed { args: &'a [&'a [u8]], envs: &'a [&'a [u8]], total_args_envs_size: usize },
}
pub fn execve(path: &CStr, arg_env: ArgEnv, interp_override: Option<redox_exec::InterpOverride>) -> Result<usize> {
    // NOTE: We must omit O_CLOEXEC and close manually, otherwise it will be closed before we
    // have even read it!
    let mut image_file = File::open(path, O_RDONLY as c_int).map_err(|_| Error::new(ENOENT))?;

    // With execve now being implemented in userspace, we need to check ourselves that this
    // file is actually executable. While checking for read permission is unnecessary as the
    // scheme will not allow us to read otherwise, the execute bit is completely unenforced. We
    // have the permission to mmap executable memory and fill it with the program even if it is
    // unset, so the best we can do is check that nothing is executed by accident.
    //
    // TODO: At some point we might have capabilities limiting the ability to allocate
    // executable memory, and in that case we might use the `escalate:` scheme as we already do
    // when the binary needs setuid/setgid.

    let mut stat = Stat::default();
    syscall::fstat(*image_file as usize, &mut stat)?;
    let uid = syscall::getuid()?;
    let gid = syscall::getuid()?;

    let mode = if uid == stat.st_uid as usize {
        (stat.st_mode >> 3 * 2) & 0o7
    } else if gid == stat.st_gid as usize {
        (stat.st_mode >> 3 * 1) & 0o7
    } else {
        stat.st_mode & 0o7
    };

    if mode & 0o1 == 0o0 {
        return Err(Error::new(EPERM));
    }
    let wants_setugid = stat.st_mode & ((S_ISUID | S_ISGID) as u16) != 0;

    let cwd: Box<[u8]> = super::path::clone_cwd().unwrap_or_default().into();

    // Count arguments
    let mut len = 0;

    match arg_env {
        ArgEnv::C { argv, .. } => unsafe {
            while !(*argv.add(len)).is_null() {
                len += 1;
            }
        }
        ArgEnv::Parsed { args, .. } => len = args.len(),
    }

    let mut args: Vec<&[u8]> = Vec::with_capacity(len);

    // Read shebang (for example #!/bin/sh)
    let mut _interpreter_path = None;
    let is_interpreted = {
        let mut read = 0;
        let mut shebang = [0; 2];

        while read < 2 {
            match image_file.read(&mut shebang).map_err(|_| Error::new(ENOEXEC))? {
                0 => break,
                i => read += i,
            }
        }
        shebang == *b"#!"
    };
    // Since the fexec implementation is almost fully done in userspace, the kernel can no longer
    // set UID/GID accordingly, and this code checking for them before using interfaces to upgrade
    // UID/GID, can not be trusted. So we ask the `escalate:` scheme for help. Note that
    // `escalate:` can be deliberately excluded from the scheme namespace to deny privilege
    // escalation (such as su/sudo/doas) for untrusted processes.
    //
    // According to execve(2), Linux and most other UNIXes ignore setuid/setgid for interpreted
    // executables and thereby simply keep the privileges as is. For compatibility we do that
    // too.

    if is_interpreted {
        // TODO: Does this support prepending args to the interpreter? E.g.
        // #!/usr/bin/env python3

        // So, this file is interpreted.
        // Then, read the actual interpreter:
        let mut interpreter = Vec::new();
        BufReader::new(&mut image_file).read_until(b'\n', &mut interpreter).map_err(|_| Error::new(EIO))?;
        if interpreter.ends_with(&[b'\n']) {
            interpreter.pop().unwrap();
        }
        let cstring = CString::new(interpreter).map_err(|_| Error::new(ENOEXEC))?;
        image_file = File::open(&cstring, O_RDONLY as c_int).map_err(|_| Error::new(ENOENT))?;

        // Make sure path is kept alive long enough, and push it to the arguments
        _interpreter_path = Some(cstring);
        let path_ref = _interpreter_path.as_ref().unwrap();
        args.push(path_ref.as_bytes());
    } else {
        image_file.seek(SeekFrom::Start(0)).map_err(|_| Error::new(EIO))?;
    }

    let (total_args_envs_size, args, envs): (usize, Vec<_>, Vec<_>) = match arg_env {
        ArgEnv::C { mut argv, mut envp } => unsafe {
            let mut args_envs_size_without_nul = 0;

            // Arguments
            while !argv.read().is_null() {
                let arg = argv.read();

                let len = strlen(arg);
                args.push(core::slice::from_raw_parts(arg as *const u8, len));
                args_envs_size_without_nul += len;
                argv = argv.add(1);
            }

            // Environment variables
            let mut len = 0;
            while !envp.add(len).read().is_null() {
                len += 1;
            }

            let mut envs: Vec<&[u8]> = Vec::with_capacity(len);
            while !envp.read().is_null() {
                let env = envp.read();

                let len = strlen(env);
                envs.push(core::slice::from_raw_parts(env as *const u8, len));
                args_envs_size_without_nul += len;
                envp = envp.add(1);
            }
            (args_envs_size_without_nul + args.len() + envs.len(), args, envs)
        }
        ArgEnv::Parsed { args: new_args, envs, total_args_envs_size } => {
            let prev_size: usize = args.iter().map(|a| a.len()).sum();
            args.extend(new_args);
            (total_args_envs_size + prev_size, args, Vec::from(envs))
        }
    };


    // Close all O_CLOEXEC file descriptors. TODO: close_range?
    {
        // NOTE: This approach of implementing O_CLOEXEC will not work in multithreaded
        // scenarios. While execve() is undefined according to POSIX if there exist sibling
        // threads, it could still be allowed by keeping certain file descriptors and instead
        // set the active file table.
        let files_fd = File::new(syscall::open("thisproc:current/filetable", O_RDONLY)? as c_int);
        for line in BufReader::new(files_fd).lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let fd = match line.parse::<usize>() {
                Ok(f) => f,
                Err(_) => continue,
            };

            let flags = syscall::fcntl(fd, F_GETFD, 0)?;

            if flags & O_CLOEXEC == O_CLOEXEC {
                let _ = syscall::close(fd);
            }
        }
    }

    if !is_interpreted && wants_setugid {
        // Make sure the last file descriptor not covered by O_CLOEXEC is not leaked.
        drop(image_file);

        // We are now going to invoke `escalate:` rather than loading the program ourselves.
        let escalate_fd = FdGuard::new(syscall::open("escalate:", O_WRONLY)?);

        // First, we write the path.
        //
        // TODO: For improved security, use a hypothetical SYS_DUP_FORWARD syscall to give the
        // scheme our file descriptor. It can check through the kernel-overwritten stat.st_dev
        // field that it pertains to a "trusted" scheme (i.e. of at least the privilege the
        // new uid/gid has), although for now only root can open schemes. Passing a file
        // descriptor and not a path will allow escalated to run in a limited namespace.
        //
        // TODO: Plus, at this point fexecve is not implemented (but specified in
        // POSIX.1-2008), and to avoid bad syscalls such as fpath, passing a file descriptor
        // would be better.
        let _ = syscall::write(*escalate_fd, path.to_bytes());

        // Second, we write the flattened args and envs with NUL characters separating
        // individual items. This can be copied directly into the new executable's memory.
        let _ = syscall::write(*escalate_fd, &flatten_with_nul(args))?;
        let _ = syscall::write(*escalate_fd, &flatten_with_nul(envs))?;
        let _ = syscall::write(*escalate_fd, &cwd)?;

        // Closing will notify the scheme, and from that point we will no longer have control
        // over this process (unless it fails). We do this manually since drop cannot handle
        // errors.
        let fd = *escalate_fd as usize;
        core::mem::forget(escalate_fd);

        syscall::close(fd)?;

        unreachable!()
    } else {
        let extrainfo = ExtraInfo { cwd: Some(&cwd) };
        fexec_impl(image_file, path.to_bytes(), &args, &envs, total_args_envs_size, &extrainfo, interp_override)
    }
}
fn flatten_with_nul<T>(iter: impl IntoIterator<Item = T>) -> Box<[u8]> where T: AsRef<[u8]> {
    let mut vec = Vec::new();
    for item in iter {
        vec.extend(item.as_ref());
        vec.push(b'\0');
    }
    vec.into_boxed_slice()
}
