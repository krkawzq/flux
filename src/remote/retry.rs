//! Retry policy for transient `RemoteOps` failures.

use crate::remote::RemoteOpsError;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u8,
    pub base_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_backoff: Duration::from_millis(200),
        }
    }
}

impl RetryPolicy {
    pub fn no_retry() -> Self {
        Self {
            max_attempts: 1,
            base_backoff: Duration::ZERO,
        }
    }
}

pub fn is_retryable(err: &RemoteOpsError) -> bool {
    matches!(err, RemoteOpsError::Transport(_) | RemoteOpsError::Io(_))
}

pub async fn with_retry<T, F, Fut>(policy: RetryPolicy, mut op: F) -> Result<T, RemoteOpsError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, RemoteOpsError>>,
{
    let mut wait = policy.base_backoff;
    let mut last_err = None;

    for attempt in 0..policy.max_attempts {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if !is_retryable(&err) || attempt + 1 == policy.max_attempts {
                    return Err(err);
                }
                last_err = Some(err);
                tokio::time::sleep(wait).await;
                wait = wait.saturating_mul(2);
            }
        }
    }

    Err(last_err.expect("retry loop must store an error before exhaustion"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn retries_on_transport_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let policy = RetryPolicy {
            max_attempts: 3,
            base_backoff: Duration::from_millis(1),
        };

        let result = with_retry(policy, || {
            let calls = Arc::clone(&calls2);
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(RemoteOpsError::Transport("flake".into()))
                } else {
                    Ok(())
                }
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_not_found() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let policy = RetryPolicy::default();

        let _ = with_retry::<(), _, _>(policy, || {
            let calls = Arc::clone(&calls2);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(RemoteOpsError::NotFound("/x".into()))
            }
        })
        .await;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
