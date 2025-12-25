use indicatif::ProgressStyle;

pub fn bar_style() -> ProgressStyle {
    let style = ProgressStyle::with_template(
        "{spinner:.green} {msg} [{elapsed_precise}] [{bar:24.cyan/blue}] {pos}/{len}",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar());
    style.progress_chars("=> ")
}
