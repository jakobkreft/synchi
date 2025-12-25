use indicatif::ProgressStyle;

pub fn bar_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{spinner:.green} {msg} [{elapsed_precise}] [{bar:24.cyan/blue}] {pos}/{len}",
    )
    .unwrap()
    .progress_chars("=> ")
}
