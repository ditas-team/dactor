# dactor v0.2 — Implementation Progress

> Tracks PR status for the v0.2 implementation. See [dev-plan.md](dev-plan.md) for full plan details.

---

## Progress Summary

| Milestone | PRs | Status |
|-----------|-----|--------|
| v0.2.0-alpha.1 — Core API + Test Harness | PR 1–3 | ✅ Complete |
| v0.2.0-alpha.2 — Communication (tell/ask) | PR 4–6 | ✅ Complete |
| v0.2.0-alpha.3 — Messaging & Mailbox | PR 7–11 | ✅ Complete |
| v0.2.0-beta.1 — Streaming & Cancellation | PR 12–15 | ✅ Complete |
| v0.2.0-beta.2 — Error Model & Persistence | PR 16–18 | ✅ Complete |
| v0.2.0-rc.1 — Observability & Remote | PR 19–21 | ✅ Complete |
| Adapter — dactor-ractor | PR R1–R3 | ✅ Complete |
| Adapter — dactor-kameo | PR K1 | ✅ Complete |
| Adapter — dactor-mock | PR M1–M2 | ✅ Complete |
| Adapter — dactor-coerce | PR C1 | ✅ Complete (stub) |
| Cleanup — dispatch extraction, V2 prefix removal | cleanup | ✅ Complete |

---

## PR Tracker

| PR | Title | Branch | Status | Tests | Notes |
|----|-------|--------|--------|-------|-------|
| 1 | Module reorganization & cleanup | impl/pr-01-module-reorg | ✅ PR #3 | 46/46 pass | Green-to-green refactor |
| 2 | Actor trait & ActorId | impl/pr-02-actor-trait | ✅ PR #4 | 56/56 pass | 10 new tests |
| 3 | **Integration test harness (gRPC)** | impl/pr-03-test-harness | ✅ PR #5 | 61/61 pass | gRPC control protocol, fault injection, events |
| 4 | Message, Handler, ActorRef\<A\> | impl/pr-04-message-handler | ✅ PR #6 | 62/62 pass | 6 new tests |
| 5 | Tell (fire-and-forget) | impl/pr-05-tell | ✅ PR #7 | 69/69 pass | 7 new tests, v0.2 API functional |
| 6 | Ask (request-reply) | impl/pr-06-ask | ✅ PR #8 | 74/74 pass | 5 new tests |
| 7 | Envelope, Headers, RuntimeHeaders | impl/pr-07-envelope-headers | ✅ PR #9 | 89/89 pass | 16 new tests |
| 8 | Interceptor pipeline (Inbound) | impl/pr-08-inbound-interceptor | ✅ PR #10 | 104/104 pass | 14 new tests |
| 9 | Interceptor pipeline (Outbound) | impl/pr-09-outbound-interceptor | ✅ PR #11 | 113/113 pass | 7 new tests |
| 10 | Lifecycle hooks & ErrorAction | impl/pr-10-lifecycle | ✅ PR #12 | 121/121 pass | 8 new tests |
| 11 | MailboxConfig & OverflowStrategy | impl/pr-11-mailbox | ✅ PR #13 | 126/126 pass | 5 new tests |
| 12 | Supervision & DeathWatch | impl/pr-12-supervision | ✅ PR #14 | 130/130 pass | 4 new tests |
| 13 | Stream (server-streaming) | impl/pr-13-stream | ✅ PR #15 | 135/135 pass | 5 new tests |
| 14 | Feed (client-streaming) | impl/pr-14-feed | ✅ PR #16 | 139/139 pass | 4 new tests |
| 15 | Cancellation (CancellationToken) | impl/pr-15-cancellation | ✅ PR #17 | 146/146 pass | 7 new tests |
| 16 | Error model (ActorError, ErrorCodec) | impl/pr-16-error-model | ✅ PR #18 | 155/155 pass | 10 new tests |
| 17 | Persistence (EventSourced + DurableState) | impl/pr-17-persistence | ✅ PR #19 | 172/172 pass | 17 new tests |
| 18 | Dead letters, Delay, Throttle | impl/pr-18-dead-letters | ✅ PR #20 | 180/180 pass | 8 new tests |
| 19 | Observability (MetricsInterceptor) | impl/pr-19-observability | ✅ PR #21 | 189/189 pass | 9 new tests |
| 20 | Conformance suite & MockCluster | impl/pr-20-conformance | ✅ PR #22 | 195/195 pass | 6 new tests |
| 21 | Remote actors & cluster stubs | impl/pr-21-remote | ✅ PR #23 | 201/201 pass | 6 new tests |

### Adapter & Mock PRs

