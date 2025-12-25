use crate::progress;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressDrawTarget};
use std::io::{self, Write};
use std::time::Duration;

pub struct Console {
    stdout: Box<dyn Write>,
}

impl Console {
    pub fn stdio() -> Self {
        Self {
            stdout: Box::new(io::stdout()),
        }
    }

    pub fn out(&mut self, msg: &str) -> Result<()> {
        writeln!(self.stdout, "{msg}").map_err(Into::into)
    }

    pub fn out_raw(&mut self, msg: &str) -> Result<()> {
        write!(self.stdout, "{msg}").map_err(Into::into)
    }

    pub fn flush_out(&mut self) -> Result<()> {
        self.stdout.flush().map_err(Into::into)
    }

    pub fn progress_bar(&self, total: u64, label: &str) -> ProgressBar {
        let pb = ProgressBar::with_draw_target(
            Some(total.max(1)),
            ProgressDrawTarget::stderr_with_hz(10),
        );
        pb.set_style(progress::bar_style());
        pb.set_message(label.to_string());
        pb.enable_steady_tick(Duration::from_millis(100));
        pb
    }

    pub fn spinner(&self, label: &str) -> ProgressBar {
        let pb = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr_with_hz(10));
        pb.set_style(progress::bar_style());
        pb.set_message(label.to_string());
        pb.enable_steady_tick(Duration::from_millis(100));
        pb
    }
}
