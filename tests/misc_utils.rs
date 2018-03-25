use std::io::Result;
use std::net::{IpAddr, UdpSocket};
use std::sync::mpsc::*;
use std::thread;
use std::time::Duration;

pub const TIMEOUT: u64 = 3;

pub fn create_socket(timeout: Option<Duration>) -> Result<UdpSocket> {
    let socket = UdpSocket::bind((IpAddr::from([127, 0, 0, 1]), 0))?;
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(timeout)?;
    socket.set_write_timeout(timeout)?;
    Ok(socket)
}

pub struct DeadmanThread {
    tx: Sender<()>,
}

impl DeadmanThread {
    pub fn start(dur: Duration, msg: &str) -> Self {
        let msg = msg.to_owned();
        let (tx, rx) = channel();
        thread::spawn(move || {
            if rx.recv_timeout(dur).is_err() {
                eprintln!("\nDeadman timeout expired: {}\n", msg);
                ::std::process::exit(1)
            }
        });
        Self { tx }
    }
}

impl Drop for DeadmanThread {
    fn drop(&mut self) {
        if !::std::thread::panicking() {
            self.tx.send(()).expect("cannot stop deadman thread");
        }
    }
}
