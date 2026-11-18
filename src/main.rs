use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use wordropbot::bot::BotApp;
use wordropbot::config::Config;
use wordropbot::storage::Storage;

const DEFAULT_CONFIG: &str = "config.toml";

struct Args {
    config: PathBuf,
}

impl Args {
    /// Accepts `--config <path>`; everything else is an error.
    fn parse(mut args: impl Iterator<Item = String>) -> anyhow::Result<Args> {
        let mut config = PathBuf::from(DEFAULT_CONFIG);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config" => {
                    config = args.next().context("--config needs a path")?.into();
                }
                other => bail!("unknown argument {other}; usage: wordropbot [--config <path>]"),
            }
        }
        Ok(Args { config })
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let args = Args::parse(std::env::args().skip(1))?;
    let config = Arc::new(Config::load(&args.config)?);
    let storage = Storage::connect(&config.database.path)
        .await
        .with_context(|| format!("cannot open database {}", config.database.path.display()))?;
    let dropped = storage.delete_unsent_quizzes().await?;
    tracing::info!(dropped, "removed quizzes left unsent by the previous run");

    let mut sigterm = signal(SignalKind::terminate()).context("cannot handle SIGTERM")?;
    let mut sigint = signal(SignalKind::interrupt()).context("cannot handle SIGINT")?;

    let app = BotApp::new(storage.clone(), config);
    let token = app.shutdown_token();
    let (stop, stopped) = watch::channel(false);
    let scheduler = tokio::spawn(app.scheduler().run(stopped));
    let dispatcher = tokio::spawn(app.run());
    tracing::info!("bot started");

    tokio::select! {
        _ = sigterm.recv() => tracing::info!("SIGTERM received, shutting down"),
        _ = sigint.recv() => tracing::info!("SIGINT received, shutting down"),
    }
    // Receivers may already be gone if the scheduler stopped on its own.
    let _ = stop.send(true);
    match token.shutdown() {
        Ok(done) => done.await,
        // The dispatcher is not polling updates yet (or any more), so there is nothing to wait for.
        Err(_) => dispatcher.abort(),
    }
    if let Err(error) = scheduler.await {
        tracing::error!(%error, "scheduler task failed");
    }
    // An aborted task reports as cancelled; that is expected here.
    let _ = dispatcher.await;
    storage.close().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Args> {
        Args::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn config_defaults_and_overrides() {
        assert_eq!(parse(&[]).unwrap().config, PathBuf::from("config.toml"));
        assert_eq!(
            parse(&["--config", "x.toml"]).unwrap().config,
            PathBuf::from("x.toml")
        );
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse(&["--config"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
