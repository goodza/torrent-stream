use anyhow::Result;
use clap::{CommandFactory, FromArgMatches};
use tokio_util::sync::CancellationToken;
use torrent_stream::{
    app,
    cli::{Cli, Management},
    settings::{self, Settings},
};

#[tokio::main]
async fn main() -> Result<()> {
    let matches = Cli::command().get_matches();
    let mut cli = Cli::from_arg_matches(&matches)?;
    let config_path = cli
        .config
        .clone()
        .map(Ok)
        .unwrap_or_else(settings::default_path)?;
    if let Some(Management::Config { action }) = &cli.command {
        return Settings::manage(&config_path, action);
    }
    Settings::read(&config_path)?.apply(&mut cli, &matches)?;
    if let Some(Management::Cache { action }) = &cli.command {
        return torrent_stream::downloads::manage(
            &torrent_stream::cli::expand_home(&cli.download_dir)?,
            action,
        );
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(if cli.verbose {
            "torrent_stream=debug"
        } else {
            "torrent_stream=warn"
        })
        .init();
    let shutdown = CancellationToken::new();
    let mut stopped = false;
    let result = {
        let task = app::run_with_shutdown(cli, shutdown.clone());
        tokio::pin!(task);
        tokio::select! {
            result = &mut task => result,
            signal = tokio::signal::ctrl_c() => {
                signal?;
                stopped = true;
                shutdown.cancel();
                tokio::time::timeout(std::time::Duration::from_secs(8), &mut task)
                    .await.unwrap_or_else(|_| Err(anyhow::anyhow!("shutdown timed out; saved downloads are retained")))
            }
        }
    };
    if stopped {
        println!("\nStopped. Downloaded data is retained.");
    }
    result
}
