//  Copyright 2020 Two Sigma Investments, LP.
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.

use anyhow::{Result, Context};
use std::{
    io::Result as IoResult,
    io::Error as IoError,
    os::unix::io::AsRawFd,
    ffi::{OsString, OsStr},
    collections::HashMap,
    process::Command as StdCommand,
    os::unix::process::CommandExt,
};
use nix::{
    fcntl::{fcntl, FcntlArg, FdFlag, OFlag},
    unistd::setsid,
};
use crate::util::Pipe;
use super::Process;

// We re-export these, as they are part of our API
pub use std::process::{
    ExitStatus, Stdio, ChildStdin, ChildStdout, ChildStderr, Output
};

pub type EnvVars = HashMap<OsString, OsString>;

// We wrap the standard library `Command` to provide additional features:
// * Logging of the command executed, and failures
// * set_pgrp()
// We have to delegate a few methods to the inner `StdCommand`, which makes it a bit verbose.
// We considered the subprocess crate, but it wasn't very useful, and it lacked
// the crucial feature of pre_exec() that the standard library has for doing setsid().

pub struct Command {
    inner: StdCommand,
    display_args: Vec<String>,
    show_cmd_on_spawn: bool,
}

impl Command {
    pub fn new<I: IntoIterator<Item = S>, S: AsRef<OsStr>>(args: I) -> Self {
        let mut args = args.into_iter();
        let program = args.next().unwrap(); // unwrap() is fine as we never pass empty args
        let mut cmd = Self {
            inner: StdCommand::new(&program),
            display_args: vec![Self::arg_for_display(&program)],
            show_cmd_on_spawn: true,
        };
        cmd.args(args);
        cmd
    }

    pub fn new_shell<S: AsRef<OsStr>>(script: S) -> Self {
        // We use bash for pipefail support
        let mut inner = StdCommand::new("/bin/bash");
        inner.arg("-o").arg("pipefail")
             .arg("-c").arg(&script);
        Self {
            inner,
            display_args: vec![Self::arg_for_display(&script)],
            show_cmd_on_spawn: true,
        }
    }

    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.display_args.push(Self::arg_for_display(&arg));
        self.inner.arg(&arg);
        self
    }

    pub fn arg_for_display<S: AsRef<OsStr>>(arg: S) -> String {
        arg.as_ref().to_string_lossy().into_owned()
    }

    pub fn args<I: IntoIterator<Item = S>, S: AsRef<OsStr>>(&mut self, args: I) -> &mut Self {
        for arg in args { self.arg(arg); }
        self
    }

    pub fn setsid(&mut self) -> &mut Self {
        unsafe {
            self.pre_exec(|| match setsid() {
                Err(e) => {
                    error!("Failed to setuid(): {}", e);
                    // Only errno is propagated back to the parent
                    Err(IoError::last_os_error())
                },
                Ok(_) => Ok(()),
            })
        }
    }

    pub fn show_cmd_on_spawn(&mut self, value: bool) -> &mut Self {
        self.show_cmd_on_spawn = value;
        self
    }

    pub fn spawn(&mut self) -> Result<Process> {
        let display_cmd = self.display_args.join(" ");
        let inner = self.inner.spawn()
            .with_context(|| format!("Failed to spawn `{}`", display_cmd))?;
        if self.show_cmd_on_spawn {
            debug!("+ {}", display_cmd);
        }
        Ok(Process::new(inner, display_cmd))
    }

    pub fn exec(&mut self) -> Result<()> {
        bail!(self.inner.exec())
    }
}

// These are delegates to the inner `StdCommand`.
impl Command {
    pub fn env<K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, key: K, val: V) -> &mut Command
        { self.inner.env(key, val); self }
    pub fn envs<I: IntoIterator<Item = (K, V)>, K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, vars: I) -> &mut Command
        { self.inner.envs(vars); self }
    pub fn env_remove<K: AsRef<OsStr>>(&mut self, key: K) -> &mut Command
        { self.inner.env_remove(key); self }
    pub fn env_clear(&mut self) -> &mut Command
        { self.inner.env_clear(); self }
    pub fn stdin<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Command
        { self.inner.stdin(cfg); self }
    pub fn stdout<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Command
        { self.inner.stdout(cfg); self }
    pub fn stderr<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Command
        { self.inner.stderr(cfg); self }
    pub unsafe fn pre_exec<F>(&mut self, f: F) -> &mut Command
        where
        F: FnMut() -> IoResult<()> + Send + Sync + 'static
        { self.inner.pre_exec(f); self }
}

pub trait PipeCommandExt: Sized {
    /// Create a new pipe input (e.g., stdin).
    fn new_input() -> Result<Self>;
    /// Create a new pipe output (e.g., stdout, stderr)
    fn new_output() -> Result<Self>;
}

impl PipeCommandExt for Pipe {
    fn new_input() -> Result<Self> {
        let pipe = Self::new(OFlag::empty())?;
        fcntl(pipe.write.as_raw_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        Ok(pipe)
    }

    fn new_output() -> Result<Self> {
        let pipe = Self::new(OFlag::empty())?;
        fcntl(pipe.read.as_raw_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        Ok(pipe)
    }
}
