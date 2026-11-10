use std::{
    io::{self, IsTerminal, Write},
    time::{SystemTime, UNIX_EPOCH},
};

pub struct Progress {
    interactive: bool,
    line_visible: bool,
    active_stage: Option<String>,
    rendered_message: Option<String>,
}

pub struct WatchDisplay {
    interactive: bool,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            interactive: io::stdout().is_terminal(),
            line_visible: false,
            active_stage: None,
            rendered_message: None,
        }
    }

    pub fn stage(&mut self, message: &str) {
        self.finish_active_stage();
        self.active_stage = Some(message.to_owned());
        if self.interactive {
            self.render(message);
        } else {
            println!("==> {message}");
        }
    }

    pub fn update(&mut self, message: &str) {
        if self.interactive {
            if self.active_stage.is_none() {
                self.active_stage = Some(message.to_owned());
            }
            self.render(message);
        } else {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs());
            println!("[{timestamp}] {message}");
        }
    }

    pub fn done(&mut self, message: &str) {
        self.clear_line();
        self.active_stage = None;
        println!("OK  {message}");
    }

    fn finish_active_stage(&mut self) {
        let Some(stage) = self.active_stage.take() else {
            return;
        };
        self.clear_line();
        if self.interactive {
            println!("OK  {stage}");
        }
    }

    fn render(&mut self, message: &str) {
        if self.rendered_message.as_deref() == Some(message) {
            return;
        }
        print!("\r\x1b[2K● {message}");
        let _ = io::stdout().flush();
        self.line_visible = true;
        self.rendered_message = Some(message.to_owned());
    }

    fn clear_line(&mut self) {
        if self.interactive && self.line_visible {
            print!("\r\x1b[2K");
            let _ = io::stdout().flush();
            self.line_visible = false;
        }
        self.rendered_message = None;
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
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
