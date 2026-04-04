# dactor v0.2 — Implementation Progress

> Tracks PR status for the v0.2 implementation. See [dev-plan.md](dev-plan.md) for full plan details.

---

## Current Status (PR #66)
- Phase 3: ✅ Complete (features, examples, conformance, batching)
- Phase 4: R1-R3 ✅, S1-S4 ✅, SE1-SE4 ✅, C1-C5 ✅ — R4-R5 & SE5 & P1-P3 pending
- Phase 6: ✅ Complete (supervision, pools, timers, on_reply, message comparer)
- Phase 7: ✅ Complete (metrics, dead letters, circuit breaker, drop observer)
- Phase 8: NR1 done (actor registry), NR2-NR4 need Phase 4
- Zero clippy warnings, cargo doc clean
- Next: Phase 4 R4-R5 (Connection mgmt, Batched remote sends) + P1-P3 (Remote spawn)

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
| 2026-04-04 | PR R1 | Transport trait, InMemoryTransport, TransportRegistry |
| 2026-04-04 | PR R2 | WireEnvelope pipeline: TypeRegistry, JsonSerializer, HeaderRegistry, wire send/receive helpers |
| 2026-04-04 | PR R3 | RemoteActorRef: location-transparent remote actor ref with tell/ask via transport |
| 2026-04-04 | PR S1-S4 | System actors: SpawnManager, WatchManager, CancelManager, NodeDirectory |
| 2026-04-04 | PR C1-C5 | Cluster management: ClusterEventEmitter, AdapterCluster, HealthChecker, UnreachableHandler |

---

## Phase 3: Feature Completion & E2E Testing

Based on design spec audit (§4-§17 vs implementation), the following work items remain:

### 3.1 Missing Features from Design Spec

| # | Feature | Design Section | Priority | Status |
|---|---------|----------------|----------|--------|
| F1 | Stream/Feed batching (BatchConfig) | §4.11.1 | High | ✅ PR #42 |
| F2 | Actor Pool (PoolRef, routing) | §4.14 | Medium | ✅ PR #45 (local) |
| F3 | EventSourced/DurableState actor traits | §6.3.2-6.3.4 | Medium | ✅ PR #44 |
| F4 | Supervision strategies (OneForOne, etc.) | §6.1 | Low | ✅ PR #46 |
| F5 | Priority mailbox scheduling | §5.7-5.9 | Low | ✅ PR #46 |
| F6 | on_reply wiring for outbound interceptors | §5.3 | Low | ✅ PR #46 |
| F7 | Timer methods (send_after, send_interval) | §4.5 | Low | ✅ PR #46 |

### 3.2 Sample Code for All Key Features

| # | Example | Feature | Status |
|---|---------|---------|--------|
| E1 | basic_counter | tell/ask | ✅ Done |
| E2 | streaming | stream/feed | ✅ Done |
| E3 | interceptors | inbound/outbound | ✅ Done |
| E4 | supervision | watch/ChildTerminated | ✅ Done |
| E5 | bounded_mailbox | MailboxConfig | ✅ Done |
| E6 | persistence | JournalStorage/SnapshotStorage | ✅ Done |
| E7 | cancellation | CancellationToken/cancel_after | ✅ PR #43 |
| E8 | error_handling | ErrorCode/error chains | ✅ PR #43 |
| E9 | metrics | MetricsInterceptor | ✅ PR #43 |
| E10 | dead_letters | DeadLetterHandler | ✅ PR #43 |
| E11 | rate_limiting | ActorRateLimiter | ✅ PR #43 |
| E12 | batch_streaming | BatchConfig | ✅ Done |

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
| T11 | E2E: remote ask with version breaking change — sender v1, receiver v2, verify MessageVersionHandler migration or rejection | all | 🔲 Not started |

### 3.4 Stream/Feed Batching (PR #42)

| # | Item | Status |
|---|------|--------|
| B1 | BatchConfig (max_items + max_delay + max_bytes) | ✅ Done |
| B2 | BatchWriter (generic push + Vec\<u8\>::push_bytes) | ✅ Done |
| B3 | BatchReader (unbatching) | ✅ Done |
| B4 | Merged stream()/stream_batched() → Option\<BatchConfig\> | ✅ Done |
| B5 | Merged feed()/feed_batched() → Option\<BatchConfig\> | ✅ Done |
| B6 | Removed FeedMessage trait (direct Item/Reply generics) | ✅ Done |
| B7 | Per-item on_stream_item interception with Disposition | ✅ Done |
| B8 | DropObserver trait for observing interceptor drops | ✅ Done |
| B9 | runtime_support module (shared OutboundPipeline helpers) | ✅ Done |
| B10 | ContentLength built-in header | ✅ Done |
| B11 | Example: batch_streaming | ✅ Done |
| B12 | Tests (count, bytes, deadline, oversized, pre-flush) | ✅ Done |

