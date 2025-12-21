use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub fn bar_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{spinner:.green} {msg} [{elapsed_precise}] [{bar:24.cyan/blue}] {pos}/{len}",
    )
    .unwrap()
    .progress_chars("=> ")
}

pub fn new_bar(total: u64, label: &str) -> ProgressBar {
    let pb = ProgressBar::new(total.max(1));
    pb.set_style(bar_style());
    pb.set_message(label.to_string());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}
