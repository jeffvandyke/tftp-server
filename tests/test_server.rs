use assert_matches::*;

use std::borrow::BorrowMut;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::thread;
use std::time::Duration;
use tftp_server::packet::{ErrorCode, Packet, TftpOption, MAX_PACKET_SIZE};
use tftp_server::server::{Result, ServerConfig, TftpServer};

use tftp_server::packet::TransferMode::*;

mod misc_utils;
use crate::misc_utils::*;

/// Starts the server in a new thread.
pub fn start_server() -> Result<Vec<SocketAddr>> {
    let mut cfg: ServerConfig = Default::default();
    cfg.addrs = vec![];
    assert!(
        TftpServer::with_cfg(&cfg).is_err(),
        "server creation succeeded without addresses"
    );

    cfg.addrs = vec![
        (IpAddr::from([127, 0, 0, 1]), None),
        (IpAddr::from([127, 0, 0, 1]), None),
    ];
    let mut server = TftpServer::with_cfg(&cfg)?;
    let mut addrs = vec![];
    server.get_local_addrs(&mut addrs)?;
    assert_eq!(addrs.len(), cfg.addrs.len(), "wrong number of addresses");
    thread::spawn(move || {
        if let Err(e) = server.run() {
            println!("Error with server: {:?}", e);
        }
        ()
    });

    Ok(addrs)
}

pub fn assert_files_identical(fa: &str, fb: &str) {
    assert!(fs::metadata(fa).is_ok());
    assert!(fs::metadata(fb).is_ok());

    let (mut f1, mut f2) = (File::open(fa).unwrap(), File::open(fb).unwrap());
    let mut buf1 = String::new();
    let mut buf2 = String::new();

    f1.read_to_string(&mut buf1).unwrap();
    f2.read_to_string(&mut buf2).unwrap();

    assert_eq!(buf1, buf2);
}

fn timeout_test(server_addr: &SocketAddr) -> Result<()> {
    let socket = create_socket(None)?;
    let init_packet = Packet::WRQ {
        filename: "hello.txt".into(),
        mode: Octet,
        options: vec![],
    };
    socket.send_to(init_packet.into_bytes()?.as_slice(), server_addr)?;

    let mut buf = [0; MAX_PACKET_SIZE];
    let amt = socket.recv(&mut buf)?;
    let reply_packet = Packet::read(&buf[0..amt])?;
    assert_eq!(reply_packet, Packet::ACK(0));

    let deadman = DeadmanThread::start(Duration::from_millis(3500), "timeout failed");
    let amt = socket.recv(&mut buf)?;
    let reply_packet = Packet::read(&buf[0..amt])?;
    assert_eq!(reply_packet, Packet::ACK(0));
    drop(deadman);

    socket
        .set_read_timeout(Some(Duration::from_millis(3200)))
        .unwrap();
    assert_matches!(
        socket.recv_from(&mut buf), Err(ref e) if e.kind() == io::ErrorKind::WouldBlock,
        "packet received after connection should have dropped"
    );

    assert!(fs::metadata("./hello.txt").is_ok());
    assert!(fs::remove_file("./hello.txt").is_ok());
    Ok(())
}

struct WritingTransfer {
    socket: UdpSocket,
    file: File,
    block_num: u16,
    remote: Option<SocketAddr>,
    blocksize: u64,
}

impl WritingTransfer {
    fn start(
        local_file: &str,
        server_addr: &SocketAddr,
        server_file: &str,
        options: Vec<TftpOption>,
    ) -> Self {
        let mut blocksize: u64 = 512;
        for opt in &options {
            if let TftpOption::Blocksize(size) = *opt {
                blocksize = u64::from(size);
            }
        }
        let xfer = Self {
            socket: create_socket(Some(Duration::from_secs(TIMEOUT))).unwrap(),
            file: File::open(local_file).expect(&format!("cannot open {}", local_file)),
            block_num: 0,
            remote: None,
            blocksize,
        };
        let init_packet = Packet::WRQ {
            filename: server_file.into(),
            mode: Octet,
            options,
        };
        xfer.socket
            .send_to(init_packet.to_bytes().unwrap().as_slice(), &server_addr)
            .expect(&format!(
                "cannot send initial packet {:?} to {:?}",
                init_packet, server_addr
            ));
        xfer
    }

