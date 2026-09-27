//! Bounds for work a tool handler cannot interrupt or fully trust: blocking `SQLite` calls,
//! app-supplied probes, a handler that panics, and caller-supplied look-back windows.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::sync::Semaphore;

/// Blocking database calls (`query_db`, `introspect db_health`) allowed to run at once.
///
/// A single `SQLite` string op (`LIKE`, `replace`, `instr`, `GLOB`) never checks for an
/// interrupt, so it can overrun its deadline by seconds; a timed-out call's thread keeps
/// running until the op returns. The cap stops repeated calls from stacking such threads.
#[cfg(feature = "sqlite")]
pub const DB_MAX_CONCURRENT: usize = 2;
#[cfg(feature = "sqlite")]
pub static DB_SLOTS: Semaphore = Semaphore::const_new(DB_MAX_CONCURRENT);

/// Grace on top of an op's own deadline before the caller stops waiting for it.
#[cfg(feature = "sqlite")]
pub const BLOCKING_DEADLINE_SLACK: Duration = Duration::from_secs(5);

/// Run blocking `f` on the blocking pool and return within `deadline`, whatever `f` does.
///
/// With `slots`, a permit is taken first (the wait counts against `deadline`) and is held BY
/// THE BLOCKING CLOSURE, so a call the caller gave up on keeps its slot until its thread
/// actually finishes — concurrency stays capped even across timeouts. A panic in `f` becomes
/// an error, never a hung or reset request.
pub async fn run_blocking_bounded<T: Send + 'static>(
    slots: Option<&'static Semaphore>,
    what: &str,
    deadline: Duration,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let deadline_at = tokio::time::Instant::now() + deadline;
    let permit = match slots {
        Some(slots) => match tokio::time::timeout_at(deadline_at, slots.acquire()).await {
            Ok(Ok(permit)) => Some(permit),
            Ok(Err(_closed)) => return Err(format!("{what} is unavailable")),
            Err(_) => {
                return Err(format!(
                    "{what} is busy: earlier calls are still running (a single SQLite string \
                     operation such as LIKE, GLOB, replace() or instr() cannot be interrupted \
                     and may overrun its deadline). Retry shortly."
                ));
            }
        },
        None => None,
    };
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    });
    match tokio::time::timeout_at(deadline_at, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) if e.is_panic() => Err(format!(
            "{what} panicked: {}",
            panic_message(&*e.into_panic())
        )),
        Ok(Err(e)) => Err(format!("{what} failed: {e}")),
        Err(_) => Err(format!(
            "{what} did not finish within {} ms (it is left to finish in the background)",
            deadline.as_millis()
        )),
    }
}

/// The message carried by a panic payload, if it is a string.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

/// A future that turns a panic while polling `F` into `Err(message)`.
///
/// Tool dispatch has no other panic boundary: a panicking handler used to unwind into
/// hyper's connection task, so an MCP call hung until the client timed out and a REST call
/// saw a reset connection.
pub struct CatchUnwind<F>(Pin<Box<F>>);

impl<F: Future> CatchUnwind<F> {
    pub fn new(future: F) -> Self {
        Self(Box::pin(future))
    }
}

impl<F: Future> Future for CatchUnwind<F> {
    type Output = Result<F::Output, String>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = self.0.as_mut();
        match std::panic::catch_unwind(AssertUnwindSafe(|| inner.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Err(payload) => Poll::Ready(Err(panic_message(&*payload))),
        }
    }
}

/// `now - ms` milliseconds, saturating at the earliest representable instant ("all history").
///
/// `Utc::now() - TimeDelta` PANICS when the result leaves chrono's range, and a caller's
/// `u64` cast to `i64` wrapped negative into a FUTURE baseline.
pub fn ms_ago(now: chrono::DateTime<chrono::Utc>, ms: u64) -> chrono::DateTime<chrono::Utc> {
    let delta = i64::try_from(ms)
        .ok()
        .and_then(chrono::TimeDelta::try_milliseconds)
        .unwrap_or(chrono::TimeDelta::MAX);
    now.checked_sub_signed(delta)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
}

/// `now - secs` seconds, saturating like [`ms_ago`].
pub fn secs_ago(now: chrono::DateTime<chrono::Utc>, secs: u64) -> chrono::DateTime<chrono::Utc> {
    ms_ago(now, secs.saturating_mul(1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_back_saturates_instead_of_panicking_or_wrapping() {
        let now = chrono::Utc::now();
        for ms in [u64::MAX, i64::MAX as u64 + 1, 9_000_000_000_000_000] {
            assert_eq!(
                ms_ago(now, ms),
                chrono::DateTime::<chrono::Utc>::MIN_UTC,
                "{ms}"
            );
        }
        for secs in [u64::MAX, 10_000_000_000_000] {
            assert_eq!(
                secs_ago(now, secs),
                chrono::DateTime::<chrono::Utc>::MIN_UTC
            );
        }
        assert_eq!(
            ms_ago(now, 1500),
            now - chrono::TimeDelta::milliseconds(1500)
        );
        assert_eq!(secs_ago(now, 30), now - chrono::TimeDelta::seconds(30));
        assert!(ms_ago(now, 0) == now);
    }

    #[tokio::test]
    async fn catch_unwind_reports_a_panic_as_an_error() {
        let r: Result<(), String> = CatchUnwind::new(async {
            tokio::task::yield_now().await;
            panic!("boom in handler");
        })
        .await;
        let err = r.unwrap_err();
        assert!(err.contains("boom in handler"), "{err}");
        assert_eq!(CatchUnwind::new(async { 7 }).await, Ok(7));
    }

    /// A blocking op past its deadline returns within the deadline (not when the op ends),
    /// its slot stays taken until the op really finishes, and a panic is an error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_calls_are_deadlined_capped_and_panic_safe() {
        static ONE_SLOT: Semaphore = Semaphore::const_new(1);
        let started = std::time::Instant::now();
        let err = run_blocking_bounded(
            Some(&ONE_SLOT),
            "slow op",
            Duration::from_millis(200),
            || {
                std::thread::sleep(Duration::from_millis(1500));
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("did not finish within 200 ms"), "{err}");
        assert!(
            started.elapsed() < Duration::from_millis(1000),
            "{:?}",
            started.elapsed()
        );

        // The abandoned op still holds the only slot: a second call is refused as busy.
        let err = run_blocking_bounded(Some(&ONE_SLOT), "db", Duration::from_millis(200), || Ok(1))
            .await
            .unwrap_err();
        assert!(err.contains("busy"), "{err}");
        assert_eq!(ONE_SLOT.available_permits(), 0);

        // Once the op's thread finishes, the slot comes back.
        let ok = run_blocking_bounded(Some(&ONE_SLOT), "db", Duration::from_secs(5), || Ok(2))
            .await
            .unwrap();
        assert_eq!(ok, 2);

        let err = run_blocking_bounded::<()>(None, "probe", Duration::from_secs(5), || {
            panic!("probe exploded")
        })
        .await
        .unwrap_err();
        assert!(err.contains("probe panicked: probe exploded"), "{err}");
    }
}
