use crate::packet::{Error as PacketError, ErrorCode, Packet, MAX_PACKET_SIZE};
use crate::tftp_proto::*;
use log::*;
use mio::net::UdpSocket;
use mio::*;
use mio_more::timer::{Timeout, Timer, TimerError};
use std::collections::HashMap;
use std::io;
use std::net::{self, IpAddr, SocketAddr};
use std::path::PathBuf;
use std::result;
use std::time::Duration;

/// The token used by the timer.
const TIMER: Token = Token(0);

#[derive(Debug)]
pub enum TftpError {
    Packet(PacketError),
    Io(io::Error),
    Timer(TimerError),
}

impl From<io::Error> for TftpError {
    fn from(err: io::Error) -> Self {
        TftpError::Io(err)
    }
}

impl From<PacketError> for TftpError {
    fn from(err: PacketError) -> Self {
        TftpError::Packet(err)
    }
}

impl From<TimerError> for TftpError {
    fn from(err: TimerError) -> Self {
        TftpError::Timer(err)
    }
}

pub type Result<T> = result::Result<T, TftpError>;

/// The state of an ongoing read/write connection with a client,
/// corresponding to a single read/write transfer
struct ConnectionState<IO: IOAdapter> {
    /// The UDP socket for the connection that receives ACK, DATA, or ERROR packets.
    socket: UdpSocket,
    /// The timeout for the last packet. Every time a new packet is received, the
    /// timeout is reset.
    timeout: Timeout,
    /// The protocol state associated with this transfer
    transfer: Transfer<IO>,
    /// The last packets sent.
    /// This is useful when packets have to be resent due to timeouts or other errors
    last_packets: Vec<Vec<u8>>,
    /// The address of the client socket to reply to.
    remote: SocketAddr,
}

/// Struct used to specify working configuration of a server
pub struct Config {
    /// Specifies that the server should reject write requests
    pub readonly: bool,
    /// The directory the server will serve from instead of the default
    pub dir: Option<PathBuf>,
    /// The IP addresses (and optionally ports) on which the server must listen
    pub addrs: Vec<(IpAddr, Option<u16>)>,
    /// The idle time until a connection with a client is closed
    pub timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            readonly: false,
            dir: None,
            addrs: vec![
                (IpAddr::from([127, 0, 0, 1]), Some(69)),
                (IpAddr::from([0; 16]), Some(69)),
            ],
            timeout: Duration::from_secs(3),
        }
    }
}

pub type TftpServer = ServerImpl<FSAdapter>;

pub struct ServerImpl<IO: IOAdapter> {
    /// The ID of a new token used for generating different tokens.
    new_token: Token,
    /// The event loop for handling async events.
    poll: Poll,
    /// The main timer that can be used to set multiple timeout events.
    timer: Timer<Token>,
    /// The connection timeout
    timeout: Duration,
    /// The main server socket that receives RRQ and WRQ packets
    /// and creates a new separate UDP connection.
    server_sockets: HashMap<Token, UdpSocket>,
    /// The separate UDP connections for handling multiple requests.
    connections: HashMap<Token, ConnectionState<IO>>,
    /// The TFTP protocol state machine and filesystem accessor
    proto_handler: TftpServerProto<IO>,
}

impl<IO: IOAdapter + Default> ServerImpl<IO> {
    /// Creates a new TFTP server from a random open UDP port.
    pub fn new() -> Result<Self> {
        Self::with_cfg(&Config::default())
    }

