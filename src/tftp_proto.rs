use std::io::{self, Read, Write};
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};
use packet::{ErrorCode, Packet, TftpOption};
use std::time::Duration;

#[derive(Debug, PartialEq)]
pub enum TftpResult {
    /// Indicates the packet should be sent back to the client,
    /// and the transfer may continue
    Reply(Packet),

    /// Signals the calling code that it should resend the last packet
    Repeat,

    /// Indicates that the packet (if any) should be sent back to the client,
    /// and the transfer is considered terminated
    Done(Option<Packet>),

    /// Indicates an error encountered while processing the packet
    Err(TftpError),
}
use self::TftpResult::{Done, Repeat, Reply};

#[derive(Debug, PartialEq)]
pub enum TftpError {
    /// The is already running and cannot be restarted
    TransferAlreadyRunning,

    /// The received packet type cannot be used to initiate a transfer
    NotIniatingPacket,
}

/// Trait used to inject filesystem IO handling into a server.
/// A trivial default implementation is provided by `FSAdapter`.
/// If you want to employ things like buffered IO, it can be done by providing
/// an implementation for this trait and passing the implementing type to the server.
pub trait IOAdapter {
    type R: Read + Sized;
    type W: Write + Sized;
    fn open_read(&self, file: &Path) -> io::Result<(Self::R, Option<u64>)>;
    fn create_new(&mut self, file: &Path, len: Option<u64>) -> io::Result<Self::W>;
}

/// Provides a simple, default implementation for `IOAdapter`.
pub struct FSAdapter;

impl IOAdapter for FSAdapter {
    type R = File;
    type W = File;
    fn open_read(&self, file: &Path) -> io::Result<(File, Option<u64>)> {
        let f = File::open(file)?;
        let len = f.metadata().ok().map(|meta| meta.len());
        Ok((f, len))
    }
    fn create_new(&mut self, file: &Path, len: Option<u64>) -> io::Result<File> {
        let f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file)?;
        if let Some(l) = len {
            f.set_len(l)?;
        }
        Ok(f)
    }
}

impl Default for FSAdapter {
    fn default() -> Self {
        FSAdapter
    }
}

#[derive(Debug)]
struct TransferMeta {
    blocksize: u16,
    timeout: Option<u8>,
    timed_out: bool,
    window_size: u16,
}

/// The TFTP protocol and filesystem usage implementation,
/// used as backend for a TFTP server
pub struct TftpServerProto<IO: IOAdapter> {
    io_proxy: IOPolicyProxy<IO>,
}

#[derive(Debug)]
pub struct Response {
    p: Vec<ResponseItem>,
}

impl Iterator for Response {
    type Item = ResponseItem;
    fn next(&mut self) -> Option<Self::Item> {
        self.p.pop()
    }
}

impl<T> From<T> for Response
where
    T: Into<ResponseItem>,
{
    fn from(r: T) -> Self {
        Self { p: vec![r.into()] }
    }
}
impl From<Vec<ResponseItem>> for Response {
    fn from(mut v: Vec<ResponseItem>) -> Self {
        v.reverse();
        Self { p: v }
    }
}

#[derive(Debug, PartialEq)]
pub enum ResponseItem {
    Packet(Packet),
    Done,
    RepeatLast(usize),
}

impl<IO: IOAdapter> TftpServerProto<IO> {
    /// Creates a new instance with the provided IOAdapter
    pub fn new(io: IO, cfg: IOPolicyCfg) -> Self {
        TftpServerProto {
            io_proxy: IOPolicyProxy::new(io, cfg),
        }
    }

