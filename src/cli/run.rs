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
    time::Duration,
    ffi::OsString,
    path::PathBuf,
    fs, collections::HashSet
};
use nix::{
    sys::signal::{self, kill, killpg, SigmaskHow, SigSet},
    sys::wait::{wait, WaitStatus},
    unistd::Pid,
};
use structopt::StructOpt;
use serde::{Serialize, Deserialize};
use signal::{pthread_sigmask, Signal};
use crate::{
    consts::*,
    store,
    virt,
    cli::ExitCode,
    image::{ManifestFetchResult, ImageManifest, shard},
    process::{Command, CommandPidExt, ProcessExt, ProcessGroup, Stdio,
              spawn_set_ns_last_pid_server, set_ns_last_pid, MIN_PID},
    metrics::with_metrics,
    filesystem::spawn_untar,
    image_streamer::{Stats, ImageStreamer},
    lock::with_checkpoint_restore_lock,
    criu,
};
use libc::c_int;
use virt::time::Nanos;


/// Run application. If a checkpoint image exists, the application is restored. Otherwise, the
/// application is run from scratch.
#[derive(StructOpt, PartialEq, Debug, Serialize)]
#[structopt(after_help("\
ENVS:
    FF_APP_PATH                 The PATH to use for the application
    FF_APP_LD_LIBRARY_PATH      The LD_LIBRARY_PATH to use for the application
    FF_APP_VIRT_CPUID_MASK      The CPUID mask to use. See libvirtcpuid documentation for more details
    FF_APP_INJECT_<VAR_NAME>    Additional environment variables to inject to the application and its children.
                                For example, FF_APP_INJECT_LD_PRELOAD=/opt/lib/libx.so
    FF_METRICS_RECORDER         When specified, FastFreeze invokes the specified program to report metrics.
                                The metrics are formatted in JSON and passed as first argument
    CRIU_OPTS                   Additional arguments to pass to CRIU, whitespace separated
    S3_CMD                      Command to access AWS S3. Defaults to 'aws s3'
    GS_CMD                      Command to access Google Storage. Defaults to 'gsutil'

EXIT CODES:
    171          A failure happned during restore, or while fetching the image manifest.
                 Retrying with --no-restore will avoid that failure
    170          A failure happened before the application was ready
    128+sig_nr   The application caught a fatal signal corresponding to `sig_nr`
    exit_code    The application exited with `exit_code`"
))]
pub struct Run {
    /// Image URL. S3, GCS and local filesystem are supported: {n}
    ///   * s3://bucket_name/image_path {n}
    ///   * gs://bucket_name/image_path {n}
    ///   * file:image_path
    // {n} means new line in the CLI's --help command
    #[structopt(long, name="url")]
    image_url: String,

    /// Application arguments, used when running the app from scratch.
    /// Ignored during restore.
    // Note: Type should be OsString, but structopt doesn't like it
    #[structopt()]
    app_args: Vec<String>,

    /// Shell command to run once the application is running.
    // Note: Type should be OsString, but structopt doesn't like it
    #[structopt(long="on-app-ready", name="cmd")]
    on_app_ready_cmd: Option<String>,

    /// Alawys run the app from scratch. Useful to ignore a faulty image.
    #[structopt(long)]
    no_restore: bool,

    /// Allow restoring of images that don't match the version we expect.
    #[structopt(long)]
    allow_bad_image_version: bool,

    /// Dir/file to include in the checkpoint image.
    /// May be specified multiple times. Multiple paths can also be specified colon separated.
    // require_delimiter is set to avoid clap's non-standard way of accepting lists.
    #[structopt(long="preserve-path", name="path", require_delimiter=true, value_delimiter=":")]
    preserved_paths: Vec<PathBuf>,

    /// Leave application stopped after restore, useful for debugging.
    /// Has no effect when running the app from scratch.
    #[structopt(long)]
    leave_stopped: bool,

