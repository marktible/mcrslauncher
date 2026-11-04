//! Bounded retries for read-only requests, including cancellation during backoff.
use anyhow::{Result, ensure};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

pub fn is_temporary(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    // Horizon's captured output may include an earlier nonfatal download warning.
    let cause = message
        .rsplit_once("error:")
        .map_or(message.as_str(), |(_, cause)| cause);
    if ["certificate", "tls error", "tls handshake"]
        .iter()
        .any(|marker| cause.contains(marker))
    {
        return false;
    }
    for source in error.chain() {
        if let Some(http) = source.downcast_ref::<reqwest::Error>() {
            if let Some(status) = http.status() {
                return matches!(status.as_u16(), 502..=504);
            }
            if http.is_timeout() || http.is_connect() {
                return true;
            }
        }
        if let Some(io) = source.downcast_ref::<io::Error>()
            && matches!(
                io.kind(),
                io::ErrorKind::TimedOut
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::NotConnected
            )
        {
            return true;
        }
    }
    [
        "dns failed:",
        "temporary failure in name resolution",
        "failed to lookup address information",
        "connection reset",
        "connection aborted",
        "connection timed out",
        "operation timed out",
        "network is unreachable",
        "http 502",
        "http 503",
        "http 504",
        "status 502",
        "status 503",
        "status 504",
    ]
    .iter()
    .any(|marker| cause.contains(marker))
}

pub fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Operation cancelled");
    Ok(())
}

pub fn retry<T>(
    cancel: &AtomicBool,
    delays: &[Duration],
    notify: impl Fn(usize, Duration),
    mut request: impl FnMut() -> Result<T>,
) -> Result<T> {
    for attempt in 0..=delays.len() {
        check_cancel(cancel)?;
        let error = match request() {
            Ok(value) => {
                check_cancel(cancel)?;
                return Ok(value);
            }
            Err(error) => error,
        };
        check_cancel(cancel)?;
        if !is_temporary(&error) {
            return Err(error);
        }
        let Some(delay) = delays.get(attempt) else {
            return Err(error);
        };
        notify(attempt + 2, *delay);
        let started = Instant::now();
        while started.elapsed() < *delay {
            check_cancel(cancel)?;
            thread::sleep(
                delay
                    .saturating_sub(started.elapsed())
                    .min(Duration::from_millis(80)),
            );
        }
    }
    unreachable!("the final request returns its result")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_failures_retry_to_the_limit_without_retrying_permanent_errors() {
        let mut attempts = 0;
        let result = retry::<()>(
            &AtomicBool::new(false),
            &[Duration::ZERO; 2],
            |_, _| {},
            || {
                attempts += 1;
                anyhow::bail!("Temporary failure in name resolution")
            },
        );
        assert!(result.is_err());
        assert_eq!(attempts, 3);
        assert!(!is_temporary(&anyhow::anyhow!("invalid peer certificate")));
        assert!(!is_temporary(&anyhow::anyhow!(
            "Download checksum mismatch"
        )));
    }
}
