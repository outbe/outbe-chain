use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

pub struct Radicle {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Radicle {
    pub fn start(socket: &Path, node_id: [u8; 32]) -> Self {
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut bytes = vec![0xed, 0x01];
        bytes.extend_from_slice(&node_id);
        let encoded = multibase::encode(multibase::Base::Base58Btc, bytes);
        let stop = Arc::new(AtomicBool::new(false));
        let running = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !running.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut line = String::new();
                        if BufReader::new(stream.try_clone().unwrap())
                            .read_line(&mut line)
                            .is_err()
                        {
                            continue;
                        }
                        let command: serde_json::Value = serde_json::from_str(&line).unwrap();
                        let response = match command["command"].as_str().unwrap() {
                            "nodeId" => serde_json::json!(encoded),
                            "config" => serde_json::json!({"externalAddresses":["127.0.0.1:8776"]}),
                            _ => serde_json::json!({"success":true}),
                        };
                        let _ = writeln!(stream, "{response}");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Radicle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
