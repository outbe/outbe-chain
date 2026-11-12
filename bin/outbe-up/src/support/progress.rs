use std::{
    io::{self, IsTerminal, Write},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

pub struct Progress {
    interactive: bool,
    active_stage: Option<String>,
    spinner: Option<ProgressBar>,
}

pub struct WatchDisplay {
    interactive: bool,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            interactive: io::stdout().is_terminal(),
            active_stage: None,
            spinner: None,
        }
    }

    pub fn stage(&mut self, message: &str) {
        self.finish_active_stage();
        self.active_stage = Some(message.to_owned());
        if self.interactive {
            self.spinner = Some(new_spinner(message));
        } else {
            println!("==> {message}");
        }
    }

    pub fn update(&mut self, message: &str) {
        if self.interactive {
            if self.active_stage.is_none() {
                self.active_stage = Some(message.to_owned());
            }
            match &self.spinner {
                Some(spinner) => spinner.set_message(message.to_owned()),
                None => self.spinner = Some(new_spinner(message)),
            }
        } else {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs());
            println!("[{timestamp}] {message}");
        }
    }

    pub fn done(&mut self, message: &str) {
        self.clear_spinner();
        self.active_stage = None;
        println!("OK  {message}");
    }

    fn finish_active_stage(&mut self) {
        let Some(stage) = self.active_stage.take() else {
            return;
        };
        self.clear_spinner();
        if self.interactive {
            println!("OK  {stage}");
        }
    }

    fn clear_spinner(&mut self) {
        if let Some(spinner) = self.spinner.take() {
            spinner.finish_and_clear();
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.clear_spinner();
    }
}

fn new_spinner(message: &str) -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    spinner.set_draw_target(ProgressDrawTarget::stdout());
    spinner.set_style(
        ProgressStyle::with_template("{spinner} {msg}")
            .expect("static progress template must be valid")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    spinner.set_message(message.to_owned());
    spinner.enable_steady_tick(Duration::from_millis(100));
    spinner
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
