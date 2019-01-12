pub use crate::options::*;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::io::Write;
use std::{io, result, str};

#[derive(Debug)]
pub enum PacketErr {
    StrOutOfBounds,
    OpCodeOutOfBounds,
    UnsupportedField,
    Utf8Error(str::Utf8Error),
    IOError(io::Error),
}

impl From<str::Utf8Error> for PacketErr {
    fn from(err: str::Utf8Error) -> PacketErr {
        PacketErr::Utf8Error(err)
    }
}

impl From<io::Error> for PacketErr {
    fn from(err: io::Error) -> PacketErr {
        PacketErr::IOError(err)
    }
}

pub type Result<T> = result::Result<T, PacketErr>;

macro_rules! primitive_enum {
    (
        $( #[$enum_attr:meta] )*
        pub enum $enum_name:ident of $base_int:tt {
            $( $variant:ident = $value:expr, )+
        }
    ) => {
        $( #[$enum_attr] )*
        #[repr($base_int)]
        pub enum $enum_name {
            $( $variant = $value, )+
        }

        // TODO: change this to a From<u16> impl
        impl $enum_name {
            fn from_u16(i: $base_int) -> Result<$enum_name> {
                match i {
                    $( $value => Ok($enum_name::$variant), )+
                    _ => Err(PacketErr::OpCodeOutOfBounds)
                }
            }
        }
    }
}

primitive_enum! (
    #[derive(PartialEq, Copy, Clone, Debug)]
    pub enum OpCode of u16 {
        RRQ = 1,
        WRQ = 2,
        DATA = 3,
        ACK = 4,
        ERROR = 5,
        OACK = 6,
    }
);

primitive_enum! (
    #[derive(PartialEq, Clone, Copy, Debug)]
    pub enum ErrorCode of u16 {
        NotDefined = 0,
        FileNotFound = 1,
        AccessViolation = 2,
        DiskFull = 3,
        IllegalTFTP = 4,
        UnknownID = 5,
        FileExists = 6,
        NoUser = 7,
        BadOption = 8,
    }
);

impl ErrorCode {
    /// Returns the string description of the error code.
    pub fn to_string(self) -> String {
        match self {
            ErrorCode::NotDefined => "Not defined, see error message (if any).",
            ErrorCode::FileNotFound => "File not found.",
            ErrorCode::AccessViolation => "Access violation.",
            ErrorCode::DiskFull => "Disk full or allocation exceeded.",
            ErrorCode::IllegalTFTP => "Illegal TFTP operation.",
            ErrorCode::UnknownID => "Unknown transfer ID.",
            ErrorCode::FileExists => "File already exists.",
            ErrorCode::NoUser => "No such user.",
            ErrorCode::BadOption => "Bad option.",
        }
        .to_owned()
    }
}

impl From<ErrorCode> for Packet {
    /// Returns the ERROR packet with the error code and
    /// the default description as the error message.
    fn from(code: ErrorCode) -> Packet {
        let msg = code.to_string();
        Packet::ERROR { code, msg }
    }
}

pub const MAX_PACKET_SIZE: usize = MAX_BLOCKSIZE as usize + 2/*opcode size*/;

#[derive(PartialEq, Clone, Debug)]
pub enum Packet {
    RRQ {
        filename: String,
        mode: TransferMode,
        options: Vec<TftpOption>,
    },
    WRQ {
        filename: String,
        mode: TransferMode,
        options: Vec<TftpOption>,
    },
    DATA {
        block_num: u16,
        data: Vec<u8>,
    },
    ACK(u16),
    ERROR {
        code: ErrorCode,
        msg: String,
    },
    OACK {
        options: Vec<TftpOption>,
    },
}

#[derive(PartialEq, Copy, Clone, Debug)]
pub enum TransferMode {
    Octet,
    Mail,
    Netascii,
}

impl TransferMode {
    fn try_from(s: &str) -> Result<Self> {
        use self::TransferMode::*;
        if "octet".eq_ignore_ascii_case(s) {
            Ok(Octet)
        } else if "netascii".eq_ignore_ascii_case(s) {
            Ok(Netascii)
        } else if "mail".eq_ignore_ascii_case(s) {
            Ok(Mail)
        } else {
            Err(PacketErr::UnsupportedField)
        }
    }
}

use std::fmt;
impl fmt::Display for TransferMode {
    fn fmt(&self, f: &mut fmt::Formatter) -> result::Result<(), fmt::Error> {
        use self::TransferMode::*;
        match *self {
            Octet => write!(f, "octet"),
            Mail => write!(f, "mail"),
            Netascii => write!(f, "netascii"),
        }
    }
}

impl Packet {
    /// Creates and returns a packet parsed from its byte representation.
    pub fn read(mut bytes: &[u8]) -> Result<Packet> {
        let opcode = OpCode::from_u16(bytes.read_u16::<BigEndian>()?)?;
        match opcode {
            OpCode::RRQ => read_rrq_packet(bytes),
            OpCode::WRQ => read_wrq_packet(bytes),
            OpCode::DATA => read_data_packet(bytes),
            OpCode::ACK => read_ack_packet(bytes),
            OpCode::ERROR => read_error_packet(bytes),
            OpCode::OACK => read_oack_packet(bytes),
        }
    }

    /// Consumes the packet and returns the packet in byte representation.
    pub fn into_bytes(self) -> Result<Vec<u8>> {
        self.to_bytes()
    }

    /// Returns a buffer containing the packet's byte representation
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(MAX_PACKET_SIZE);
        self.write_bytes_to(&mut buf)?;
        Ok(buf)
    }

    /// Writes the packet bytes to the give slice, returning the amount of bytes written
    pub fn write_to_slice(&self, sl: &mut [u8]) -> Result<usize> {
        let left = {
            let mut buf = sl.split_at_mut(0).1;
            self.write_bytes_to(&mut buf)?;
            buf.len()
        };
        Ok(sl.len() - left)
    }

    fn write_bytes_to(&self, buf: &mut impl Write) -> Result<()> {
        match *self {
            Packet::RRQ {
                ref filename,
                mode,
                ref options,
            } => rw_packet_bytes(OpCode::RRQ, filename, mode, options, buf),
            Packet::WRQ {
                ref filename,
                mode,
                ref options,
            } => rw_packet_bytes(OpCode::WRQ, filename, mode, options, buf),
            Packet::DATA {
                block_num,
                ref data,
            } => data_packet_bytes(block_num, data.as_slice(), buf),
            Packet::ACK(block_num) => ack_packet_bytes(block_num, buf),
            Packet::ERROR { code, ref msg } => error_packet_bytes(code, msg, buf),
            Packet::OACK { ref options } => oack_packet_bytes(options, buf),
        }
    }
}

use self::strings::Strings;
mod strings {
    /// Interprets a buffer as a series of null-terminated UTF-8 strings,
    /// and iterates over them in order
    pub struct Strings<'a> {
        bytes: &'a [u8],
    }
    impl<'a> From<&'a [u8]> for Strings<'a> {
        fn from(bytes: &'a [u8]) -> Self {
            Self { bytes }
        }
    }
    impl<'a> Iterator for Strings<'a> {
        type Item = &'a str;

        fn next(&mut self) -> Option<Self::Item> {
            let zero = self.bytes.iter().position(|c| *c == 0)?;
            let s = ::std::str::from_utf8(&self.bytes[..zero]);
            self.bytes = self.bytes.split_at(zero + 1).1;
            s.ok()
        }
    }

    #[test]
    fn simple() {
        let a: &[u8] = b"hello\0";
        let mut s = Strings::from(a);
        assert_eq!(s.next(), Some("hello"));
        assert_eq!(s.next(), None);
    }
    #[test]
    fn two() {
        let a: &[u8] = b"hello\0world\0";
        let mut s = Strings::from(a);
        assert_eq!(s.next(), Some("hello"));
        assert_eq!(s.next(), Some("world"));
        assert_eq!(s.next(), None);
    }
    #[test]
    fn junk() {
        let a: &[u8] = b"hello\0dude";
        let mut s = Strings::from(a);
        assert_eq!(s.next(), Some("hello"));
        assert_eq!(s.next(), None);
        assert_eq!(s.next(), None);
    }
}