    /// Creates a new TFTP server from the provided config
    pub fn with_cfg(cfg: &Config) -> Result<Self> {
        if cfg.addrs.is_empty() {
            return Err(TftpError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "address list empty; nothing to listen on",
            )));
        }

        let poll = Poll::new()?;
        let timer = Timer::default();
        poll.register(
            &timer,
            TIMER,
            Ready::readable(),
            PollOpt::edge() | PollOpt::level(),
        )?;

        let mut server_sockets = HashMap::new();
        let mut new_token = Token(1); // skip timer token
        for &(ip, port) in &cfg.addrs {
            let socket = make_bound_socket(ip, port)?;
            poll.register(
                &socket,
                new_token,
                Ready::readable(),
                PollOpt::edge() | PollOpt::level(),
            )?;
            server_sockets.insert(new_token, socket);
            new_token.0 += 1;
        }

        info!(
            "Server listening on {:?}",
            server_sockets
                .iter()
                .map(|(_, socket)| format!("{}", socket.local_addr().unwrap()))
                .collect::<Vec<_>>()
        );

        Ok(Self {
            new_token,
            poll,
            timer,
            timeout: cfg.timeout,
            server_sockets,
            connections: HashMap::new(),
            proto_handler: TftpServerProto::new(
                Default::default(),
                IOPolicyCfg {
                    readonly: cfg.readonly,
                    path: cfg.dir.clone(),
                },
            ),
        })
    }

    /// Returns a new token created from incrementing a counter.
    fn generate_token(&mut self) -> Token {
        use std::usize;
        if self
            .connections
            .len()
            .saturating_add(self.server_sockets.len())
            .saturating_add(1 /* timer token */)
            == usize::max_value()
        {
            panic!("no more tokens, but impressive amount of memory");
        }
        while self.new_token == TIMER
            || self.server_sockets.contains_key(&self.new_token)
            || self.connections.contains_key(&self.new_token)
        {
            self.new_token.0 = self.new_token.0.wrapping_add(1);
        }
        self.new_token
    }

    /// Cancels a connection given the connection's token. It cancels the
    /// connection's timeout and deregisters the connection's socket from the event loop.
    fn cancel_connection(&mut self, token: Token) -> Result<()> {
        if let Some(conn) = self.connections.remove(&token) {
            info!("Closing connection with token {:?}", token);
            self.poll.deregister(&conn.socket)?;
            self.timer.cancel_timeout(&conn.timeout);
        }
        Ok(())
    }

    /// Resets a connection's timeout given the connection's token.
    fn reset_timeout(&mut self, token: Token) -> Result<()> {
        if let Some(ref mut conn) = self.connections.get_mut(&token) {
            self.timer.cancel_timeout(&conn.timeout);
            conn.timeout = self
                .timer
                .set_timeout(conn.transfer.timeout().unwrap_or(self.timeout), token)?;
        }
        Ok(())
    }

    /// Creates a new UDP connection from the provided arguments
    fn create_connection(
        &mut self,
        token: Token,
        socket: UdpSocket,
        transfer: Transfer<IO>,
        packet: &[u8],
        remote: SocketAddr,
    ) -> Result<()> {
        let timeout = self
            .timer
            .set_timeout(transfer.timeout().unwrap_or(self.timeout), token)?;
        self.poll.register(
            &socket,
            token,
            Ready::readable(),
            PollOpt::edge() | PollOpt::level(),
        )?;

        self.connections.insert(
            token,
            ConnectionState {
                socket,
                timeout,
                transfer,
                last_packets: vec![packet.to_vec()],
                remote,
            },
        );

        info!("Created connection with token: {:?}", token);

        Ok(())
    }

    /// Handles the event when a timer times out.
    /// It gets the connection from the token and resends
    /// the last packet sent from the connection.
    /// If the transfer associated with that connection is over,
    /// it instead kills the connection.
    fn process_timer(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut tokens = vec![];
        while let Some(token) = self.timer.poll() {
            tokens.push(token);
        }

        for token in tokens {
            let status = if let Some(ref mut conn) = self.connections.get_mut(&token) {
                match conn.transfer.timeout_expired() {
                    ResponseItem::Packet(packet) => {
                        let amt_written = packet.write_to_slice(buf)?;
                        let sent = Vec::from(&buf[..amt_written]);
                        conn.socket.send_to(&sent, &conn.remote)?;
                        conn.last_packets = vec![sent];

                        Some(Ok(()))
                    }
                    ResponseItem::RepeatLast(count) => {
                        let skipped = conn.last_packets.len().saturating_sub(count);
                        for pkt in conn.last_packets.iter().skip(skipped) {
                            conn.socket.send_to(pkt, &conn.remote)?;
                        }
                        Some(Ok(()))
                    }
                    ResponseItem::Done => Some(Err(())),
                }
            } else {
                None
            };

            match status {
                Some(Ok(_)) => self.reset_timeout(token)?,
                Some(Err(_)) => self.cancel_connection(token)?,
                _ => {}
            }
        }

        Ok(())
    }

    /// Called to process an available I/O event for a token.
    /// Normally these correspond to packets received on a socket or to a timeout
    fn handle_token(&mut self, token: Token, buf: &mut [u8]) -> Result<()> {
        match token {
            TIMER => self.process_timer(buf),
            _ if self.server_sockets.contains_key(&token) => self.handle_server_packet(token, buf),
            _ => self.handle_connection_packet(token, buf),
        }
    }

    fn handle_server_packet(&mut self, token: Token, buf: &mut [u8]) -> Result<()> {
        let (local_ip, amt, src) = {
            let socket = match self.server_sockets.get(&token) {
                Some(socket) => socket,
                None => {
                    error!("Invalid server token");
                    return Ok(());
                }
            };
            let (amt, src) = socket.recv_from(buf)?;
            (socket.local_addr()?.ip(), amt, src)
        };
        let packet = Packet::read(&buf[..amt])?;

        let new_conn_token = self.generate_token();
        let (xfer, res) = self.proto_handler.rx_initial(packet);
        let reply_packet = match res {
            Err(e) => {
                error!("{:?}", e);
                return Ok(());
            }
            Ok(packet) => packet,
        };

        let socket = make_bound_socket(local_ip, None)?;

        // send packet back for all cases
        let amt_written = reply_packet.write_to_slice(buf)?;
        socket.send_to(&buf[..amt_written], &src)?;

        if let Some(xfer) = xfer {
            self.create_connection(new_conn_token, socket, xfer, &buf[..amt_written], src)?;
        }

        Ok(())
    }

    fn handle_connection_packet(&mut self, token: Token, buf: &mut [u8]) -> Result<()> {
        self.reset_timeout(token)?;
        let conn = if let Some(conn) = self.connections.get_mut(&token) {
            conn
        } else {
            error!("No connection with token {:?}", token);
            return Ok(());
        };

        let (amt, src) = conn.socket.recv_from(buf)?;

        if conn.remote != src {
            // packet from somehere else, reply with error
            let amt_written = Packet::from(ErrorCode::UnknownID).write_to_slice(buf)?;
            conn.socket.send_to(&buf[..amt_written], &conn.remote)?;
            return Ok(());
        }
        let packet = Packet::read(&buf[..amt])?;

        let response = match conn.transfer.rx(packet) {
            Ok(resp) => resp,
            Err(e) => {
                error!("{:?}", e);
                return Ok(());
            }
        };

        let mut sent_packets = vec![];
        for item in response {
            match item {
                ResponseItem::Done => break,
                ResponseItem::Packet(packet) => {
                    let amt_written = packet.write_to_slice(buf)?;
                    let sent = Vec::from(&buf[..amt_written]);
                    conn.socket.send_to(&sent, &conn.remote)?;
                    sent_packets.push(sent);
                }
                ResponseItem::RepeatLast(count) => {
                    let skipped = conn.last_packets.len().saturating_sub(count);
                    for pkt in conn.last_packets.iter().skip(skipped) {
                        conn.socket.send_to(pkt, &conn.remote)?;
                    }
                }
            }
        }
        conn.last_packets = sent_packets;

        Ok(())
    }

    /// Runs the server's event loop.
    pub fn run(&mut self) -> Result<()> {
        let mut events = Events::with_capacity(1024);
        let mut scratch_buf = vec![0; MAX_PACKET_SIZE];

        loop {
            self.poll.poll(&mut events, None)?;

            for event in events.iter() {
                match self.handle_token(event.token(), &mut scratch_buf) {
                    Ok(_) | Err(TftpError::Io(_)) => { /* swallow Io errors */ }
                    Err(TftpError::Packet(_)) => {
                        error!("malformed packet");
                    }
                    e => return e,
                }
            }
        }
    }

    /// Stores the local addresses in the provided vec
    pub fn get_local_addrs(&self, bag: &mut Vec<SocketAddr>) -> Result<()> {
        for socket in self.server_sockets.values() {
            bag.push(socket.local_addr()?);
        }
        Ok(())
    }
}

fn make_bound_socket(ip: IpAddr, port: Option<u16>) -> Result<UdpSocket> {
    let socket = net::UdpSocket::bind((ip, port.unwrap_or(0)))?;

    socket.set_nonblocking(true)?;

    Ok(UdpSocket::from_socket(socket)?)
}
