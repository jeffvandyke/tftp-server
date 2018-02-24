use std::io::{self, Write};

pub const MAX_BLOCKSIZE: u16 = 65_464;

#[derive(PartialEq, Clone, Debug)]
pub enum TftpOption {
    Blocksize(u16),
    TransferSize(u64),
}

impl TftpOption {
    pub fn write_to(&self, buf: &mut Write) -> io::Result<()> {
        use self::TftpOption::*;
        match *self {
            Blocksize(size) => {
                write!(buf, "blksize\0{}\0", size)?;
            }
            TransferSize(_) => {
            }
        };
        Ok(())
    }

    pub fn try_from(name: &str, value: &str) -> Option<Self> {
        if "blksize".eq_ignore_ascii_case(name) {
            let val = value.parse::<u16>().ok()?;
            if val >= 8 && val <= MAX_BLOCKSIZE {
                return Some(TftpOption::Blocksize(val));
            }
        } else if "tsize".eq_ignore_ascii_case(name) {
            let val = value.parse().ok()?;
            return Some(TftpOption::TransferSize(val));
        }
        None
    }
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
        assert_eq!(
            TftpOption::try_from("bLkSIzE", "512"),
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
        assert_eq!(MAX_BLOCKSIZE, 65_464);
        assert_eq!(
            TftpOption::try_from("blksize", "65464"),
            Some(TftpOption::Blocksize(65_464))
        );
        assert_eq!(TftpOption::try_from("blksize", "65465"), None);
    }

    #[test]
    fn transfer_size_parse() {
        assert_eq!(
            TftpOption::try_from("tsize", "56246"),
            Some(TftpOption::TransferSize(56246))
        );
        assert_eq!(
            TftpOption::try_from("tSiZE", "0"),
            Some(TftpOption::TransferSize(0))
        );
    }
}
