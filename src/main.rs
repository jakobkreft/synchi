use clap::Parser;

fn main() {
    let cli = synchi::Cli::parse();
    if let Err(err) = synchi::run(cli) {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}
