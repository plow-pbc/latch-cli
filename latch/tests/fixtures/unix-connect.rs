use std::os::unix::net::UnixStream;

fn main() -> std::io::Result<()> {
    UnixStream::connect(std::env::args_os().nth(1).expect("socket path"))?;
    Ok(())
}
