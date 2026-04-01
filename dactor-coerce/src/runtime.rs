//! Coerce adapter runtime.
//!
//! Currently wraps `TestRuntime` as a placeholder. The actual `coerce-rt`
//! integration will replace the internal implementation while keeping
//! the same public API.
//!
//! # Why a stub?
//!
//! The coerce-rs crate (`coerce-rt`) may have dependency or maintenance
//! issues. This stub lets us lock down the API surface and pass
//! conformance tests now, then swap in the real engine later.

use dactor::actor::*;
use dactor::errors::ActorSendError;
use dactor::interceptor::*;
use dactor::mailbox::MailboxConfig;
use dactor::message::*;
use dactor::node::*;
use dactor::stream::*;
use dactor::test_support::test_runtime::{TestActorRef, TestRuntime};

use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// SpawnOptions
// ---------------------------------------------------------------------------

/// Options for spawning an actor with the coerce adapter.
pub struct SpawnOptions {
    pub interceptors: Vec<Box<dyn InboundInterceptor>>,
    pub mailbox: MailboxConfig,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            interceptors: Vec::new(),
            mailbox: MailboxConfig::Unbounded,
        }
    }
}

// ---------------------------------------------------------------------------
// CoerceRuntime
// ---------------------------------------------------------------------------

/// Coerce adapter runtime.
///
/// Currently backed by [`TestRuntime`] as a placeholder.
/// The `coerce-rt` dependency will be integrated in a future PR,
/// replacing the internals while preserving this public API.
pub struct CoerceRuntime {
    inner: TestRuntime,
}

impl CoerceRuntime {
    /// Create a new `CoerceRuntime`.
    pub fn new() -> Self {
        Self {
            inner: TestRuntime::new(),
        }
    }

    /// Spawn an actor whose `Deps` type is `()`.
    pub fn spawn<A>(&self, name: &str, args: A::Args) -> CoerceActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        CoerceActorRef {
            inner: self.inner.spawn(name, args),
        }
    }

    /// Spawn an actor with explicit dependencies.
    pub fn spawn_with_deps<A>(
        &self,
        name: &str,
        args: A::Args,
        deps: A::Deps,
    ) -> CoerceActorRef<A>
    where
        A: Actor + 'static,
    {
        CoerceActorRef {
            inner: self.inner.spawn_with_deps(name, args, deps),
        }
    }

    /// Spawn an actor with spawn options (interceptors, mailbox config).
    pub fn spawn_with_options<A>(
        &self,
        name: &str,
        args: A::Args,
        options: SpawnOptions,
    ) -> CoerceActorRef<A>
    where
        A: Actor<Deps = ()> + 'static,
    {
        // Convert our SpawnOptions into TestRuntime's SpawnOptions.
        let inner_opts = dactor::test_support::test_runtime::SpawnOptions {
            interceptors: options.interceptors,
            mailbox: options.mailbox,
        };
        CoerceActorRef {
            inner: self.inner.spawn_with_options(name, args, inner_opts),
        }
    }

    /// Add a global outbound interceptor.
    ///
    /// Must be called before any actors are spawned.
    pub fn add_outbound_interceptor(&mut self, interceptor: Box<dyn OutboundInterceptor>) {
        self.inner.add_outbound_interceptor(interceptor);
    }
}

impl Default for CoerceRuntime {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// CoerceActorRef
// ---------------------------------------------------------------------------

/// A reference to an actor managed by the coerce adapter.
///
/// Currently wraps [`TestActorRef`] as a placeholder.
pub struct CoerceActorRef<A: Actor> {
    inner: TestActorRef<A>,
}

impl<A: Actor> Clone for CoerceActorRef<A> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<A: Actor + 'static> ActorRef<A> for CoerceActorRef<A> {
    fn id(&self) -> ActorId {
        self.inner.id()
    }

    fn name(&self) -> String {
        self.inner.name()
    }

    fn is_alive(&self) -> bool {
        self.inner.is_alive()
    }

    fn stop(&self) {
        self.inner.stop()
    }

    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError>
    where
        A: Handler<M>,
        M: Message<Reply = ()>,
    {
        self.inner.tell(msg)
    }

    fn ask<M>(
        &self,
        msg: M,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: Handler<M>,
        M: Message,
    {
        self.inner.ask(msg, cancel)
    }

    fn stream<M>(
        &self,
        msg: M,
        buffer: usize,
        cancel: Option<CancellationToken>,
    ) -> Result<BoxStream<M::Reply>, ActorSendError>
    where
        A: StreamHandler<M>,
        M: Message,
    {
        self.inner.stream(msg, buffer, cancel)
    }

    fn feed<M>(
        &self,
        msg: M,
        input: BoxStream<M::Item>,
        buffer: usize,
        cancel: Option<CancellationToken>,
    ) -> Result<AskReply<M::Reply>, ActorSendError>
    where
        A: FeedHandler<M>,
        M: FeedMessage,
    {
        self.inner.feed(msg, input, buffer, cancel)
    }
}