| PR | Title | Branch | Status | Notes |
|----|-------|--------|--------|-------|
| R1 | Ractor adapter v0.2 — spawn, tell, ask, stream, feed | impl/ractor-v2 | ✅ PR #25 | Full v0.2 adapter |
| R2 | Ractor — inbound + outbound interceptor pipelines | impl/ractor-interceptors | ✅ PR #26 | Interceptor wiring |
| R3 | Ractor — comprehensive adapter tests | impl/ractor-tests | ✅ PR #27 | Adapter test coverage |
| K1 | Kameo adapter v0.2 — full implementation | impl/kameo-v2 | ✅ PR #28 | Full v0.2 adapter |
| M1 | dactor-mock — MockCluster, MockNode, MockNetwork | impl/mock-cluster | ✅ PR #29 | Multi-node simulation |
| C1 | dactor-coerce adapter crate (stub) | impl/coerce-stub | ✅ PR #30 | Stub, no runtime wired |
| — | Remove V2 prefix and old v0.1 API | impl/cleanup-v2-prefix | ✅ PR #31 | Naming cleanup |
| — | Refactor: Extract shared dispatch module | impl/dispatch-extract | ✅ PR #32 | Shared dispatch module |
| M2 | Mock cluster — node fault injection + delivery checks | impl/mock-faults | ✅ PR #33 | Fault injection |

---

## Test Coverage

| Layer | Target | Current |
|-------|--------|---------|
| Core unit tests | ~150 | 157 |
| Adapter unit tests | ~80 | 44 (ractor + kameo) |
| Conformance tests | ~50 | 6 |
| Integration tests | ~30 | 0 |
| **Total** | **~310** | **201+** |

---

## Change Log

| Date | PR | Change |
|------|-----|--------|
| 2026-03-30 | PR 1 | Module reorganization: split traits/runtime.rs, feature-gate test_support, NodeId(String), serde optional |
| 2026-03-30 | PR 2 | Actor trait, ActorId, ErrorAction, ActorContext, SpawnConfig, ActorError stub |
| 2026-03-30 | PR 3 | dactor-test-harness crate: gRPC control protocol, TestNode, TestCluster, FaultInjector, EventStream |
| 2026-03-30 | PR 4 | Message trait, Handler<M> async trait, TypedActorRef<A> trait |
| 2026-03-30 | PR 5 | tell() on TypedActorRef, V2TestRuntime, type-erased Dispatch, 7 e2e tests |
| 2026-03-30 | PR 6 | ask() on TypedActorRef, AskReply future, RuntimeError enum, AskDispatch |
| 2026-03-30 | PR 7 | HeaderValue trait, Headers, MessageId, RuntimeHeaders, Envelope, Priority |
| 2026-03-30 | PR 8 | InboundInterceptor trait, Disposition, SendMode, InboundContext, Outcome, SpawnOptions |
| 2026-03-31 | PR 9 | OutboundInterceptor trait, OutboundContext, sender-side pipeline |
| 2026-03-31 | PR 10 | Lifecycle: stop(), on_error→ErrorAction, expanded ActorContext |
| 2026-03-31 | PR 11 | MailboxConfig (Unbounded/Bounded), OverflowStrategy, MailboxSender/Receiver |
| 2026-03-31 | PR 12 | ChildTerminated message, watch/unwatch, type-erased watcher notifications |
| 2026-03-31 | PR 13 | StreamHandler, StreamSender, BoxStream, stream() on TypedActorRef |
| 2026-03-31 | PR 14 | FeedMessage, FeedHandler, StreamReceiver, feed() with drain task |
| 2026-03-31 | PR 15 | CancellationToken, ctx.cancelled(), cancel_after(), RuntimeError::Cancelled |
| 2026-04-01 | PR 16 | ActorError with ErrorCode/details/chain, NotSupportedError, 11 error codes |
| 2026-04-01 | PR 17 | Persistence traits (Journal/Snapshot/State storage), InMemoryStorage, PersistenceId |
| 2026-04-01 | PR 18 | DeadLetterHandler, CollectingDeadLetterHandler, ActorRateLimiter throttle |
| 2026-04-01 | PR 19 | MetricsInterceptor, MetricsStore, ActorMetrics with latency percentiles |
| 2026-04-01 | PR 20 | Conformance test suite (6 standardized tests for runtime verification) |
| 2026-04-01 | PR 21 | Remote stubs: WireEnvelope, MessageSerializer, ClusterState, ClusterDiscovery |
| 2026-04-01 | PR R1 | Ractor adapter v0.2: RactorRuntime, RactorActorRef, spawn/tell/ask/stream/feed |
| 2026-04-01 | PR R2 | Ractor inbound + outbound interceptor pipelines |
| 2026-04-01 | PR R3 | Comprehensive ractor adapter tests |
| 2026-04-01 | PR K1 | Kameo adapter v0.2: KameoRuntime, KameoActorRef, full API |
| 2026-04-01 | PR M1 | dactor-mock: MockCluster, MockNode, MockNetwork, cross-node messaging |
| 2026-04-01 | PR C1 | dactor-coerce stub crate |
| 2026-04-01 | cleanup | Remove V2 prefix from all types, drop old v0.1 API |
| 2026-04-01 | cleanup | Extract shared dispatch module from test_runtime |
| 2026-04-01 | PR M2 | Mock cluster: node fault injection, delivery checks |