    /// Verbosity. Can be repeated
    #[structopt(short, long, parse(from_occurrences))]
    pub verbose: u8,

    /// Used for testing, not for normal use.
    /// App monitoring is skipped: FastFreeze exits as soon as the app is running
    // Maybe we could explore this feature at some point instead of having the
    // start hook. It might be tricky to figure out who should be the parent of
    // app during restore. We could explore CLONE_PARENT. But we would need to do similar
    // tricks to what CRIU does to monitor the process, which is to use ptrace.
    #[structopt(long, hidden=true)]
    detach: bool,
}


/// `AppConfig` is created during the run command, and updated during checkpoint.
/// These settings are saved under `APP_CONFIG_PATH`.
/// It's useful for the checkpoint command to know the image_url and preserved_paths.
/// During restore, it is useful to read the app_clock.

#[derive(Serialize, Deserialize)]
pub struct AppConfig {
    pub image_url: String,
    pub preserved_paths: HashSet<PathBuf>,
    pub app_clock: Nanos,
}

impl AppConfig {
    pub fn save(&self) -> Result<()> {
        serde_json::to_writer_pretty(fs::File::create(&*APP_CONFIG_PATH)?, &self)?;
        Ok(())
    }

    pub fn restore() -> Result<AppConfig> {
        let file = fs::File::open(&*APP_CONFIG_PATH)
            .with_context(|| format!("Failed to open {}. \
                It is created during the run command", APP_CONFIG_PATH.display()))?;
        Ok(serde_json::from_reader(file)?)
    }
}


fn restore(
    image_url: String,
    preserved_paths: HashSet<PathBuf>,
    shard_download_cmds: Vec<String>,
    leave_stopped: bool,
) -> Result<Stats> {
    info!("Restoring application{}", if leave_stopped { " (leave stopped)" } else { "" });
    let mut pgrp = ProcessGroup::new()?;

    let mut img_streamer = ImageStreamer::spawn_serve(shard_download_cmds.len())?;
    img_streamer.process.join(&mut pgrp);

    // Spawn the download processes connected to the image streamer's input
    for (download_cmd, shard_pipe) in shard_download_cmds.into_iter().zip(img_streamer.shard_pipes) {
        Command::new_shell(&download_cmd)
            .stdout(Stdio::from(shard_pipe))
            .spawn()?
            .join(&mut pgrp);
    }

    debug!("Restoring filesystem");
    spawn_untar(img_streamer.tar_fs_pipe.unwrap())?
        .wait_for_success()?;
    debug!("Filesystem restored");

    // The file system is back, including the application configuration containing user-defined
    // preserved-paths, and application time offset.
    // We load the app config, add the new preserved_paths, and save it. It will be useful for the
    // subsequent checkpoint.
    let mut config = AppConfig::restore()?;
    config.image_url = image_url;
    config.preserved_paths.extend(preserved_paths);
    config.save()?;

    // Adjust the libtimevirt offsets
    debug!("Application clock: {:.1}s",
           Duration::from_nanos(config.app_clock as u64).as_secs_f64());
    virt::time::ConfigPath::default().adjust_timespecs(config.app_clock)?;

    // We start the ns_last_pid daemon here. Note that we join_as_daemon() instead of join(),
    // this is so we don't wait for it in wait_for_success().
    debug!("Starting set_ns_last_pid server");
    spawn_set_ns_last_pid_server()?
        .join_as_daemon(&mut pgrp);

    debug!("Continuing reading image in memory...");

    let stats = img_streamer.progress.wait_for_stats()?;
    stats.show();

    // Wait for the imager to be ready.
    img_streamer.progress.wait_for_socket_init()?;

    // Restore processes. We become the parent of the application as CRIU
    // is configured to use CLONE_PARENT.
    // If we fail, we kill whatever is left of the application.
    debug!("Restoring processes");
    criu::spawn_restore(leave_stopped)?
        .join(&mut pgrp);

    // Wait for all our all our monitored processes to finish.
    // If there's an issue, kill the app if it's still laying around.
    // We might want to check that we are the parent of the process with pid APP_ROOT_PID,
    // otherwise, we might be killing an innocent process. But that would be racy anyways.
    if let Err(e) = pgrp.wait_for_success() {
        let _ = killpg(Pid::from_raw(APP_ROOT_PID), signal::SIGKILL);
        return Err(e);
    }

    info!("Application is ready, restore took {:.1}s", START_TIME.elapsed().as_secs_f64());

    Ok(stats)
}

