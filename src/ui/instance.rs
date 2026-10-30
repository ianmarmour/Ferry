//! One Tidings at a time: launching it again shows the window of the one running,
//! which matters once it keeps running in the tray.

use super::tray::Waker;

/// Becomes the running instance, or asks the running one to show its window and
/// returns `false`.
#[cfg(unix)]
pub fn claim(name: &str, waker: Waker) -> bool {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    use super::tray::Wake;

    let directory = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let user = std::env::var("USER").unwrap_or_default();
    let path = directory.join(format!("{}-{}.sock", name, user));

    if let Ok(mut running) = UnixStream::connect(&path) {
        if running.write_all(b"open\n").is_ok() {
            return false;
        }
    }

    let _ = std::fs::remove_file(&path);
    match UnixListener::bind(&path) {
        Ok(listener) => {
            let watching = std::thread::Builder::new()
                .name("tidings-instance".to_string())
                .spawn(move || {
                    for stream in listener.incoming().flatten() {
                        let mut line = String::new();
                        let read = BufReader::new(stream).read_line(&mut line);
                        if read.is_ok() && line.trim() == "open" {
                            waker.wake(Wake::Open);
                        }
                    }
                });
            if let Err(error) = watching {
                log::warn!("cannot watch for other launches: {}", error);
            }
            true
        }
        Err(error) => {
            log::warn!("cannot watch for other launches: {}", error);
            true
        }
    }
}

/// Always the running instance where launches are not tracked.
#[cfg(not(unix))]
pub fn claim(_name: &str, _waker: Waker) -> bool {
    true
}
