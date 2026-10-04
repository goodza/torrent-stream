use anyhow::Result;
use clap::{CommandFactory, Parser};
use torrent_stream::{app, cli::Cli};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(shell) = cli.generate_completion {
        clap_complete::generate(
            shell,
            &mut Cli::command(),
            "torrent-stream",
            &mut std::io::stdout(),
        );
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(if cli.verbose {
            "torrent_stream=debug"
        } else {
            "torrent_stream=warn"
        })
        .init();
    tokio::select! {
        result = app::run(cli) => result,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            println!("\nStopped. Downloaded data is retained.");
            Ok(())
        }
    }
}