/// `monitor_app()` assumes the init role. We do the following:
/// 1) We proxy signals we receive to our child pid=APP_ROOT_PID.
/// 2) We reap processes that get reparented to us.
/// 3) When APP_ROOT_PID dies, we return an error that contains the appropriate exit_code.
///    (even when the application exited with 0. It makes the code simpler).
fn monitor_app() -> Result<()> {
    for sig in Signal::iterator() {
        // We don't forward SIGCHLD, and neither `FORBIDDEN` signals (e.g.,
        // SIGSTOP, SIGFPE, SIGKILL, ...)
        if sig == Signal::SIGCHLD || signal_hook::FORBIDDEN.contains(&(sig as c_int)) {
            continue;
        }

        // Forward signal to our child.
        // The `register` function is unsafe because one could call malloc(),
        // and deadlock the program. Here we call kill() which is safe.
        unsafe {
            signal_hook::register(sig as c_int, move || {
                let _ = kill(Pid::from_raw(APP_ROOT_PID), sig);
            })?;
        }
    }
    pthread_sigmask(SigmaskHow::SIG_UNBLOCK, Some(&SigSet::all()), None)?;

    // Helper function used in the loop
    fn child_exited<F: Fn() -> anyhow::Error>(pid: Pid, app_exited_f: F) -> Result<()> {
        if pid.as_raw() == APP_ROOT_PID {
            // kill remaining orphans: They belong to the process group that we
            // made with setsid() in run_from_scratch().
            // TODO Check if that's actually necessary.
            let _ = killpg(pid, signal::SIGKILL);
            Err(app_exited_f())
        } else {
            Ok(())
        }
    }

    loop {
        match wait()? {
            WaitStatus::Exited(pid, exit_status) =>
                child_exited(pid, || {
                    anyhow!("Application exited with exit_code={}", exit_status)
                        .context(ExitCode(exit_status as u8))
                })?,
            WaitStatus::Signaled(pid, signal, _core_dumped) =>
                child_exited(pid, || {
                    anyhow!("Application caught fatal signal {}", signal)
                        .context(ExitCode(128 + signal as u8))
                })?,
            _ => {},
        };
    }
}

fn run_from_scratch(
    image_url: String,
    preserved_paths: HashSet<PathBuf>,
    app_cmd: Vec<OsString>,
) -> Result<()>
{
    let config = AppConfig {
        image_url,
        preserved_paths,
        app_clock: 0,
    };
    config.save()?;

    virt::time::ConfigPath::default().write_intial()?;
    virt::enable_system_wide_virtualization()?;

    let mut cmd = Command::new(app_cmd);
    if let Some(path) = std::env::var_os("FF_APP_PATH") {
        cmd.env_remove("FF_APP_PATH")
           .env("PATH", path);
    }
    if let Some(library_path) = std::env::var_os("FF_APP_LD_LIBRARY_PATH") {
        cmd.env_remove("FF_APP_LD_LIBRARY_PATH")
           .env("LD_LIBRARY_PATH", library_path);
    }
    cmd.setsid();
    cmd.spawn_with_pid(APP_ROOT_PID)?;

    info!("Application is ready, started from scratch");

    Ok(())
}

pub enum RunMode {
    Restore { shard_download_cmds: Vec<String> },
    FromScratch,
}

