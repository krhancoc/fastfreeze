use crate::{
    consts::FF_SOCKET_PATH,
    poller::{Poller, EpollFlags},
    cli::checkpoint::{Checkpoint, do_checkpoint},
    image::CpuBudget,
};

use std::os::unix::{
    net::{UnixListener, UnixStream},
    io::{AsRawFd, FromRawFd},
};
use std::io::{Read, Write};
use nix::{
    fcntl::OFlag,
    unistd::pipe2,
};
use std::fs;
use anyhow::{Result, Context};

pub const EPOLL_CAPACITY: usize =8;

pub struct FastFreezeDaemon {
    stop_pipe_w: fs::File,
    thread: std::thread::JoinHandle<()>,
}

pub struct FastFreezeConnection {
    socket: UnixStream,
}

pub struct FastFreezeListener {
    listener: UnixListener,
}

enum PollType {
    Listener(FastFreezeListener),
    Connection(FastFreezeConnection),
    Stop,
}

// TODO:
// We need to make sure we can handle callbacks within the FastFreezeDaemon, so we 
// need a communication channel to send our callback requests to the running daemon
// (main_loop), it will then dispatch these callbacks and collect up acknowledgements.
//
// Modify the poller object to include iterators of connection objects so we broadcast
// functions to these connection

fn main_loop(listener: FastFreezeListener, stop_pipe_r: fs::File) -> Result<()> {
    let mut poller = Poller::<PollType>::new()?;
    debug!("FastFreeze Socket: {}, Stop Pipe: {}", listener.listener.as_raw_fd(), stop_pipe_r.as_raw_fd());
    poller.add(stop_pipe_r.as_raw_fd(), PollType::Stop, EpollFlags::EPOLLHUP | EpollFlags::EPOLLIN)?;
    poller.add(listener.listener.as_raw_fd(), PollType::Listener(listener), EpollFlags::EPOLLIN)?;

    // We currently only poll on reads as we don't believe it is reasonable to poll on writes,
    // so we are fine with blocking on writes to the application.
    // Possible problems in the future?
    //      The deamon could possibly not stop as it maybe blocked trying to write.
    while let Some((poll_key, poll_obj)) = poller.poll(EPOLL_CAPACITY)? {
        match poll_obj {
            // Recieve new connection
            PollType::Listener(listener) => {
                let new_connection = listener.accept()?;
                poller.add(new_connection.socket.as_raw_fd(), PollType::Connection(new_connection),
                    EpollFlags::EPOLLIN)?;
            }
            // Getting an actual checkpoint command
            PollType::Connection(connection) => {
                let mut buf = [0u8; 1024];
                // Read the checkpoint command
                // 
                // TODO:
                // For now we expect the application will send us the args identical to the required
                // arguments for `fastfreeze checkpoint`
                match connection.read(&mut buf) {
                    Ok(size) => {
                        println!("SIZE");
                        if size != 0 {
                            let cp = Checkpoint {
                                image_url: None, 
                                preserved_paths: vec![] as Vec<std::path::PathBuf>, 
                                leave_running: true, 
                                num_shards: 1, 
                                cpu_budget: CpuBudget::Medium,
                                passphrase_file: None, 
                                verbose: 0,
                                app_name: None
                            };
                            let _ = do_checkpoint(cp);
                            let _ = connection.write_all(&mut buf);
                        } else {
                            let _ = poller.remove(poll_key);
                        }
                    }
                    Err(_) => {
                        println!("SIZE OUT");
                        let _ = poller.remove(poll_key);
                    }
                }
            }
            PollType::Stop => {
                return Ok(());
            }
        }
    }

    Ok(())
}

impl Read for FastFreezeConnection {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        return self.socket.read(buf);
    }
}

impl Write for FastFreezeConnection {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        return self.socket.write(buf);
    }
    fn flush(&mut self) -> std::io::Result<()> {
        return self.socket.flush();
    }
}

impl FastFreezeDaemon {
    pub fn stop(self) -> Result<()> {
        drop(self.stop_pipe_w);
        let _ = self.thread.join();
        Ok(())
    }
}

impl FastFreezeListener {
    pub fn bind() -> Result<Self> {
        let socket_path = &*FF_SOCKET_PATH;
        let _ = fs::remove_file(socket_path);
        let listener = UnixListener::bind(socket_path)
            .with_context(|| format!("Failed to bind socket to {}", socket_path.display()))?;
        Ok(Self { listener })
    }

    pub fn accept(&mut self) -> Result<FastFreezeConnection> {
        let (socket, _) = self.listener.accept()?;
        Ok(FastFreezeConnection { socket })
    }

    pub fn into_daemon(self) -> Result<FastFreezeDaemon> {
        let (pipe_r, pipe_w) = pipe2(OFlag::O_CLOEXEC)?;
        let thread = std::thread::spawn(move || {
            main_loop(self, unsafe { fs::File::from_raw_fd(pipe_r) }).expect("Daemon crashed");
        });
        Ok(FastFreezeDaemon { stop_pipe_w: unsafe { fs::File::from_raw_fd(pipe_w) }, thread: thread })
    }


}
