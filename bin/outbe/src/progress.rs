use std::{
    io::{self, IsTerminal, Write},
    time::{SystemTime, UNIX_EPOCH},
};

pub struct Progress {
    interactive: bool,
    line_visible: bool,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            interactive: io::stdout().is_terminal(),
            line_visible: false,
        }
    }

    pub fn stage(&mut self, message: &str) {
        self.clear_line();
        println!("==> {message}");
    }

    pub fn update(&mut self, message: &str) {
        if self.interactive {
            print!("\r\x1b[2K{message}");
            let _ = io::stdout().flush();
            self.line_visible = true;
        } else {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs());
            println!("[{timestamp}] {message}");
        }
    }

    pub fn done(&mut self, message: &str) {
        self.clear_line();
        println!("OK  {message}");
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
        self.clear_line();
    }
}