    fn step(&mut self, rx_buf: &mut [u8]) -> Option<()> {
        let (amt, src) = self.socket.recv_from(rx_buf).expect("cannot receive");
        if self.remote.is_some() {
            assert_eq!(self.remote.unwrap(), src, "transfer source changed");
        } else {
            self.remote = Some(src);
        }
        let received = Packet::read(&rx_buf[0..amt]).unwrap();
        if let Packet::OACK { .. } = received {
            assert_eq!(self.block_num, 0);
        } else {
            assert_eq!(received, Packet::ACK(self.block_num));
        }
        self.block_num = self.block_num.wrapping_add(1);

        // Read and send data packet
        let mut data = Vec::with_capacity(self.blocksize as usize);
        let res = self
            .file
            .borrow_mut()
            .take(self.blocksize)
            .read_to_end(&mut data);
        if res.expect("error reading from file") == 0 {
            return None;
        }
        let data_packet = Packet::DATA {
            block_num: self.block_num,
            data,
        };

        self.socket
            .send_to(data_packet.to_bytes().unwrap().as_slice(), &src)
            .expect(&format!(
                "cannot send packet {:?} to {:?}",
                data_packet, src
            ));
        Some(())
    }
}

fn wrq_whole_file_test(server_addr: &SocketAddr, options: Vec<TftpOption>) -> Result<()> {
    // remore file if it was left over after a test that panicked
    let _ = fs::remove_file("./hello.txt");

    let mut scratch_buf = [0; MAX_PACKET_SIZE];

    let mut tx = WritingTransfer::start("./files/hello.txt", server_addr, "hello.txt", options);
    while let Some(_) = tx.step(&mut scratch_buf) {}

    // Would cause server to have an error if not handled robustly
    tx.socket.send_to(&[1, 2, 3], &tx.remote.unwrap())?;

    assert_files_identical("./hello.txt", "./files/hello.txt");
    assert!(fs::remove_file("./hello.txt").is_ok());
    Ok(())
}

struct ReadingTransfer {
    socket: UdpSocket,
    file: File,
    block_num: u16,
    remote: Option<SocketAddr>,
    blocksize: u64,
}

impl ReadingTransfer {
    fn start(
        local_file: &str,
        server_addr: &SocketAddr,
        server_file: &str,
        options: Vec<TftpOption>,
    ) -> Self {
        let mut blocksize: u64 = 512;
        for opt in &options {
            if let TftpOption::Blocksize(size) = *opt {
                blocksize = u64::from(size);
            }
        }
        let xfer = Self {
            socket: create_socket(Some(Duration::from_secs(TIMEOUT))).unwrap(),
            file: File::create(local_file).expect(&format!("cannot create {}", local_file)),
            block_num: 1,
            remote: None,
            blocksize,
        };
        let init_packet = Packet::RRQ {
            filename: server_file.into(),
            mode: Octet,
            options,
        };
        xfer.socket
            .send_to(init_packet.to_bytes().unwrap().as_slice(), &server_addr)
            .expect(&format!(
                "cannot send initial packet {:?} to {:?}",
                init_packet, server_addr
            ));
        xfer
    }

    fn step(&mut self, rx_buf: &mut [u8]) -> Option<()> {
        let (amt, src) = self.socket.recv_from(rx_buf).expect("cannot receive");
        if self.remote.is_some() {
            assert_eq!(self.remote.unwrap(), src, "transfer source changed");
        } else {
            self.remote = Some(src);
        }

        let received = Packet::read(&rx_buf[0..amt]).unwrap();
        match received {
            Packet::OACK { .. } => {
                assert_eq!(self.block_num, 1);
                let ack_packet = Packet::ACK(0);
                self.socket
                    .send_to(ack_packet.to_bytes().unwrap().as_slice(), &src)
                    .expect(&format!("cannot send packet {:?} to {:?}", ack_packet, src));
            }
            Packet::DATA { block_num, data } => {
                assert_eq!(self.block_num, block_num);
                self.file
                    .write_all(&data)
                    .expect("cannot write to local file");

                let ack_packet = Packet::ACK(self.block_num);
                self.socket
                    .send_to(ack_packet.to_bytes().unwrap().as_slice(), &src)
                    .expect(&format!("cannot send packet {:?} to {:?}", ack_packet, src));

                self.block_num = self.block_num.wrapping_add(1);

                if data.len() < self.blocksize as usize {
                    return None;
                }
            }
            _ => {
                panic!("Reply packet is not a data packet");
            }
        }
        Some(())
    }
}

