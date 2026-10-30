//! Turns a blocking channel into a tokio stream, for the platforms whose Bluetooth APIs
//! are not tokio based.
//!
//! Two threads move bytes between the channel and one end of an in-memory duplex
//! stream, whose other end is handed to the session. When either side closes, the
//! other is closed too.

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::runtime::Handle;

/// A channel with blocking reads and writes.
pub(crate) trait Channel: Send + 'static {
    /// Reads at least one byte, or returns 0 at the end of the stream.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize>;
}

/// The sending half of a [`Channel`].
pub(crate) trait Sender: Send + 'static {
    /// Writes all of `data`.
    fn write(&mut self, data: &[u8]) -> io::Result<()>;

    /// Closes the channel, ending a blocked read.
    fn close(&mut self);
}

/// Bridges a channel to a tokio stream, running on `runtime`.
pub(crate) fn bridge(
    runtime: Handle,
    mut channel: impl Channel,
    mut sender: impl Sender,
) -> DuplexStream {
    let (ours, theirs) = tokio::io::duplex(64 * 1024);
    let (mut from_app, mut to_app) = tokio::io::split(theirs);

    let incoming = runtime.clone();
    std::thread::Builder::new()
        .name("tidings-bt-read".to_string())
        .spawn(move || {
            let mut buffer = vec![0; 8192];
            loop {
                match channel.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if incoming
                            .block_on(to_app.write_all(&buffer[..read]))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        log::info!("Bluetooth channel closed: {}", error);
                        break;
                    }
                }
            }
            let _ = incoming.block_on(to_app.shutdown());
        })
        .expect("spawning a thread");

    std::thread::Builder::new()
        .name("tidings-bt-write".to_string())
        .spawn(move || {
            let mut buffer = vec![0; 8192];
            loop {
                match runtime.block_on(from_app.read(&mut buffer)) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if let Err(error) = sender.write(&buffer[..read]) {
                            log::info!("Bluetooth channel closed: {}", error);
                            break;
                        }
                    }
                }
            }
            sender.close();
        })
        .expect("spawning a thread");

    ours
}
