use std::{
    io::{self, IsTerminal, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

pub struct Progress {
    interactive: bool,
    line_visible: bool,
    spinner: Option<Spinner>,
}

pub struct WatchDisplay {
    interactive: bool,
}

struct Spinner {
    stop: Arc<AtomicBool>,
    message: Arc<Mutex<String>>,
    handle: Option<JoinHandle<()>>,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            interactive: io::stdout().is_terminal(),
            line_visible: false,
            spinner: None,
        }
    }

    pub fn stage(&mut self, message: &str) {
        self.stop_spinner();
        self.clear_line();
        if self.interactive {
            self.spinner = Some(Spinner::start(message));
            self.line_visible = true;
        } else {
            println!("==> {message}");
        }
    }

    pub fn update(&mut self, message: &str) {
        if self.interactive {
            if let Some(spinner) = self.spinner.as_ref() {
                spinner.set_message(message);
            } else {
                self.spinner = Some(Spinner::start(message));
                self.line_visible = true;
            }
        } else {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs());
            println!("[{timestamp}] {message}");
        }
    }

    pub fn done(&mut self, message: &str) {
        self.stop_spinner();
        self.clear_line();
        println!("OK  {message}");
    }

    fn stop_spinner(&mut self) {
        if let Some(mut spinner) = self.spinner.take() {
            spinner.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = spinner.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn clear_line(&mut self) {
        if self.interactive && self.line_visible {
            print!("\r\x1b[2K");
            let _ = io::stdout().flush();
            self.line_visible = false;
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.stop_spinner();
        self.clear_line();
    }
}

impl WatchDisplay {
    pub fn new() -> Self {
        let interactive = io::stdout().is_terminal();
        if interactive {
            print!("\x1b[?1049h\x1b[?25l");
            let _ = io::stdout().flush();
        }
        Self { interactive }
    }

    pub fn render(&self, status: &str) {
        if self.interactive {
            print!(
                "\x1b[H\x1b[2JOutbe node status\n\n{status}\n\nRefresh: 5s  |  Ctrl-C to stop\n"
            );
            let _ = io::stdout().flush();
        } else {
            println!("{status}");
        }
    }
}

impl Drop for WatchDisplay {
    fn drop(&mut self) {
        if self.interactive {
            print!("\x1b[?25h\x1b[?1049l");
            let _ = io::stdout().flush();
        }
    }
}

impl Spinner {
    fn start(initial: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let message = Arc::new(Mutex::new(initial.to_owned()));
        let worker_stop = Arc::clone(&stop);
        let worker_message = Arc::clone(&message);
        let handle = thread::spawn(move || {
            const FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];
            let mut frame = 0;
            while !worker_stop.load(Ordering::Relaxed) {
                let message = worker_message
                    .lock()
                    .map(|value| value.clone())
                    .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
                print!("\r\x1b[2K{} {}", FRAMES[frame], message);
                let _ = io::stdout().flush();
                frame = (frame + 1) % FRAMES.len();
                thread::sleep(Duration::from_millis(120));
            }
        });
        Self {
            stop,
            message,
            handle: Some(handle),
        }
    }

    fn set_message(&self, value: &str) {
        match self.message.lock() {
            Ok(mut message) => value.clone_into(&mut message),
            Err(poisoned) => value.clone_into(&mut poisoned.into_inner()),
        }
    }
}