fn read_rrq_packet(bytes: &[u8]) -> Result<Packet> {
    use self::PacketErr::StrOutOfBounds;
    if bytes.len() > 512 {
        Err(StrOutOfBounds)?;
    }
    let mut strings = Strings::from(bytes);

    let filename = strings.next().ok_or(StrOutOfBounds)?.to_owned();
    let mode = TransferMode::try_from(strings.next().ok_or(StrOutOfBounds)?)?;
    let options = read_options(strings);

    Ok(Packet::RRQ {
        filename,
        mode,
        options,
    })
}

fn read_wrq_packet(bytes: &[u8]) -> Result<Packet> {
    use self::PacketErr::StrOutOfBounds;
    if bytes.len() > 512 {
        Err(StrOutOfBounds)?;
    }
    let mut strings = Strings::from(bytes);

    let filename = strings.next().ok_or(StrOutOfBounds)?.to_owned();
    let mode = TransferMode::try_from(strings.next().ok_or(StrOutOfBounds)?)?;
    let options = read_options(strings);

    Ok(Packet::WRQ {
        filename,
        mode,
        options,
    })
}

fn read_options(mut strings: Strings) -> Vec<TftpOption> {
    let mut options = vec![];

    // errors ignored while parsing options
    while let (Some(opt), Some(value)) = (strings.next(), strings.next()) {
        if let Some(opt) = TftpOption::try_from(opt, value) {
            options.push(opt);
        }
    }

    options
}

fn read_data_packet(mut bytes: &[u8]) -> Result<Packet> {
    let block_num = bytes.read_u16::<BigEndian>()?;
    let mut data = Vec::with_capacity(512);
    use std::io::Read;
    bytes.read_to_end(&mut data)?;

    Ok(Packet::DATA { block_num, data })
}

