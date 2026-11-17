//! Keeps gridwatch to one running copy. The first copy listens on a local port and notes
//! it in a file; a second copy finds it there, asks the first to show its window, and ends.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

const REQUEST: &str = "gridwatch show";
const ANSWER: &str = "gridwatch here";

/// Ask a copy that is already running to come to the front. `true` if one answered.
pub fn wake_running(port_file: &Path) -> bool {
    let Some(port) = std::fs::read_to_string(port_file)
        .ok()
        .and_then(|p| p.trim().parse::<u16>().ok())
    else {
        return false;
    };
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(400)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
    if writeln!(stream, "{REQUEST}").is_err() {
        return false;
    }
    // The port in an old file may belong to some other program by now; only gridwatch answers like this.
    let mut answer = String::new();
    let _ = BufReader::new(stream).read_line(&mut answer);
    answer.trim() == ANSWER
}

/// Become the running copy: `on_wake` is called whenever another copy is started.
pub fn listen(port_file: &Path, on_wake: impl Fn() + Send + 'static) {
    let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) else {
        return;
    };
    let Ok(address) = listener.local_addr() else { return };
    if std::fs::write(port_file, address.port().to_string()).is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("instance".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
            let mut request = String::new();
            let mut reader = BufReader::new(stream);
            let _ = reader.read_line(&mut request);
            if request.trim() == REQUEST {
                let _ = writeln!(reader.get_mut(), "{ANSWER}");
                on_wake();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn second_copy_wakes_the_first() {
        let file = std::env::temp_dir().join(format!("gridwatch-instance-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        // Nobody is running yet.
        assert!(!wake_running(&file));

        let woken = Arc::new(AtomicUsize::new(0));
        let counter = woken.clone();
        listen(&file, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert!(wake_running(&file));
        assert!(wake_running(&file));
        assert_eq!(woken.load(Ordering::SeqCst), 2);

        // A file left behind that points at something else is not a running copy.
        let other = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        std::fs::write(&file, other.local_addr().unwrap().port().to_string()).unwrap();
        assert!(!wake_running(&file));
        let _ = std::fs::remove_file(&file);
    }
}
