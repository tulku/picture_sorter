use clap::Parser;
#[derive(Parser)]
#[command(version, about = "Offline photo archive manager")]
struct Cli {}
fn main() {
    Cli::parse();
}