pub fn determine_run_mode(image_url: &str, allow_bad_image_version: bool) -> Result<RunMode> {
    let store = store::from_url(&image_url)?;

    info!("Fetching image manifest for {}", image_url);

    let fetch_result = with_metrics("fetch_manifest",
        || ImageManifest::fetch_from_store(&*store, allow_bad_image_version),
        |fetch_result| match fetch_result {
            ManifestFetchResult::Some(_)              => json!({"manifest": "good",             "run_mode": "restore"}),
            ManifestFetchResult::VersionMismatch {..} => json!({"manifest": "version_mismatch", "run_mode": "run_from_scratch"}),
            ManifestFetchResult::NotFound             => json!({"manifest": "not_found",        "run_mode": "run_from_scratch"}),
        }
    )?;

    Ok(match fetch_result {
        ManifestFetchResult::Some(img_manifest) => {
            debug!("Image manifest found: {:?}", img_manifest);
            let shard_download_cmds = shard::download_cmds(&img_manifest, &*store);
            RunMode::Restore { shard_download_cmds }
        }
        ManifestFetchResult::VersionMismatch { fetched, desired } => {
            info!("Image manifest found, but has version {} while the expected version is {}. \
                   You may try again with --allow-bad-image-version. \
                   Running application from scratch", fetched, desired);
            RunMode::FromScratch
        }
        ManifestFetchResult::NotFound => {
            info!("Image manifest not found, running application from scratch");
            RunMode::FromScratch
        }
    })
}

fn ensure_non_conflicting_pid() -> Result<()> {
    // We don't want to use a PID that could be potentially used by the
    // application when being restored.
    if std::process::id() > APP_ROOT_PID as u32 {
        // We should be pid=1 in a container, so this code block only applies when running
        // outside of a container.
        set_ns_last_pid(MIN_PID)?;
        bail!("Current pid is too high. Re-run the same command again.");
    }

    Ok(())
}

impl super::CLI for Run {
    fn run(self) -> Result<()> {
        let Self {
            image_url, app_args, on_app_ready_cmd, no_restore,
            allow_bad_image_version, preserved_paths, leave_stopped, verbose: _,
            detach } = self;

        let preserved_paths = preserved_paths.into_iter().collect();

        // Holding the lock while invoking any process (e.g., `spawn_smoke_check`) is
        // preferrable to avoid disturbing another instance of FastFreeze trying
        // to do PID control.
        with_checkpoint_restore_lock(|| {
            criu::spawn_smoke_check()?
                .wait_for_success()?;

            ensure_non_conflicting_pid()?;

            // We prepare the store for writes to speed up checkpointing. Notice that
            // we also prepare the store during restore, because we want to make sure
            // we can checkpoint after a restore.
            trace!("Preparing image store");
            store::from_url(&image_url)?.prepare(true)?;

            let run_mode = if no_restore {
                info!("Running app from scratch as specified with --no-restore");
                RunMode::FromScratch
            } else {
                determine_run_mode(&image_url, allow_bad_image_version)
                    .context(ExitCode(EXIT_CODE_RESTORE_FAILURE))?
            };

            match run_mode {
                RunMode::Restore { shard_download_cmds } => {
                    with_metrics("restore", ||
                        restore(image_url, preserved_paths, shard_download_cmds, leave_stopped)
                            .context(ExitCode(EXIT_CODE_RESTORE_FAILURE)),
                        |stats| json!({"stats": stats}))?;
                }
                RunMode::FromScratch => {
                    let app_args = app_args.into_iter().map(|s| s.into()).collect();
                    with_metrics("run_from_scratch", ||
                        run_from_scratch(image_url, preserved_paths, app_args),
                        |_| json!({}))?;
                }
            }

            Ok(())
        })?;

        if let Some(on_app_ready_cmd) = on_app_ready_cmd {
            // Fire and forget.
            Command::new_shell(&on_app_ready_cmd)
                .spawn()?;
        }

        // detach is only used for integration tests
        if !detach {
            monitor_app()?;
        }

        Ok(())
    }
}