    /// Signals the receipt of a transfer-initiating packet (either RRQ or WRQ).
    /// If a `Transfer` is returned in the first tuple member, that must be used to
    /// handle all future packets from the same client via `Transfer::rx`
    /// If a 'Transfer' is not returned, then a transfer cannot be started from the
    /// received packet
    ///
    /// In both cases the packet contained in the `Result` should be sent back to the client
    pub fn rx_initial(
        &mut self,
        packet: Packet,
    ) -> (Option<Transfer<IO>>, Result<Packet, TftpError>) {
        let (filename, mode, mut options, is_write) = match packet {
            Packet::RRQ {
                filename,
                mode,
                options,
            } => (filename, mode, options, false),
            Packet::WRQ {
                filename,
                mode,
                options,
            } => (filename, mode, options, true),
            _ => return (None, Err(TftpError::NotIniatingPacket)),
        };
        use packet::TransferMode;
        match mode {
            TransferMode::Octet => {}
            TransferMode::Mail => return (None, Ok(ErrorCode::NoUser.into())),
            _ => return (None, Ok(ErrorCode::NotDefined.into())),
        }
        let file = Path::new(&filename);

        let mut meta = TransferMeta {
            blocksize: 512,
            timeout: None,
            timed_out: false,
            window_size: 1,
        };
        let mut tsize = None;

        let mut options = options
            .drain(..)
            .filter_map(|opt| {
                match opt {
                    TftpOption::Blocksize(size) => meta.blocksize = size,
                    TftpOption::TimeoutSecs(secs) => meta.timeout = Some(secs),
                    TftpOption::TransferSize(size) => {
                        tsize = Some(size);
                        if !is_write {
                            // for read take out the transfer size initially, it needs changing
                            return None;
                        }
                    }
                    TftpOption::WindowSize(size) => meta.window_size = size,
                }
                Some(opt)
            })
            .collect::<Vec<_>>();

        let (xfer, packet) = if is_write {
            let fwrite = match self.io_proxy.create_new(file, tsize) {
                Ok(f) => f,
                _ => return (None, Ok(ErrorCode::FileExists.into())),
            };

            Transfer::<IO>::new_write(fwrite, meta, options)
        } else {
            let (fread, len) = match self.io_proxy.open_read(file) {
                Ok(f) => f,
                _ => return (None, Ok(ErrorCode::FileNotFound.into())),
            };

            if let (Some(_), Some(file_size)) = (tsize, len) {
                options.push(TftpOption::TransferSize(file_size));
            }

            Transfer::<IO>::new_read(fread, meta, options)
        };

        (xfer, Ok(packet.into()))
    }
}

/// The state of an ongoing transfer with one client
#[derive(Debug)]
pub enum Transfer<IO: IOAdapter> {
    Rx(TransferRx<IO::W>),
    Tx(TransferTx<IO::R>),
    Complete,
}

#[derive(Debug)]
pub struct TransferRx<W: Write> {
    fwrite: W,
    expected_block_num: u16,
    meta: TransferMeta,
}

#[derive(Debug)]
pub struct TransferTx<R: Read> {
    fread: R,
    expected_block_num: u16,
    sent_final: bool,
    meta: TransferMeta,
}

impl<IO: IOAdapter> Transfer<IO> {
    fn new_read(
        fread: IO::R,
        meta: TransferMeta,
        options: Vec<TftpOption>,
    ) -> (Option<Transfer<IO>>, Packet) {
        let mut xfer = TransferTx {
            fread,
            expected_block_num: 0,
            sent_final: false,
            meta,
        };

        let packet = if options.is_empty() {
            xfer.read_step()
        } else {
            Ok(Packet::OACK { options })
        };
        match packet {
            Ok(p) => (Some(Transfer::Tx(xfer)), p),
            Err(p) => (None, p),
        }
    }

    fn new_write(
        fwrite: IO::W,
        meta: TransferMeta,
        options: Vec<TftpOption>,
    ) -> (Option<Transfer<IO>>, Packet) {
        let xfer = TransferRx {
            fwrite,
            expected_block_num: 1,
            meta,
        };

        let packet = if options.is_empty() {
            Packet::ACK(0)
        } else {
            Packet::OACK { options }
        };
        (Some(Transfer::Rx(xfer)), packet)
    }

    /// Checks to see if the transfer has completed
    pub fn is_done(&self) -> bool {
        match *self {
            Transfer::Complete => true,
            _ => false,
        }
    }

