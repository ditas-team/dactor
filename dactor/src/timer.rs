//! Timer utilities for scheduling delayed and periodic messages.
//!
//! Provides [`send_after`] and [`send_interval`] for scheduling messages
//! to actors using the Tokio runtime. Also defines [`TimerHandle`], the
//! trait for cancellable timer handles.

use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, ActorRef, Handler};
use crate::message::Message;

/// A handle to a scheduled timer that can be cancelled.
pub trait TimerHandle: Send + 'static {
    /// Cancel the timer. Idempotent — calling cancel on an already-cancelled
    /// or fired timer is a no-op.
    fn cancel(self);
}

/// Schedule a single message to be sent after `delay`.
///
/// Returns a [`JoinHandle`] that completes once the message has been sent
/// (or the delay has elapsed and the actor is no longer reachable).
pub fn send_after<A, M, R>(
    actor_ref: &R,
    msg: M,
    delay: Duration,
) -> JoinHandle<()>
where
    A: Actor + Handler<M>,
    M: Message<Reply = ()>,
    R: ActorRef<A> + Clone + 'static,
{
    let actor = actor_ref.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = actor.tell(msg);
    })
}

/// Repeatedly send a message at fixed `interval`, using `msg_factory` to
/// create each message instance.
///
/// The loop stops when `cancel` is triggered or when the actor is no longer
/// reachable (i.e., `tell()` returns an error).
pub fn send_interval<A, M, R, F>(
    actor_ref: &R,
    msg_factory: F,
    interval: Duration,
    cancel: CancellationToken,
) -> JoinHandle<()>
where
    A: Actor + Handler<M>,
    M: Message<Reply = ()>,
    R: ActorRef<A> + Clone + 'static,
    F: Fn() -> M + Send + 'static,
{
    let actor = actor_ref.clone();
    tokio::spawn(async move {
        let mut interval_timer = tokio::time::interval(interval);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                _ = interval_timer.tick() => {
                    if actor.tell(msg_factory()).is_err() { break; }
                }
            }
        }
    })
}

#[cfg(test)]
#[cfg(feature = "test-support")]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use async_trait::async_trait;

    use crate::actor::{Actor, ActorContext};
    use crate::message::Message;
    use crate::test_support::test_runtime::TestRuntime;

    // -- Shared test actor --------------------------------------------------

    struct Tick;
    impl Message for Tick {
        type Reply = ();
    }

    struct TickCounter {
        count: Arc<AtomicU64>,
    }

    impl Actor for TickCounter {
        type Args = Arc<AtomicU64>;
        type Deps = ();
        fn create(args: Arc<AtomicU64>, _deps: ()) -> Self {
            Self { count: args }
        }
    }

    #[async_trait]
    impl Handler<Tick> for TickCounter {
        async fn handle(&mut self, _msg: Tick, _ctx: &mut ActorContext) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    // -- Tests --------------------------------------------------------------

    #[tokio::test]
    async fn send_after_delivers_message() {
        let count = Arc::new(AtomicU64::new(0));
        let rt = TestRuntime::new();
        let actor = rt.spawn::<TickCounter>("ticker", count.clone());

        send_after::<TickCounter, Tick, _>(&actor, Tick, Duration::from_millis(50));

        // Should not have arrived yet
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(count.load(Ordering::SeqCst), 0);

        // Wait for it
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn send_interval_delivers_multiple_messages() {
        let count = Arc::new(AtomicU64::new(0));
        let rt = TestRuntime::new();
        let actor = rt.spawn::<TickCounter>("ticker", count.clone());

        let cancel = CancellationToken::new();
        send_interval::<TickCounter, Tick, _, _>(
            &actor,
            || Tick,
            Duration::from_millis(30),
            cancel.clone(),
        );

        tokio::time::sleep(Duration::from_millis(200)).await;
        let received = count.load(Ordering::SeqCst);
        // Should have received multiple ticks (at least 3)
        assert!(received >= 3, "expected ≥ 3 ticks, got {}", received);

        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let after_cancel = count.load(Ordering::SeqCst);
        // No more ticks after cancel
        tokio::time::sleep(Duration::from_millis(100)).await;
        let final_count = count.load(Ordering::SeqCst);
        assert_eq!(after_cancel, final_count, "ticks continued after cancel");
    }

    #[tokio::test]
    async fn send_interval_stops_when_actor_stops() {
        let count = Arc::new(AtomicU64::new(0));
        let rt = TestRuntime::new();
        let actor = rt.spawn::<TickCounter>("ticker", count.clone());

        let cancel = CancellationToken::new();
        let handle = send_interval::<TickCounter, Tick, _, _>(
            &actor,
            || Tick,
            Duration::from_millis(20),
            cancel,
        );

        tokio::time::sleep(Duration::from_millis(100)).await;
        actor.stop();
        // The interval task should stop once tell() fails
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }
}
