use anyhow::Result;
use clap::Parser;
use torrent_stream::{app, cli::Cli};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(if cli.verbose {
            "torrent_stream=debug"
        } else {
            "torrent_stream=warn"
        })
        .init();
    let mut stopped = false;
    let result = tokio::select! {
        result = app::run(cli) => result,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            stopped = true;
            Ok(())
        }
    };
    // The cancelled app future has dropped its dashboard and restored the
    // terminal before printing the final message on the original screen.
    if stopped {
        println!("\nStopped. Downloaded data is retained.");
    }
    result
}
