use std::{
    io::{IsTerminal, stdout},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, bail};
use clap::Parser;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::watch,
    time::timeout,
};
use wordropbot::{bot::BotApp, config::Config, storage::Storage};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_CONFIG: &str = "config.toml";

#[derive(Parser)]
struct Args {
    /// Path to the TOML config file.
    #[arg(long, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(stdout)
        // journald would show escape codes literally.
        .with_ansi(stdout().is_terminal())
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
    let args = Args::parse();
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
    let mut scheduler = tokio::spawn(app.scheduler().run(stopped));
    let mut dispatcher = tokio::spawn(app.run());
    tracing::info!("bot started");

    let mut dispatcher_result = None;
    let mut scheduler_result = None;
    // The dispatcher fails at start without Telegram; keep running only if it
    // runs.
    tokio::select! {
        _ = sigterm.recv() => tracing::info!("SIGTERM received, shutting down"),
        _ = sigint.recv() => tracing::info!("SIGINT received, shutting down"),
        result = &mut dispatcher => dispatcher_result = Some(result),
        result = &mut scheduler => scheduler_result = Some(result),
    }
    let signalled = dispatcher_result.is_none() && scheduler_result.is_none();
    let scheduler_early = scheduler_result.is_some();
    // Receivers may already be gone if the scheduler stopped on its own.
    let _ = stop.send(true);
    // Telegram can swallow a shutdown request, so the whole stop is bounded.
    let stopping = async {
        if dispatcher_result.is_none() {
            dispatcher_result = Some(match token.shutdown() {
                // A dispatcher that dies while shutting down never completes
                // `done`.
                Ok(done) => tokio::select! {
                    _ = done => (&mut dispatcher).await,
                    result = &mut dispatcher => result,
                },
                // Not polling updates yet (or any more): nothing to wait for.
                Err(_) => {
                    dispatcher.abort();
                    (&mut dispatcher).await
                }
            });
        }
        if scheduler_result.is_none() {
            scheduler_result = Some((&mut scheduler).await);
        }
    };
    if timeout(SHUTDOWN_TIMEOUT, stopping).await.is_err() {
        tracing::warn!(timeout = ?SHUTDOWN_TIMEOUT, "shutdown timed out, aborting tasks");
        dispatcher.abort();
        scheduler.abort();
    }
    storage.close().await;
    if let Some(Err(error)) = &scheduler_result
        && !scheduler_early
    {
        tracing::error!(%error, "scheduler task failed");
    }
    match (scheduler_result, dispatcher_result) {
        (Some(Ok(())), _) if scheduler_early => bail!("scheduler stopped unexpectedly"),
        (Some(Err(error)), _) if scheduler_early => bail!("scheduler task failed: {error}"),
        (_, Some(Ok(Ok(())))) if signalled => Ok(()),
        (_, Some(Ok(Ok(())))) => bail!("telegram dispatcher stopped unexpectedly"),
        (_, Some(Ok(Err(error)))) => bail!("telegram dispatcher failed: {error}"),
        // Aborting is how an idle dispatcher is stopped on a signal.
        (_, Some(Err(error))) if error.is_cancelled() && signalled => Ok(()),
        (_, Some(Err(error))) => bail!("telegram dispatcher task failed: {error}"),
        // Only a signal gets here; the stop timed out before the dispatcher
        // finished.
        (_, None) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::iter;

    use super::*;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(iter::once("wordropbot").chain(args.iter().copied()))
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