    /// Call this to indicate that the timeout since the last received packe has expired
    /// This may return some packets to (re)send or may terminate the transfer
    pub fn timeout_expired(&mut self) -> TftpResult {
        let result = match *self {
            Transfer::Rx(TransferRx { ref mut meta, .. })
            | Transfer::Tx(TransferTx { ref mut meta, .. }) => {
                if meta.timed_out {
                    Done(None)
                } else {
                    meta.timed_out = true;
                    Repeat
                }
            }
            _ => Done(None),
        };
        if let Done(_) = result {
            *self = Transfer::Complete;
        };
        result
    }

    /// Returns the timeout negotiated via option for this transfer,
    /// or NULL if the server default should be used
    pub fn timeout(&self) -> Option<Duration> {
        match *self {
            Transfer::Rx(TransferRx { ref meta, .. })
            | Transfer::Tx(TransferTx { ref meta, .. }) => {
                meta.timeout.map(|s| Duration::from_secs(s as u64))
            }
            _ => None,
        }
    }

    /// Process and consume a received packet
    /// When the first `TftpResult::Done` is returned, the transfer is considered complete
    /// and all future calls to rx will also return `TftpResult::Done`
    ///
    /// Transfer completion can be checked via `Transfer::is_done()`
    pub fn rx(&mut self, packet: Packet) -> TftpResult {
        match self.rx2(packet) {
            Err(e) => TftpResult::Err(e),
            Ok(mut resp) => {
                let first = resp.next().unwrap();
                let second = resp.next();
                use self::ResponseItem::*;
                match (first, second) {
                    (RepeatLast(1), _) => Repeat,
                    (Packet(p), None) => Reply(p),
                    (Packet(p), Some(Done)) => TftpResult::Done(Some(p)),
                    (Done, _) => TftpResult::Done(None),
                    (a, b) => panic!("unhandled pattern: {:?} {:?}", a, b),
                }
            }
        }
    }

    pub fn rx2(&mut self, packet: Packet) -> Result<Response, TftpError> {
        if self.is_done() {
            return Ok(ResponseItem::Done.into());
        }
        let result = match (packet, &mut *self) {
            (Packet::ACK(ack_block), &mut Transfer::Tx(ref mut tx)) => Ok(tx.handle_ack(ack_block)),
            (
                Packet::DATA {
                    block_num,
                    ref data,
                },
                &mut Transfer::Rx(ref mut rx),
            ) => Ok(rx.handle_data(block_num, data)),
            (Packet::DATA { .. }, _) | (Packet::ACK(_), _) => {
                // wrong kind of packet, kill transfer
                Ok(vec![
                    ResponseItem::Packet(ErrorCode::IllegalTFTP.into()),
                    ResponseItem::Done,
                ].into())
            }

            (Packet::ERROR { .. }, _) => {
                // receiving an error kills the transfer
                Ok(ResponseItem::Done.into())
            }
            _ => Err(TftpError::TransferAlreadyRunning),
        };

        if let Ok(true) = result.as_ref().map(|r| r.p.contains(&ResponseItem::Done)) {
            *self = Transfer::Complete;
        }
        result
    }
}

impl<R: Read> TransferTx<R> {
    fn handle_ack(&mut self, ack_block: u16) -> Response {
        use self::ResponseItem::RepeatLast;
        use sna::SerialNumber;
        let ack_block = SerialNumber(ack_block);
        let expected_block = SerialNumber(self.expected_block_num);

        let mut window_start = 0;
        let mut v = vec![];
        if ack_block < expected_block && ack_block + self.meta.window_size >= expected_block {
            let mut a = ack_block;
            while a != expected_block {
                window_start += 1;
                a += 1;
            }
            v.push(RepeatLast(window_start));
        }

        if self.meta.window_size == 1 && ack_block + 1 == expected_block {
            RepeatLast(1).into()
        } else if self.meta.window_size == 1 && ack_block != expected_block {
            vec![
                ResponseItem::Packet(Packet::ERROR {
                    code: ErrorCode::UnknownID,
                    msg: "Incorrect block num in ACK".to_owned(),
                }),
                ResponseItem::Done,
            ].into()
        } else if self.sent_final {
            ResponseItem::Done.into()
        } else {
            self.meta.timed_out = false;
            for _ in window_start..self.meta.window_size as usize {
                match self.read_step() {
                    Ok(p) => v.push(ResponseItem::Packet(p)),
                    Err(p) => {
                        return vec![ResponseItem::Packet(p), ResponseItem::Done].into();
                    }
                }
                if self.sent_final {
                    break;
                }
            }
            v.into()
        }
    }

