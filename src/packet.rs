use std::{io, result, str};
use std::io::Write;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use read_512::Read512;

#[derive(Debug)]
pub enum PacketErr {
    StrOutOfBounds,
    OpCodeOutOfBounds,
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
    }
);

impl ErrorCode {
    /// Returns the string description of the error code.
    pub fn to_string(&self) -> String {
        (match *self {
            ErrorCode::NotDefined => "Not defined, see error message (if any).",
            ErrorCode::FileNotFound => "File not found.",
            ErrorCode::AccessViolation => "Access violation.",
            ErrorCode::DiskFull => "Disk full or allocation exceeded.",
            ErrorCode::IllegalTFTP => "Illegal TFTP operation.",
            ErrorCode::UnknownID => "Unknown transfer ID.",
            ErrorCode::FileExists => "File already exists.",
            ErrorCode::NoUser => "No such user.",
        }).to_string()
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

pub const MAX_PACKET_SIZE: usize = 1024;

#[derive(PartialEq, Clone, Debug)]
pub enum Packet {
    RRQ {
        filename: String,
        mode: String,
        options: Vec<TftpOption>,
    },
    WRQ {
        filename: String,
        mode: String,
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
}

#[derive(PartialEq, Clone, Debug)]
pub enum TftpOption {
    Blocksize(u16),
}

impl TftpOption {
    fn write_to(&self, buf: &mut Write) -> io::Result<()> {
        use packet::TftpOption::*;
        match self {
            &Blocksize(size) => {
                buf.write_all(b"blksize\0")?;
                write!(buf, "{}\0", size)?;
            }
        };
        Ok(())
    }

    fn try_from(name: &str, value: &str) -> Option<Self> {
        match name {
            "blksize" => {
                let val = value.parse::<u16>().ok()?;
                if val >= 8 && val <= 65_464 {
                    Some(TftpOption::Blocksize(val))
                } else {
                    None
                }
            }
            _ => None,
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

    fn write_bytes_to(&self, buf: &mut Write) -> Result<()> {
        match *self {
            Packet::RRQ {
                ref filename,
                ref mode,
                ref options,
            } => r_packet_bytes(OpCode::RRQ, filename, mode, options, buf),
            Packet::WRQ {
                ref filename,
                ref mode,
            } => w_packet_bytes(OpCode::WRQ, filename, mode, buf),
            Packet::DATA {
                block_num,
                ref data,
            } => data_packet_bytes(block_num, data.as_slice(), buf),
            Packet::ACK(block_num) => ack_packet_bytes(block_num, buf),
            Packet::ERROR { code, ref msg } => error_packet_bytes(code, msg, buf),
        }
    }
}

/// Reads until the zero byte and returns a string containing the bytes read
/// and the rest of the buffer, skipping the zero byte
fn read_string(bytes: &[u8]) -> Result<(String, &[u8])> {
    let result_bytes = bytes
        .iter()
        .take_while(|c| **c != 0)
        .cloned()
        .collect::<Vec<u8>>();
    // TODO: add test for error condition below
    if result_bytes.len() == bytes.len() {
        // reading didn't stop on a zero byte
        return Err(PacketErr::StrOutOfBounds);
    }

    let result_str = str::from_utf8(result_bytes.as_slice())?.to_string();
    let (_, tail) = bytes.split_at(result_bytes.len() + 1 /* +1 so we skip the \0 byte*/);
    Ok((result_str, tail))
}

fn read_rrq_packet(bytes: &[u8]) -> Result<Packet> {
    let (filename, rest) = read_string(bytes)?;
    let (mode, rest) = read_string(rest)?;

    let mut bytes = rest;
    let mut options = vec![];
    loop {
        // errors ignored while parsing options
        let (opt, rest) = match read_string(bytes) {
            Ok(v) => v,
            _ => break,
        };
        let (value, rest) = match read_string(rest) {
            Ok(v) => v,
            _ => break,
        };
        bytes = rest;
        if let Some(opt) = TftpOption::try_from(&opt, &value) {
            options.push(opt);
        }
    }
    Ok(Packet::RRQ {
        filename,
        mode,
        options,
    })
}

fn read_wrq_packet(bytes: &[u8]) -> Result<Packet> {
    let (filename, rest) = read_string(bytes)?;
    let (mode, _) = read_string(rest)?;

    Ok(Packet::WRQ { filename, mode })
}

fn read_data_packet(mut bytes: &[u8]) -> Result<Packet> {
    let block_num = bytes.read_u16::<BigEndian>()?;
    let mut data = Vec::with_capacity(512);
    // TODO: test with longer packets
    bytes.read_512(&mut data)?;

    Ok(Packet::DATA { block_num, data })
}

fn read_ack_packet(mut bytes: &[u8]) -> Result<Packet> {
    let block_num = bytes.read_u16::<BigEndian>()?;
    Ok(Packet::ACK(block_num))
}

fn read_error_packet(mut bytes: &[u8]) -> Result<Packet> {
    let code = ErrorCode::from_u16(bytes.read_u16::<BigEndian>()?)?;
    let (msg, _) = read_string(bytes)?;

    Ok(Packet::ERROR { code, msg })
}

fn r_packet_bytes(
    packet: OpCode,
    filename: &str,
    mode: &str,
    options: &[TftpOption],
    buf: &mut Write,
) -> Result<()> {
    buf.write_u16::<BigEndian>(packet as u16)?;
    buf.write_all(filename.as_bytes())?;
    buf.write_all(&[0])?;
    buf.write_all(mode.as_bytes())?;
    buf.write_all(&[0])?;

    for opt in options {
        opt.write_to(buf)?;
    }

    Ok(())
}

fn w_packet_bytes(packet: OpCode, filename: &str, mode: &str, buf: &mut Write) -> Result<()> {
    buf.write_u16::<BigEndian>(packet as u16)?;
    buf.write_all(filename.as_bytes())?;
    buf.write_all(&[0])?;
    buf.write_all(mode.as_bytes())?;
    buf.write_all(&[0])?;

    Ok(())
}

fn data_packet_bytes(block_num: u16, data: &[u8], buf: &mut Write) -> Result<()> {
    buf.write_u16::<BigEndian>(OpCode::DATA as u16)?;
    buf.write_u16::<BigEndian>(block_num)?;
    buf.write_all(data)?;

    Ok(())
}

fn ack_packet_bytes(block_num: u16, buf: &mut Write) -> Result<()> {
    buf.write_u16::<BigEndian>(OpCode::ACK as u16)?;
    buf.write_u16::<BigEndian>(block_num)?;

    Ok(())
}

fn error_packet_bytes(code: ErrorCode, msg: &str, buf: &mut Write) -> Result<()> {
    buf.write_u16::<BigEndian>(OpCode::ERROR as u16)?;
    buf.write_u16::<BigEndian>(code as u16)?;
    buf.write_all(msg.as_bytes())?;
    buf.write_all(&[0])?;

    Ok(())
}

#[cfg(test)]
mod option {
    use super::*;

    #[test]
    fn blocksize_parse() {
        assert_eq!(
            TftpOption::try_from("blksize", "512"),
            Some(TftpOption::Blocksize(512))
        );
        assert_eq!(TftpOption::try_from("blksize", "cat"), None);
        assert_eq!(TftpOption::try_from("blocksize", "512"), None);
    }

    #[test]
    fn blocksize_bounds() {
        assert_eq!(TftpOption::try_from("blksize", "7"), None);
        assert_eq!(
            TftpOption::try_from("blksize", "8"),
            Some(TftpOption::Blocksize(8))
        );
        assert_eq!(
            TftpOption::try_from("blksize", "65464"),
            Some(TftpOption::Blocksize(65_464))
        );
        assert_eq!(TftpOption::try_from("blksize", "65465"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    macro_rules! test_read_string {
    ($name:ident, $bytes:expr, $start_pos:expr, $string:expr, $end_pos:expr) => {
        #[test]
        fn $name() {
            let mut bytes = [0; MAX_PACKET_SIZE];
            let seed_bytes = $bytes.chars().collect::<Vec<_>>();
            for i in 0..$bytes.len() {
                bytes[i] = seed_bytes[i] as u8;
            }

            let result = read_string(&bytes[$start_pos..seed_bytes.len()]);
            assert!(result.is_ok());
            let _ = result.map(|(string, rest)| {
                assert_eq!(string, $string);
                assert_eq!(seed_bytes.len() - rest.len(), $end_pos);
            });
        }
    };
    }

    test_read_string!(read_string_normal, "hello world!\0", 0, "hello world!", 13);
    test_read_string!(
        read_string_zero_in_mid,
        "hello wor\0ld!",
        0,
        "hello wor",
        10
    );
    test_read_string!(
        read_string_diff_start_pos,
        "hello world!\0",
        6,
        "world!",
        13
    );

    macro_rules! packet_enc_dec_test {
        ($name:ident, $packet:expr) => {
            #[test]
            fn $name() {
                let bytes = $packet.clone().into_bytes();
                assert!(bytes.is_ok());
                let packet = bytes.and_then(|pd| Packet::read(pd.as_slice()));
                assert!(packet.is_ok());
                let _ = packet.map(|packet| { assert_eq!(packet, $packet); });
            }
        };
    }

    const BYTE_DATA: [u8; 512] = [123; 512];

    packet_enc_dec_test!(
        rrq,
        Packet::RRQ {
            filename: "/a/b/c/hello.txt".to_string(),
            mode: "netascii".to_string(),
            options: vec![],
        }
    );
    packet_enc_dec_test!(
        rrq_blocksize,
        Packet::RRQ {
            filename: "/a/b/c/hello.txt".to_string(),
            mode: "netascii".to_string(),
            options: vec![TftpOption::Blocksize(735)],
        }
    );
    packet_enc_dec_test!(
        wrq,
        Packet::WRQ {
            filename: "./world.txt".to_string(),
            mode: "octet".to_string(),
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
}
