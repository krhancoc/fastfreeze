use std::os::unix::net::UnixStream;
use std::io::prelude::*;
fn main() -> std::io::Result<()> {
    let mut stream = UnixStream::connect("/var/tmp/fastfreeze/run/fastfreeze.sock")?;
    let mut _buf = [0u8; 1024];
    let d = std::time::Duration::from_secs(5);
    println!("My pid is {}", std::process::id());
    std::thread::sleep(d);
    stream.write_all(b"Nothing")?;
    let d = std::time::Duration::from_secs(5);
    std::thread::sleep(d);
    println!("Done!");
    Ok(())
}
