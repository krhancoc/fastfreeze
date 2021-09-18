use crate::{
    consts::FF_SOCKET_PATH,
    poller::{Poller, EpollFlags},
};

use std::os::unix::{
    net::{UnixListener, UnixStream},
    io::{AsRawFd, FromRawFd},
};

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
    poller.add(stop_pipe_r.as_raw_fd(), PollType::Stop, EpollFlags::EPOLLHUP | EpollFlags::EPOLLIN)?;
    poller.add(listener.listener.as_raw_fd(), PollType::Listener(listener), EpollFlags::EPOLLIN)?;

    // We currently only poll on reads as we don't believe it is reasonable to poll on writes,
    // so we are fine with blocking on writes to the application.
    // Possible problems in the future?
    //      The deamon could possibly not stop as it maybe blocked trying to write.
    while let Some((_, poll_obj)) = poller.poll(EPOLL_CAPACITY)? {
        match poll_obj {
            // Recieve new connection
            PollType::Listener(listener) => {
                let new_connection = listener.accept()?;
                poller.add(new_connection.socket.as_raw_fd(), PollType::Connection(new_connection),
                    EpollFlags::EPOLLIN)?;
            }
            // Getting an actual checkpoint command
            PollType::Connection(_connection) => {
                // Read the checkpoint command
                // 
                // TODO:
                // For now we expect the application will send us the args identical to the required
                // arguments for `fastfreeze checkpoint`
                //connection.read();
            }
            PollType::Stop => {
                return Ok(());
            }
        }
    }

    Ok(())
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
        let thread = std::thread::spawn(move || main_loop(self, unsafe { fs::File::from_raw_fd(pipe_r) }).expect("Daemon crashed"));
        Ok(FastFreezeDaemon { stop_pipe_w: unsafe { fs::File::from_raw_fd(pipe_w) }, thread: thread })
    }


}
