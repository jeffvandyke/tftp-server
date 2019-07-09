#![deny(clippy::all)]
#![deny(clippy::pedantic)]

mod options;
pub mod packet;
mod tftp_server;
// Re-export all public types from tftp_server
// (Idea: export server's types directly?)
pub use tftp_server::*;
mod tftp_proto;

#[cfg(test)]
mod tftp_proto_tests;
