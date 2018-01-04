use std::io::{self, Read, Write};
use packet::{ErrorCode, Packet};
use read_512::*;

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
    fn open_read(&self, filename: &str) -> io::Result<Self::R>;
    fn create_new(&mut self, filename: &str) -> io::Result<Self::W>;
}

/// The TFTP protocol and filesystem usage implementation,
/// used as backend for a TFTP server
pub struct TftpServerProto<IO: IOAdapter> {
    io_proxy: IOPolicyProxy<IO>,
}

impl<IO: IOAdapter> TftpServerProto<IO> {
    /// Creates a new instance with the provided IOAdapter
    pub fn new(io: IO, cfg: IOPolicyCfg) -> Self {
        TftpServerProto { io_proxy: IOPolicyProxy::new(io, cfg) }
    }

    /// Signals the receipt of a transfer-initiating packet (either RRQ of WRQ).
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
        match packet {
            Packet::RRQ { filename, mode } => self.handle_rrq(&filename, &mode),
            Packet::WRQ { filename, mode } => self.handle_wrq(&filename, &mode),
            _ => (None, Err(TftpError::NotIniatingPacket)),
        }
    }

    fn handle_wrq(
        &mut self,
        filename: &str,
        mode: &str,
    ) -> (Option<Transfer<IO>>, Result<Packet, TftpError>) {
        match mode {
            "octet" => {}
            "mail" => return (None, Ok(ErrorCode::NoUser.into())),
            _ => return (None, Ok(ErrorCode::NotDefined.into())),
        }

        let fwrite = match self.io_proxy.create_new(filename) {
            Ok(f) => f,
            _ => return (None, Ok(ErrorCode::FileExists.into())),
        };

        let xfer = Transfer::<IO>::new_write(fwrite);
        (Some(Transfer::Rx(xfer)), Ok(Packet::ACK(0)))
    }

    fn handle_rrq(
        &mut self,
        filename: &str,
        mode: &str,
    ) -> (Option<Transfer<IO>>, Result<Packet, TftpError>) {
        match mode {
            "octet" => {}
            "mail" => return (None, Ok(ErrorCode::NoUser.into())),
            _ => return (None, Ok(ErrorCode::NotDefined.into())),
        }

        let fread = match self.io_proxy.open_read(filename) {
            Ok(f) => f,
            _ => return (None, Ok(ErrorCode::FileNotFound.into())),
        };

        let mut xfer = Transfer::<IO>::new_read(fread);
        let mut v = vec![];
        xfer.fread.read_512(&mut v).unwrap();
        xfer.sent_final = v.len() < 512;
        (
            Some(Transfer::Tx(xfer)),
            Ok(Packet::DATA {
                block_num: 1,
                data: v,
            }),
        )
    }
}

/// The state of an ongoing transfer with one client
pub enum Transfer<IO: IOAdapter> {
    Rx(TransferRx<IO::W>),
    Tx(TransferTx<IO::R>),
    Complete,
}
use self::Transfer::*;

pub struct TransferRx<W: Write> {
    fwrite: W,
    expected_block_num: u16,
}

pub struct TransferTx<R: Read> {
    fread: R,
    expected_block_num: u16,
    sent_final: bool,
}

impl<IO: IOAdapter> Transfer<IO> {
    fn new_read(fread: IO::R) -> TransferTx<IO::R> {
        TransferTx {
            fread,
            expected_block_num: 1,
            sent_final: false,
        }
    }

    fn new_write(fwrite: IO::W) -> TransferRx<IO::W> {
        TransferRx {
            fwrite,
            expected_block_num: 1,
        }
    }

    /// Checks to see if the transfer has completed
    pub fn is_done(&self) -> bool {
        match *self {
            Complete => true,
            _ => false,
        }
    }

    /// Process and consume a received packet
    /// When the first `TftpResult::Done` is returned, the transfer is considered complete
    /// and all future calls to rx will also return `TftpResult::Done`
    ///
    /// Transfer completion can be checked via `Transfer::is_done()`
    pub fn rx(&mut self, packet: Packet) -> TftpResult {
        let result = match packet {
            Packet::ACK(ack_block) => self.handle_ack(ack_block),
            Packet::DATA { block_num, data } => self.handle_data(block_num, data),
            Packet::ERROR { .. } => {
                // receiving an error kills the transfer
                TftpResult::Done(None)
            }
            _ => TftpResult::Err(TftpError::TransferAlreadyRunning),
        };
        if let TftpResult::Done(_) = result {
            *self = Complete;
        }
        result
    }

    fn handle_ack(&mut self, ack_block: u16) -> TftpResult {
        match *self {
            Tx(ref mut tx) => tx.handle_ack(ack_block),
            Rx(_) => {
                // wrong kind of packet, kill transfer
                TftpResult::Done(Some(ErrorCode::IllegalTFTP.into()))
            }
            Complete => TftpResult::Done(None),
        }
    }

    fn handle_data(&mut self, block_num: u16, data: Vec<u8>) -> TftpResult {
        match *self {
            Rx(ref mut rx) => rx.handle_data(block_num, data),
            Tx(_) => {
                // wrong kind of packet, kill transfer
                TftpResult::Done(Some(ErrorCode::IllegalTFTP.into()))
            }
            Complete => TftpResult::Done(None),
        }
    }
}

impl<R: Read> TransferTx<R> {
    fn handle_ack(&mut self, ack_block: u16) -> TftpResult {
        if ack_block == self.expected_block_num.wrapping_sub(1) {
            TftpResult::Repeat
        } else if ack_block != self.expected_block_num {
            TftpResult::Done(Some(Packet::ERROR {
                code: ErrorCode::UnknownID,
                msg: "Incorrect block num in ACK".to_owned(),
            }))
        } else if self.sent_final {
            TftpResult::Done(None)
        } else {
            let mut v = vec![];
            self.fread.read_512(&mut v).unwrap();
            self.sent_final = v.len() < 512;
            self.expected_block_num = self.expected_block_num.wrapping_add(1);
            TftpResult::Reply(Packet::DATA {
                block_num: self.expected_block_num,
                data: v,
            })
        }
    }
}

impl<W: Write> TransferRx<W> {
    fn handle_data(&mut self, block_num: u16, data: Vec<u8>) -> TftpResult {
        if block_num != self.expected_block_num {
            TftpResult::Done(Some(Packet::ERROR {
                code: ErrorCode::IllegalTFTP,
                msg: "Data packet lost".to_owned(),
            }))
        } else {
            self.fwrite.write_all(data.as_slice()).unwrap();
            self.expected_block_num = block_num.wrapping_add(1);
            if data.len() < 512 {
                TftpResult::Done(Some(Packet::ACK(block_num)))
            } else {
                TftpResult::Reply(Packet::ACK(block_num))
            }
        }
    }
}

pub struct IOPolicyCfg {
    pub readonly: bool,
    pub path: Option<String>,
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
    fn open_read(&self, filename: &str) -> io::Result<Self::R> {
        if filename.contains("..") || filename.starts_with('/') {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot read",
            ))
        } else if let Some(ref path) = self.policy.path {
            let full = path.to_owned() + "/" + filename;
            self.io.open_read(&full)
        } else {
            self.io.open_read(filename)
        }
    }

    fn create_new(&mut self, filename: &str) -> io::Result<Self::W> {
        if self.policy.readonly || filename.contains("..") || filename.starts_with('/') {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot write",
            ))
        } else if let Some(ref path) = self.policy.path {
            let full = path.to_owned() + "/" + filename;
            self.io.create_new(&full)
        } else {
            self.io.create_new(filename)
        }
    }
}