    fn read_step(&mut self) -> Result<Packet, Packet> {
        let mut v = Vec::with_capacity(self.meta.blocksize as usize);
        if self.fread
            .by_ref()
            .take(u64::from(self.meta.blocksize))
            .read_to_end(&mut v)
            .is_err()
        {
            return Err(ErrorCode::NotDefined.into());
        }

        self.sent_final = v.len() < self.meta.blocksize as usize;
        self.expected_block_num = self.expected_block_num.wrapping_add(1);
        Ok(Packet::DATA {
            block_num: self.expected_block_num,
            data: v,
        })
    }
}

impl<W: Write> TransferRx<W> {
    fn handle_data(&mut self, block_num: u16, data: &[u8]) -> Response {
        if block_num != self.expected_block_num {
            vec![
                ResponseItem::Packet(Packet::ERROR {
                    code: ErrorCode::IllegalTFTP,
                    msg: "Data packet lost".to_owned(),
                }),
                ResponseItem::Done,
            ].into()
        } else {
            self.meta.timed_out = false;
            if self.fwrite.write_all(data).is_err() {
                return vec![
                    ResponseItem::Packet(ErrorCode::NotDefined.into()),
                    ResponseItem::Done,
                ].into();
            }
            self.expected_block_num = block_num.wrapping_add(1);
            if data.len() < self.meta.blocksize as usize {
                vec![
                    ResponseItem::Packet(Packet::ACK(block_num)),
                    ResponseItem::Done,
                ].into()
            } else {
                ResponseItem::Packet(Packet::ACK(block_num)).into()
            }
        }
    }
}

pub struct IOPolicyCfg {
    pub readonly: bool,
    pub path: Option<PathBuf>,
}

impl Default for IOPolicyCfg {
    fn default() -> Self {
        Self {
            readonly: false,
            path: None,
        }
    }
}

pub(crate) struct IOPolicyProxy<IO: IOAdapter> {
    io: IO,
    policy: IOPolicyCfg,
}

impl<IO: IOAdapter> IOPolicyProxy<IO> {
    pub(crate) fn new(io: IO, cfg: IOPolicyCfg) -> Self {
        Self { io, policy: cfg }
    }
}

impl<IO: IOAdapter> IOAdapter for IOPolicyProxy<IO> {
    type R = IO::R;
    type W = IO::W;
    fn open_read(&self, file: &Path) -> io::Result<(Self::R, Option<u64>)> {
        if file.is_absolute() || file.components().any(|c| match c {
            Component::RootDir | Component::ParentDir => true,
            _ => false,
        }) {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot read",
            ))
        } else if let Some(ref path) = self.policy.path {
            let full = path.clone().join(file);
            self.io.open_read(&full)
        } else {
            self.io.open_read(file)
        }
    }

    fn create_new(&mut self, file: &Path, len: Option<u64>) -> io::Result<Self::W> {
        if self.policy.readonly || file.is_absolute() || file.components().any(|c| match c {
            Component::RootDir | Component::ParentDir => true,
            _ => false,
        }) {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot write",
            ))
        } else if let Some(ref path) = self.policy.path {
            let full = path.clone().join(file);
            self.io.create_new(&full, len)
        } else {
            self.io.create_new(file, len)
        }
    }
}