fn rrq_whole_file_test(server_addr: &SocketAddr, options: Vec<TftpOption>) -> Result<()> {
    let mut scratch_buf = [0; MAX_PACKET_SIZE];

    let mut rx = ReadingTransfer::start("./hello.txt", server_addr, "./files/hello.txt", options);
    while let Some(_) = rx.step(&mut scratch_buf) {}

    // Would cause server to have an error if not handled robustly
    rx.socket.send_to(&[1, 2, 3], &rx.remote.unwrap())?;

    assert_files_identical("./hello.txt", "./files/hello.txt");
    assert!(fs::remove_file("./hello.txt").is_ok());
    Ok(())
}

fn wrq_file_exists_test(server_addr: &SocketAddr) -> Result<()> {
    let socket = create_socket(None)?;
    let init_packet = Packet::WRQ {
        filename: "./files/hello.txt".into(),
        mode: Octet,
        options: vec![],
    };
    socket.send_to(init_packet.into_bytes()?.as_slice(), server_addr)?;

    let mut buf = [0; MAX_PACKET_SIZE];
    let amt = socket.recv(&mut buf)?;
    let packet = Packet::read(&buf[0..amt])?;
    assert_matches!(packet, Packet::ERROR { code: ErrorCode::FileExists , .. });
    Ok(())
}

fn rrq_file_not_found_test(server_addr: &SocketAddr) -> Result<()> {
    let socket = create_socket(None)?;
    let init_packet = Packet::RRQ {
        filename: "./hello.txt".into(),
        mode: Octet,
        options: vec![],
    };
    socket.send_to(init_packet.into_bytes()?.as_slice(), server_addr)?;

    let mut buf = [0; MAX_PACKET_SIZE];
    let amt = socket.recv(&mut buf)?;
    let packet = Packet::read(&buf[0..amt])?;
    assert_matches!(packet, Packet::ERROR { code: ErrorCode::FileNotFound , .. });
    Ok(())
}

fn interleaved_read_read_same_file(server_addr: &SocketAddr) {
    let mut scratch_buf = [0; MAX_PACKET_SIZE];

    let mut read_a =
        ReadingTransfer::start("./read_a.txt", server_addr, "./files/hello.txt", vec![]);
    let mut read_b =
        ReadingTransfer::start("./read_b.txt", server_addr, "./files/hello.txt", vec![]);
    loop {
        let res_a = read_a.step(&mut scratch_buf);
        let res_b = read_b.step(&mut scratch_buf);
        assert_eq!(res_a, res_b, "reads finished in different number of steps");
        if res_a == None {
            break;
        }
    }

    assert_files_identical("./read_a.txt", "./files/hello.txt");
    assert_files_identical("./read_a.txt", "./read_b.txt");
    assert!(fs::remove_file("./read_a.txt").is_ok());
    assert!(fs::remove_file("./read_b.txt").is_ok());
}

fn main() {
    env_logger::init();
    let addrs = start_server().unwrap();
    let server_addr = addrs[0];
    for addr in &addrs {
        wrq_whole_file_test(addr, vec![]).unwrap();
        rrq_whole_file_test(addr, vec![]).unwrap();
    }

    timeout_test(&server_addr).unwrap();
    wrq_file_exists_test(&server_addr).unwrap();
    rrq_file_not_found_test(&server_addr).unwrap();
    interleaved_read_read_same_file(&server_addr);
    wrq_whole_file_test(&server_addr, vec![TftpOption::Blocksize(2050)]).unwrap();
    rrq_whole_file_test(&server_addr, vec![TftpOption::Blocksize(2050)]).unwrap();
}
