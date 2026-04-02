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
| Adapter — dactor-ractor | PR R1–R4 | ✅ Complete |
| Adapter — dactor-kameo | PR K1–K2 | ✅ Complete |
| Adapter — dactor-mock | PR M1–M2 | ✅ Complete |
| Adapter — dactor-coerce | PR C1 | ✅ Complete (stub) |
| Cleanup — dispatch extraction, V2 prefix removal | cleanup | ✅ Complete |
| Docs — comprehensive README | docs | ✅ Complete |

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
| R4 | Ractor — watch/unwatch + mailbox config | impl/ractor-watch-mailbox | ✅ PR #35 | DeathWatch + MailboxConfig |
| K1 | Kameo adapter v0.2 — full implementation | impl/kameo-v2 | ✅ PR #28 | Full v0.2 adapter |
| K2 | Kameo — watch/unwatch + mailbox config | impl/kameo-watch-mailbox | ✅ PR #36 | DeathWatch + MailboxConfig |
| M1 | dactor-mock — MockCluster, MockNode, MockNetwork | impl/mock-cluster | ✅ PR #29 | Multi-node simulation |
| C1 | dactor-coerce adapter crate (stub) | impl/coerce-stub | ✅ PR #30 | Stub, no runtime wired |
| — | Remove V2 prefix and old v0.1 API | impl/cleanup-v2-prefix | ✅ PR #31 | Naming cleanup |
| — | Refactor: Extract shared dispatch module | impl/dispatch-extract | ✅ PR #32 | Shared dispatch module |
| — | Comprehensive README | impl/readme | ✅ PR #34 | Full project documentation |
| M2 | Mock cluster — node fault injection + delivery checks | impl/mock-faults | ✅ PR #33 | Fault injection |

---

## Test Coverage

| Layer | Target | Current |
|-------|--------|---------|
| Core unit tests | ~150 | 155 |
| Adapter unit tests | ~80 | 60 (30 ractor + 30 kameo) |
| Mock tests | ~50 | 16 |
| Coerce stub tests | — | 10 |
| Integration tests | ~30 | 5 (harness) |
| **Total** | **~310** | **~246** |

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
| 2026-04-01 | docs | Comprehensive README |
| 2026-04-01 | PR M2 | Mock cluster: node fault injection, delivery checks |
| 2026-04-01 | PR R4 | Ractor watch/unwatch + mailbox config |
| 2026-04-01 | PR K2 | Kameo watch/unwatch + mailbox config |

---

## Phase 3: Feature Completion & E2E Testing

Based on design spec audit (§4-§17 vs implementation), the following work items remain:

### 3.1 Missing Features from Design Spec

| # | Feature | Design Section | Priority | Status |
|---|---------|----------------|----------|--------|
| F1 | Stream/Feed batching (BatchConfig) | §4.11.1 | High | 🔲 Not started |
| F2 | Actor Pool (PoolRef, routing) | §4.14 | Medium | 🔲 Not started |
| F3 | EventSourced/DurableState actor traits | §6.3.2-6.3.4 | Medium | 🔲 Types only |
| F4 | Supervision strategies (OneForOne, etc.) | §6.1 | Low | 🔲 Design only |
| F5 | Priority mailbox scheduling | §5.7-5.9 | Low | 🔲 Priority header exists |
| F6 | on_reply wiring for outbound interceptors | §5.3 | Low | 🔲 Trait exists, not wired |
| F7 | Timer methods (send_after, send_interval) | §4.5 | Low | 🔲 TimerHandle trait only |

### 3.2 Sample Code for All Key Features

| # | Example | Feature | Status |
|---|---------|---------|--------|
| E1 | basic_counter | tell/ask | ✅ Done |
| E2 | streaming | stream/feed | ✅ Done |
| E3 | interceptors | inbound/outbound | ✅ Done |
| E4 | supervision | watch/ChildTerminated | ✅ Done |
| E5 | bounded_mailbox | MailboxConfig | ✅ Done |
| E6 | persistence | JournalStorage/SnapshotStorage | ✅ Done |
| E7 | cancellation | CancellationToken/cancel_after | 🔲 Not started |
| E8 | error_handling | ErrorCode/error chains | 🔲 Not started |
| E9 | metrics | MetricsInterceptor | 🔲 Not started |
| E10 | dead_letters | DeadLetterHandler | 🔲 Not started |
| E11 | rate_limiting | ActorRateLimiter | 🔲 Not started |
| E12 | batch_streaming | BatchConfig (after F1) | 🔲 Not started |

### 3.3 E2E Integration Tests (Test Harness)

Real multi-process cluster tests using dactor-test-harness with gRPC control:

| # | Test | Provider | Status |
|---|------|----------|--------|
| T1 | Ractor 2-node: spawn + tell/ask cross-check | ractor | 🔲 Not started |
| T2 | Ractor 3-node: crash node + watch notification | ractor | 🔲 Not started |
| T3 | Ractor: partition + heal + verify recovery | ractor | 🔲 Not started |
| T4 | Kameo 2-node: spawn + tell/ask | kameo | 🔲 Not started |
| T5 | Kameo 3-node: crash + restart | kameo | 🔲 Not started |
| T6 | Coerce 2-node: spawn + tell/ask | coerce | 🔲 Not started |
| T7 | Happy path: 100 messages in order across nodes | all | 🔲 Not started |
| T8 | Corner case: send to stopped actor | all | 🔲 Not started |
| T9 | Corner case: concurrent asks from multiple callers | all | 🔲 Not started |
| T10 | Corner case: stream with slow consumer | all | 🔲 Not started |

### 3.4 Stream/Feed Message Batching

Reduce message overhead in streaming by batching multiple values:

| # | Item | Description | Status |
|---|------|-------------|--------|
| B1 | BatchConfig struct | max_items + max_delay configuration | 🔲 Not started |
| B2 | BatchWriter | Accumulates items, flushes on count or timeout | 🔲 Not started |
| B3 | BatchReader | Unpacks batched items into individual items | 🔲 Not started |
| B4 | stream_batched() | ActorRef method with BatchConfig param | 🔲 Not started |
| B5 | feed_batched() | ActorRef method with BatchConfig param | 🔲 Not started |
| B6 | Tests | Verify batching reduces message count | 🔲 Not started |
| B7 | Example | Demonstrate batch streaming | 🔲 Not started |

### Recommended Execution Order

1. **F1 + B1-B7**: Stream batching (highest user-requested priority)
2. **E7-E11**: Sample code for remaining features
3. **T1-T3**: Ractor E2E tests (validates real multi-process)
4. **T4-T6**: Kameo/Coerce E2E tests
5. **F3**: EventSourced/DurableState actor integration
6. **F2**: Actor Pool
7. **T7-T10**: Cross-adapter corner case tests
8. **F4-F7**: Remaining design features
