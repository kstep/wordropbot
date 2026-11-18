use std::{
    io::{IsTerminal, stdout},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, bail};
use clap::Parser;
use teloxide::RequestError;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::watch,
    task::JoinError,
    time::timeout,
};
use wordropbot::{
    bot::BotApp,
    config::Config,
    storage::{Storage, StorageLimits},
};

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
    let limits = StorageLimits::from(&config.limits);
    let storage = Storage::connect(&config.database.path, limits)
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
    TaskResults {
        signalled,
        scheduler_early,
        scheduler: scheduler_result,
        dispatcher: dispatcher_result,
    }
    .into_exit()
}

/// How the two tasks ended once the shutdown is over.
struct TaskResults {
    signalled: bool,
    /// The scheduler ended before any shutdown was requested.
    scheduler_early: bool,
    scheduler: Option<Result<(), JoinError>>,
    dispatcher: Option<Result<Result<(), RequestError>, JoinError>>,
}

impl TaskResults {
    fn into_exit(self) -> anyhow::Result<()> {
        let TaskResults {
            signalled,
            scheduler_early,
            scheduler,
            dispatcher,
        } = self;
        if let Some(Err(error)) = &scheduler
            && !scheduler_early
        {
            tracing::error!(%error, "scheduler task failed");
        }
        match (scheduler, dispatcher) {
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
}

#[cfg(test)]
mod tests {
    use std::future;

    use teloxide::ApiError;

    use super::*;

    async fn cancelled<T: Send + 'static>() -> Result<T, JoinError> {
        let handle = tokio::spawn(future::pending::<T>());
        handle.abort();
        handle.await
    }

    fn results(
        signalled: bool,
        dispatcher: Option<Result<Result<(), RequestError>, JoinError>>,
    ) -> TaskResults {
        TaskResults {
            signalled,
            scheduler_early: false,
            scheduler: Some(Ok(())),
            dispatcher,
        }
    }

    #[tokio::test]
    async fn signalled_shutdown_exits_cleanly() {
        let endings = [
            Some(Ok(Ok(()))),
            Some(cancelled().await),
            // The stop timed out before the dispatcher finished.
            None,
        ];
        for dispatcher in endings {
            assert!(results(true, dispatcher).into_exit().is_ok());
        }
    }

    #[tokio::test]
    async fn dispatcher_ending_on_its_own_is_error() {
        let api_error = RequestError::Api(ApiError::BotBlocked);
        let endings = [
            (Ok(Ok(())), "stopped unexpectedly"),
            (Ok(Err(api_error)), "dispatcher failed"),
            (cancelled().await, "dispatcher task failed"),
        ];
        for (dispatcher, expected) in endings {
            let error = results(false, Some(dispatcher)).into_exit().unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[tokio::test]
    async fn early_scheduler_end_is_error_whatever_dispatcher_did() {
        for (scheduler, expected) in [
            (Ok(()), "scheduler stopped unexpectedly"),
            (cancelled().await, "scheduler task failed"),
        ] {
            let exit = TaskResults {
                signalled: false,
                scheduler_early: true,
                scheduler: Some(scheduler),
                dispatcher: Some(Ok(Ok(()))),
            }
            .into_exit();
            let error = exit.unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }
}
