use clap::Parser;
fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--nas-worker")
    {
        if let Err(error) = photo_sorter::transfer::run_worker() {
            eprintln!("{error}");
            std::process::exit(5);
        }
        return;
    }
    let mut cli = photo_sorter::cli::Cli::parse();
    let json = cli.json;
    let id = cli.request_id.unwrap_or_default();
    cli.request_id = Some(id);
    match photo_sorter::cli::run(cli) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            let code = photo_sorter::service::protocol::error_exit(&error);
            if json {
                if let Ok(text) = serde_json::to_string(
                    &photo_sorter::service::protocol::Response::error(id, error),
                ) {
                    println!("{text}");
                }
            } else {
                eprintln!("{error}");
            }
            std::process::exit(code);
        }
    }
}
