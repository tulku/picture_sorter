use clap::Parser;
#[derive(Parser)]
#[command(version, about = "Offline photo archive manager")]
struct Cli {
    #[arg(long, hide = true)]
    nas_worker: bool,
}
fn main() {
    let cli = Cli::parse();
    if cli.nas_worker
        && let Err(error) = photo_sorter::transfer::run_worker()
    {
        eprintln!("{error}");
        std::process::exit(5);
    }
}