### Recommended Execution Order (Phase 3)

1. ~~**F1 + B1-B12**: Stream batching~~ ✅ Complete (PR #42)
2. ~~**E7-E11**: Sample code for remaining features~~ ✅ Complete (PR #43)
3. **T1-T3**: Ractor E2E tests (validates real multi-process) — ⚠️ Blocked by protoc permission issue
4. **T4-T6**: Kameo/Coerce E2E tests — ⚠️ Blocked by protoc permission issue
5. ~~**F3**: EventSourced/DurableState actor integration~~ ✅ Complete (PR #44)
6. ~~**F2**: Actor Pool~~ ✅ Complete (PR #45, local)
7. **T7-T10**: Cross-adapter corner case tests — ⚠️ Blocked by protoc permission issue
8. ~~**F4-F7**: Remaining design features~~ ✅ Complete (PR #46)
9. ~~**Design doc cleanup**~~ ✅ Complete (PR #47)

---

## Phase 4: Remote Transport & System Actors

Wire format, cross-node communication, and system actors for remote operations.

### 4.1 Transport Layer

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| R1 | Transport trait | §9 | Abstract transport interface (gRPC, TCP, etc.) | ✅ PR #62 |
| R2 | WireEnvelope send/receive | §9.1 | Serialize messages → WireEnvelope → transport → deserialize | ✅ PR #63 |
| R3 | RemoteActorRef | §9.3 | ActorRef impl that serializes + sends via transport | ✅ PR #64 |
| R3b | RemoteActorRef outbound interceptors | §5.3, §9.6 | Wire OutboundInterceptor pipeline into RemoteActorRef tell/ask (on_send, on_reply, header stamping) | ✅ PR #68 |
| R4 | Connection management | §10.2 | AdapterCluster: connect(), disconnect(), reconnect |
| R5 | Batched remote sends | §4.11.1 | BatchedTransportSender batches WireEnvelopes per-node → single transport call | ✅ PR #73 |
| R6 | WireInterceptor (envelope-level) | §9.0.4 | Intercept WireEnvelopes at the transport boundary using only headers + body bytes (no deserialization). Enables runtime-level load control: delay, reject, drop, rate-limit, prioritize remote messages before they enter the actor mailbox. Runs on receiver side between Transport and dispatch. | ✅ PR #69 |
| R6b | WireEnvelope target_name field | §9.0.4 | Add target_name: String to WireEnvelope so wire interceptors can inspect the actor name without deserialization | ✅ PR #70 |
| R6c | Wire interceptor metrics & dead letters | §9.0.4, §11.2, §5.4 | Report Drop/Reject/Delay decisions to RuntimeMetrics (counters: wire_dropped, wire_rejected, wire_delayed) and route dropped/rejected envelopes to DeadLetterHandler with DeadLetterReason::WireInterceptorDrop/WireInterceptorReject | ✅ PR #70 |

### 4.2 System Actors

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| S1 | SpawnManager | §8.2 | Process remote actor spawn requests | ✅ PR #65 |
| S2 | WatchManager | §8.2, §6.2 | Handle remote watch/unwatch, deliver ChildTerminated | ✅ PR #65 |
| S3 | CancelManager | §8.2, §4.13 | Process remote cancellation requests | ✅ PR #65 |
| S4 | NodeDirectory | §8.3 | Map NodeId → peer system actor refs | ✅ PR #65 |

#### 4.2.1 Adapter Integration for System Actors

Each adapter runtime must wire the core system actors into its remote message handling:

| # | Adapter | Feature | Description | Status |
|---|---------|---------|-------------|--------|
| SA1 | dactor-ractor | SpawnManager wiring | RactorRuntime starts SpawnManager, routes incoming SpawnRequest via ractor's remote channel, calls create_actor + local spawn | 🔲 Not started |
| SA2 | dactor-ractor | WatchManager wiring | RactorRuntime starts WatchManager, translates ractor's native watch into WatchManager entries, sends WatchNotification on termination | 🔲 Not started |
| SA3 | dactor-ractor | CancelManager wiring | RactorRuntime registers CancellationTokens with CancelManager, handles incoming CancelRequest from remote nodes | 🔲 Not started |
| SA4 | dactor-ractor | NodeDirectory wiring | RactorRuntime populates NodeDirectory from ractor_cluster membership, updates PeerStatus on connect/disconnect | 🔲 Not started |
| SA5 | dactor-kameo | SpawnManager wiring | KameoRuntime starts SpawnManager, routes SpawnRequest via kameo's distributed actor layer | 🔲 Not started |
| SA6 | dactor-kameo | WatchManager wiring | KameoRuntime wires kameo's actor lifecycle into WatchManager for remote watch delivery | 🔲 Not started |
| SA7 | dactor-kameo | CancelManager wiring | KameoRuntime registers tokens with CancelManager, handles remote cancel | 🔲 Not started |
| SA8 | dactor-kameo | NodeDirectory wiring | KameoRuntime populates NodeDirectory from kameo/libp2p peer discovery | 🔲 Not started |
| SA9 | dactor-mock | System actor wiring | MockCluster wires SpawnManager + WatchManager + CancelManager for simulated multi-node testing | 🔲 Not started |
| SA10 | dactor-coerce | System actor wiring | CoerceRuntime wires system actors (stub — depends on coerce sharding integration) | 🔲 Not started |

### 4.3 Serialization & Schema

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| SE1 | MessageSerializer integration | §9.1 | Wire MessageSerializer into transport layer | ✅ PR #63 (JsonSerializer) |
| SE2 | TypeRegistry | §8.3, §9.2 | Map type names → deserializers for remote dispatch | ✅ PR #63 |
| SE3 | HeaderRegistry | §5.1 | Deserializer registry for remote headers | ✅ PR #63 |
| SE4 | Message versioning | §9.1 | MessageVersionHandler for schema evolution/migration | ✅ PR #63 (receive_envelope_body_versioned) |
| SE5 | ActorRef serialization | §9.3 | Serialize/deserialize ActorRef for cross-node passing | ✅ PR #72 (ActorRefEnvelope) |
| SE6 | Protobuf system serialization | §9.1 | Replace JSON with protobuf for all system-level serialization (SpawnRequest/Response, WatchRequest/Notification, CancelRequest/Response, WireEnvelope framing). Protobuf provides smaller payloads, faster ser/deser, and native schema evolution via field numbering. Application messages remain pluggable via MessageSerializer. | 🔲 Not started |

### 4.4 Cluster Discovery & Health

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| C1 | ClusterDiscovery wiring | §10.1 | Wire discovery into adapter startup | ✅ PR #66 (StaticSeeds + AdapterCluster) |
| C2 | ClusterEventEmitter | §10.1 | Emit node_joined/node_left events | ✅ PR #66 |
| C3 | ClusterState API | §10.4 | runtime.cluster_state() for topology queries | ✅ PR #21 (ClusterState struct) |
| C4 | PeerStatus tracking | §10.4 | Connected, Connecting, Unreachable, Disconnected | ✅ PR #65 (NodeDirectory) |
| C5 | Health delegation | §10.3 | Delegate health checks to provider, on_node_unreachable | ✅ PR #66 (HealthChecker + UnreachableHandler) |

### 4.5 Remote Spawn & Placement

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| P1 | Remote spawn | §9.2 | SpawnConfig.target_node for spawning on specific nodes | ✅ PR #71 |
| P2 | ActorFactory trait | §9.2 | Factory for remote actor reconstruction | ✅ PR #71 |
| P3 | Location transparency | §9.2 | Caller doesn't know if actor is local or remote | ✅ PR #64 (RemoteActorRef implements ActorRef<A>) |

---

## Phase 5: Persistence Integration

Wire persistence traits into actor lifecycle (recovery, snapshots, durable state).

### 5.1 Event Sourcing

| # | Feature | Design Section | Description |
|---|---------|----------------|-------------|
| ES1 | PersistentActor trait | §6.3.2 | persistence_id(), pre_recovery(), post_recovery() | ✅ PR #44 |
| ES2 | EventSourced trait | §6.3.3 | apply(), persist(), persist_batch(), snapshot() | ✅ PR #44 |
| ES3 | SnapshotConfig | §6.3.5 | Automatic snapshotting rules | ✅ Exists |
| ES4 | Recovery pipeline | §6.3.7 | Load snapshot → replay events → post_recovery | ✅ PR #44 |
| ES5 | RecoveryFailurePolicy | §6.3.8 | Stop, Retry, SkipAndStart | ✅ PR #44 |
| ES6 | PersistFailurePolicy | §6.3.8 | Stop, ReturnError, Retry | ✅ Exists |

### 5.2 Durable State

| # | Feature | Design Section | Description |
|---|---------|----------------|-------------|
| DS1 | DurableState trait | §6.3.4 | save_state(), automatic save via SaveConfig | ✅ PR #44 |
| DS2 | SaveConfig | §6.3.5 | Rules for automatic state persistence | ✅ Exists |

### 5.3 Storage Backends

| # | Feature | Design Section | Description |
|---|---------|----------------|-------------|
| SB1 | StorageProvider trait | §6.3.6 | Pluggable storage backend abstraction | ✅ PR #44 |
| SB2 | InMemoryStorage | §6.3.6 | ✅ Done (testing/dev) | ✅ |
| SB3 | dactor-sqlite crate | §6.3.6 | SQLite storage for single-node production |
| SB4 | dactor-postgres crate | §6.3.6 | PostgreSQL storage for multi-node production |

---

## Phase 6: Actor Pools & Advanced Features

### 6.1 Actor Pool

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| AP1 | PoolRef\<A, R\> (local) | §4.14 | Handle to a local pool of worker actors | ✅ PR #45 |
| AP2 | PoolConfig | §4.14 | Pool size, routing strategy | ✅ PR #45 |
| AP3 | PoolRouting | §4.14 | RoundRobin, Random, KeyBased | ✅ PR #45 |
| AP4 | Keyed trait | §4.14 | Extract routing keys for key-based routing | ✅ PR #45 |
| AP5 | TestRuntime::spawn_pool() | §4.14 | Spawn N local workers | ✅ PR #45 |
| AP6 | LeastLoaded routing | §4.14 | Route to worker with fewest queued messages | 🔲 Needs per-worker load tracking |
| AP7 | Distributed pool | §4.14 | Workers across nodes via remote ActorRef (Phase 4) | 🔲 Depends on R3 |
| AP8 | Virtual actor pool (v2) | §4.14 | Redesign pool as virtual router actor (see below) | 🔲 Design proposed |

#### AP8: Virtual Actor Pool (Proposed Redesign)

The current `PoolRef` is a passive data structure (Vec of refs + atomic counter)
that routes from the caller's thread. This has several limitations:

1. **Metrics contention** — pool workers share one `ActorMetricsHandle`; multiple
   worker tasks contend on its Mutex.
2. **Feed scatter** — items from a single feed stream can be routed to different
   workers since each `tell()` is an independent routing decision.
3. **No single-threaded guarantee** — multiple callers route concurrently.

**Proposed v2 design: Virtual Router Actor**

```
Caller-1 ──┐
Caller-2 ──┼──→ [Router mailbox] ──→ Router Actor (single-threaded)
Caller-3 ──┘                              ├──→ Worker-0
                                          ├──→ Worker-1 ──→ reply direct to caller
                                          └──→ Worker-2
```

The router is a **real actor** with its own mailbox:

- **Single-threaded routing** — all routing decisions happen sequentially in the
  router's task. Zero contention on routing state, metrics, and counters.
- **Feed affinity** — when a feed stream starts, the router picks one worker and
  pins all items from that stream to the same worker (sticky by stream).
- **Direct reply** — the router forwards the `reply_tx` (oneshot sender) to the
  worker along with the dispatch envelope. The worker replies directly to the
  caller without going back through the router. No proxy overhead on the reply
  path.
- **Zero-copy routing** — the router doesn't inspect or clone message contents.
  It immediately forwards the type-erased `BoxedDispatch<A>` to the selected
  worker and moves on to the next message.
- **Metrics** — the router actor has its own single-threaded `ActorMetricsHandle`
  for pool-level metrics (zero contention). Workers optionally have their own
  handles for per-worker breakdown.

**Implementation sketch:**

```rust
struct RouterActor<A: Actor> {
    workers: Vec<Box<dyn ActorRef<A>>>,
    routing: PoolRouting,
    counter: u64,
}

// Router receives BoxedDispatch<A> and forwards to a worker.
// For tell: forward dispatch, no reply.
// For ask: forward dispatch + reply_tx, worker replies directly.
// For feed: pick one worker, forward the entire StreamReceiver.

struct PoolActorRef<A: Actor> {
    router: ActorRef<RouterActor<A>>,  // send routing requests to router
}

impl<A: Actor> ActorRef<A> for PoolActorRef<A> {
    fn tell<M>(&self, msg: M) -> Result<(), ActorSendError> {
        // Wrap msg into BoxedDispatch, send to router's mailbox
        self.router.tell(RouteMessage(Box::new(TypedDispatch { msg })))
    }
}
```

**Challenges:**
- `ActorRef<A>` has generic methods (`tell<M>`, `ask<M>`) which can't be
  forwarded through a single-typed router mailbox without type erasure.
  The router must accept `BoxedDispatch<A>` (already used by all adapters).
- The router could become a bottleneck if routing is slower than the message
  rate. Since routing is "pick worker index + forward" (no processing), this
  is unlikely for most workloads.
- The current `PoolRef` should be kept as a simpler alternative for use cases
  that don't need feed affinity or single-threaded routing.

### 6.2 Supervision Strategies

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| SV1 | SupervisionStrategy trait | §6.1 | on_child_failed() → SupervisionAction | ✅ PR #46 |
| SV2 | OneForOne | §6.1 | Restart only the failed child | ✅ PR #46 |
| SV3 | OneForAll | §6.1 | Restart all children when one fails | ✅ PR #46 |
| SV4 | RestForOne | §6.1 | Restart failed child + children started after it | ✅ PR #46 |

### 6.3 Advanced Messaging

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| AM1 | Priority mailbox scheduling | §5.6-5.9 | Priority-based message ordering | ✅ PR #46 |
| AM2 | MessageComparer trait | §5.6 | Custom message ordering rules | ✅ PR #46 |
| AM3 | Timer methods (send_after, send_interval) | §4.5 | Scheduled message delivery | ✅ PR #46 |
| AM4 | on_reply wiring | §5.3 | OutboundInterceptor sees ask replies | ✅ PR #46 |
| AM5 | Outbound priority queue | §5.8 | Per-destination priority lanes | 🔲 Not started |

---

## Phase 7: Observability & Tooling

### 7.1 Metrics & Monitoring

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| O1 | MetricsInterceptor wiring | §11.2 | Wire built-in metrics into runtimes | ✅ PR #55 (per-actor ActorMetricsHandle, windowed buckets) |
| O2 | MetricsStore query API | §11.3 | Query per-actor and per-message-type metrics | ✅ PR #55 (MetricsRegistry with register/snapshot) |
| O3 | RuntimeMetrics | §11.6 | System-level: actor count, mailbox depth, uptime | ✅ PR #55 (message_rate, error_rate, windowed) |
| O4 | OtelInterceptor | §11.4 | OpenTelemetry tracing integration | 🔲 Not started (needs opentelemetry crate) |
| O5 | CircuitBreakerInterceptor | §11.4 | Error-rate circuit breaker | ✅ PR #56 |

### 7.2 Dead Letter Routing

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| DL1 | Wire DeadLetterHandler into runtimes | §5.4 | Route undelivered messages to handler | ✅ PR #54 |
| DL2 | Wire DropObserver into DeadLetterHandler | §5.4 | Interceptor drops → dead letters | ✅ PR #42 (DropObserver already in runtime_support) |

---

## Phase 8: Named Registry & Cluster Events

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| NR1 | Actor naming & registry | §8.3 | runtime.lookup(name) for named actor discovery | ✅ PR #58 |
| NR2 | ClusterEvent enum | §10.1, §10.4 | NodeJoined, NodeLeft push events | 🔲 Depends on Phase 4 |
| NR3 | Cluster event handlers | §10.4 | Actors subscribe to membership changes |
| NR4 | Processing groups | §2.2 | Actor group pub/sub (ractor pg, coerce sharding) |

---

## Not Planned / Out of Scope

| Feature | Reason |
|---------|--------|
| Hot code upgrade | Excluded from design (Erlang-only, §2.2) |
| Passivation | Not in design spec |
| Security (TLS, auth) | Deferred to interceptors + application code |
| CLI tools / dashboard | Not in design spec |
| Bidirectional streaming | Composed from feed() + stream() at app level |
