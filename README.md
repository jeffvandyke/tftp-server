tftp-server
===========

#### A [TFTP](https://tools.ietf.org/html/rfc1350) server implementation in Rust

Summary
----------
* Usable as both binary and library
* 100% safe code, no `unsafe` usage
* Well tested, including error cases
* Implements the RFCs describing extensions to the TFTP protocol

Building and running
-------------------------------

Simply use `cargo build`, and `cargo run` respectively. The server will start and serve from/into the current directory.

__Note__: By default the server listens on port 69 (as per [RFC1350](https://tools.ietf.org/html/rfc1350)), and that usually requires root privileges.

In order to run it on a different port, you can specify `--address`:

```
$ ./target/debug/tftp_server --address 0.0.0.0:1234
```

This will instead listen on port 1234.

To bind to a random port you can leave out the port number and only specify the address


Features
--------
All features are implemented in the library. The binary target is a only an argument-parsing thin wrapper over it for direct usage conveninence.

* `-a` or `--address` to specify an address[:port] to listen on (multiple supported)
* `-r` will make the server treat the served directory as read-only (it will reject all write requests)
* `-d` or `--directory` specifies the directory to serve from (the given path will be prepended to all requested paths)
* `-t` or `--timeout` specifies the default timeout (in seconds) for idle connections
* see TODO section below


TFTP Protocol Options & Extensions
---------------------
The following TFTP extension RFCs are implemented:
* [RFC 2347: TFTP Option Extension](https://tools.ietf.org/html/rfc2347)
* [RFC 2348: TFTP Blocksize Option](https://tools.ietf.org/html/rfc2348)
* [RFC 2349: TFTP Timeout Interval and Transfer Size Options](https://tools.ietf.org/html/rfc2349)
* [RFC 7440: TFTP Windowsize Option](https://tools.ietf.org/html/rfc7440)


Logging and Testing
-------------------

To run all tests, use `cargo test`.

You can also run the server (or tests) with logging enabled. To do this add `RUST_LOG=tftp_server=info` before the command.
For example:

```
$ RUST_LOG=tftp_server=info ./target/debug/tftp_server
```

This will run the server with logging enabled so that you can inspect the program's behavior.


TODOs
-----

* [ ] Documentation for individual items
* [ ] Crate-level documentation with an overview and examples
* [x] serve from specified directory, not just the current one
* [x] treat directory as readonly (reject write requests)
* [x] IPv6 support
* [x] multiple address support
* [ ] CLI switches for logging
* [ ] running control (ability to stop server hard or soft)
* [ ] limit accepted blocksize to stack MSS (smaller on ipv4)
* [x] complete implementation of all option extension RFCs
* [ ] redo packets as in-place buffer references to avoid copying memory
* [ ] redo integration tests to run them with harness
* [ ] make proto tests more orthogonal
* [ ] test that transfer size is enforced on Rx
* [ ] maybe eventually split off proto handling into its own crate
* [ ] implement congestion control when using window size