fn read_ack_packet(mut bytes: &[u8]) -> Result<Packet> {
    let block_num = bytes.read_u16::<BigEndian>()?;
    Ok(Packet::ACK(block_num))
}

fn read_error_packet(mut bytes: &[u8]) -> Result<Packet> {
    let code = ErrorCode::from_u16(bytes.read_u16::<BigEndian>()?)?;
    let mut strings = Strings::from(bytes);
    let msg = strings.next().ok_or(PacketErr::StrOutOfBounds)?.to_owned();

    Ok(Packet::ERROR { code, msg })
}

fn read_oack_packet(bytes: &[u8]) -> Result<Packet> {
    let strings = Strings::from(bytes);
    let options = read_options(strings);

    Ok(Packet::OACK { options })
}

fn rw_packet_bytes(
    packet: OpCode,
    filename: &str,
    mode: TransferMode,
    options: &[TftpOption],
    buf: &mut impl Write,
) -> Result<()> {
    buf.write_u16::<BigEndian>(packet as u16)?;
    write!(buf, "{}\0{}\0", filename, mode)?;

    for opt in options {
        opt.write_to(buf)?;
    }

    Ok(())
}

fn data_packet_bytes(block_num: u16, data: &[u8], buf: &mut impl Write) -> Result<()> {
    buf.write_u16::<BigEndian>(OpCode::DATA as u16)?;
    buf.write_u16::<BigEndian>(block_num)?;
    buf.write_all(data)?;

    Ok(())
}

fn ack_packet_bytes(block_num: u16, buf: &mut impl Write) -> Result<()> {
    buf.write_u16::<BigEndian>(OpCode::ACK as u16)?;
    buf.write_u16::<BigEndian>(block_num)?;

    Ok(())
}

fn error_packet_bytes(code: ErrorCode, msg: &str, buf: &mut impl Write) -> Result<()> {
    buf.write_u16::<BigEndian>(OpCode::ERROR as u16)?;
    buf.write_u16::<BigEndian>(code as u16)?;
    write!(buf, "{}\0", msg)?;

    Ok(())
}

fn oack_packet_bytes(options: &[TftpOption], buf: &mut impl Write) -> Result<()> {
    buf.write_u16::<BigEndian>(OpCode::OACK as u16)?;

    for opt in options {
        opt.write_to(buf)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::*;

    #[test]
    fn wrq_max_size() {
        let p = Packet::WRQ {
            filename: str::from_utf8(&[b'x'; 512 - 6]).unwrap().to_owned(),
            mode: TransferMode::Octet,
            options: vec![],
        };
        let mut v = vec![];
        p.write_bytes_to(&mut v).unwrap();
        assert_matches!(Packet::read(&v), Err(_));
    }

    #[test]
    fn rrq_max_size() {
        let p = Packet::RRQ {
            filename: str::from_utf8(&[b'x'; 512 - 6]).unwrap().to_owned(),
            mode: TransferMode::Octet,
            options: vec![],
        };
        let mut v = vec![];
        p.write_bytes_to(&mut v).unwrap();
        assert_matches!(Packet::read(&v), Err(_));
    }

    macro_rules! packet_enc_dec_test {
        ($name:ident, $packet:expr) => {
            #[test]
            fn $name() {
                let bytes = $packet.clone().into_bytes();
                assert!(bytes.is_ok());
                let packet = bytes.and_then(|pd| Packet::read(pd.as_slice()));
                assert!(packet.is_ok());
                let _ = packet.map(|packet| {
                    assert_eq!(packet, $packet);
                });
            }
        };
    }

    const BYTE_DATA: [u8; 512] = [123; 512];

    packet_enc_dec_test!(
        rrq,
        Packet::RRQ {
            filename: "/a/b/c/hello.txt".to_string(),
            mode: TransferMode::Netascii,
            options: vec![],
        }
    );
    packet_enc_dec_test!(
        rrq_blocksize,
        Packet::RRQ {
            filename: "/a/b/c/hello.txt".to_string(),
            mode: TransferMode::Netascii,
            options: vec![TftpOption::Blocksize(735)],
        }
    );
    packet_enc_dec_test!(
        wrq,
        Packet::WRQ {
            filename: "./world.txt".to_string(),
            mode: TransferMode::Octet,
            options: vec![],
        }
    );
    packet_enc_dec_test!(
        wrq_blocksize,
        Packet::WRQ {
            filename: "./world.txt".to_string(),
            mode: TransferMode::Octet,
            options: vec![TftpOption::Blocksize(846)],
        }
    );
    packet_enc_dec_test!(ack, Packet::ACK(1234));
    packet_enc_dec_test!(
        data,
        Packet::DATA {
            block_num: 1234,
            data: Vec::from(&BYTE_DATA[..]),
        }
    );
    packet_enc_dec_test!(
        err,
        Packet::ERROR {
            code: ErrorCode::NoUser,
            msg: "This is a message".to_string(),
        }
    );
    packet_enc_dec_test!(
        oack,
        Packet::OACK {
            options: vec![TftpOption::Blocksize(1234)],
        }
    );
}
