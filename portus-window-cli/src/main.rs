use clap::Parser;
use portus_window_cli::{execute, render_response, Cli, Commands};
use portus_window_protocol::Response;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let export_out = match &cli.command {
        Commands::Export { out: Some(path) } => Some(path.clone()),
        _ => None,
    };
    let outcome = execute(cli).await;

    if let (Some(path), Response::Ok { data, .. }) = (export_out, &outcome.response) {
        let formatted = serde_json::to_string_pretty(&data).unwrap_or_default();
        if let Err(e) = std::fs::write(&path, formatted) {
            eprintln!("error writing export to {}: {}", path.display(), e);
            std::process::exit(1);
        }
    }

    println!("{}", render_response(&outcome.response));
    std::process::exit(outcome.exit_code);
}
