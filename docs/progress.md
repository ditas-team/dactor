# dactor v0.2 — Implementation Progress

> Tracks PR status for the v0.2 implementation. See [dev-plan.md](dev-plan.md) for full plan details.

---

## Current Status (PR #130)
- Phase 3: ✅ Complete (features, examples, conformance, batching, E2E tests)
- Phase 4: ✅ Complete — R1-R6, R3b, R6b-c, S1-S4, SA1-SA10, SE1-SE6, C1-C5, P1-P3 all done
- Phase 6: ✅ Complete (supervision, pools, timers, on_reply, AM1-AM7, AP7-AP8 all done)
- Phase 7: ✅ Complete (metrics, dead letters, circuit breaker, drop observer; O4 dropped)
- Phase 8: ✅ Complete (NR1-NR4: registry, cluster events, processing groups)
- Phase 9: ✅ Complete (NA1-NA10: native system actors, runtime auto-start, transport routing)
- Phase 10: ✅ Complete (JH1-JH5: lifecycle handles, await_stop, panic propagation)
- Phase 11: ✅ Complete (AS1-AS5,AS7: async spawn, Result return, removed std::thread bridge)
- Phase 12: ✅ Complete (CP1-CP10: coerce real actors, parity tests, mailbox config)
- Phase 13: ✅ Complete (TF1-TF7: TransformHandler, N→M streaming, batch support wired in all adapters)
- Phase 14: ✅ Complete (BC1-BC9: BroadcastRef, tell/ask, receipts, pool integration, dead letters, tests)
- AP6: ✅ Done (LeastLoaded pool routing + pending_messages trait method)
- AP7: ✅ Done (distributed pool with WorkerRef)
- AP8: ✅ Done (virtual actor pool with single-threaded router)
- Bounded mailbox: ✅ All 3 adapters + TestRuntime; shared BoundedMailboxSender in runtime_support
- T1-T11: ✅ Complete (E2E integration tests + corner cases + version migration)
- E2E: ✅ 60 tests (20 per adapter) — spawn/tell/ask, stop, partition/heal, errors, watch, crash, concurrent, large payloads, multi-actor, rapid lifecycle, slow handlers, cancellation/timeout, inter-actor forwarding, state snapshots
- SE6: ✅ Complete (protobuf system serialization)
- NA10: ✅ Complete (transport routing)
- Zero clippy warnings, 875+ tests, all workspace tests pass
- Build: `cargo clippy --workspace --exclude dactor-test-harness --all-targets --all-features -- -D warnings`
- Test: `cargo test --workspace --exclude dactor-test-harness --features test-support`
- E2E: `cargo test -p dactor-ractor --test e2e_tests --features test-harness` (+ kameo, coerce)

### Session 2026-04-06/07 Summary (PRs #111-#129)
- **NA10** (#111): Transport routing — SystemMessageRouter trait, route_system_envelope for all 3 adapters, wire protocol stability tests (31 new tests)
- **SE6** (#112): Protobuf system serialization — proto/system.proto schemas, prost encode/decode, removed serializer param from trait, size limits + validation (26 proto unit tests)
- **T1-T3** (#113): Ractor E2E integration tests — extended test harness with SpawnActor/TellActor/AskActor/StopActor RPCs, CommandHandler trait, test-node-ractor binary (3 E2E tests)
- **T4-T6** (#114): Kameo + Coerce E2E tests — test-node-kameo and test-node-coerce binaries, KameoCommandHandler + CoerceCommandHandler (3 E2E tests)
- **T7-T10** (#115): Cross-adapter corner case tests — wired conformance suite to coerce (7 tests: ordering, stop, concurrent asks, slow consumer, multiple handlers)
- **T11** (#116): Version migration and rejection tests — 7 tests covering all receive_envelope_body_versioned code paths with panicking handlers
- **Progress** (#117): Update progress.md for session
- **README** (#118): Clean up review artifact + update README (coerce status, new features, test counts)
- **Docs** (#119): Mark ExpandHandler refactor done + update pending section
- **AP7** (#120): Distributed pool — WorkerRef enum wrapping local/remote refs for mixed pools
- **AP8** (#121): Virtual actor pool — VirtualPoolRef with single-threaded router task, bounded channel, backpressure
- **Progress** (#122): Final progress update — AP7/AP8 complete
- **E2E suite** (#123): Comprehensive E2E tests — 20 new tests (error handling, concurrent ops, partition parity, graceful shutdown) across all 3 adapters
- **Watch+Crash** (#124): WatchActor RPC + node crash detection — 6 new tests, watch termination notification, duplicate watch prevention
- **Advanced E2E** (#126): Large payloads (100KB echo), multi-actor interaction, rapid spawn/stop lifecycle, slow handler isolation — 12 new tests, timing-sensitive assertions use polling
- **Docs** (#127): Update test counts for session
- **Cancellation** (#128): Ask timeout support — timeout_ms on AskActorRequest, SlowEcho message, 6 new timeout E2E tests
- **Inter-actor** (#129): Forward increment + state snapshots — inter-actor forwarding, chained forwarding, state snapshot E2E tests (9 new)

### Key Design Decisions Made This Session
- Wire protocol constants are frozen strings with regression test (not Rust paths)
- System messages use fixed protobuf format (prost) — no external MessageSerializer needed
- SystemMessageRouter::route_system_envelope removed serializer parameter
- Proto generated types are pub(crate) (not public API surface)
- MAX_SYSTEM_MSG_SIZE = 4MB, MAX_WIRE_ENVELOPE_SIZE = 64MB pre-decode limits
- decode_* functions validate non-empty required fields (type_name, request_id, node_id)
- encode_* functions take parameters by value (no unnecessary clones)
- Test harness CommandHandler trait with adapter_name() for adapter-agnostic E2E testing
- Test node binaries use with_node_id(env DACTOR_NODE_ID) for proper multi-node identity
- stop_actor returns Err on 1s timeout, spawn_actor rejects duplicate names
- All binaries bind to 127.0.0.1 (not 0.0.0.0) for security
- Version migration tests use panicking handlers to prove skip behavior
- WorkerRef<A, L> enum wraps Local or Remote refs; PoolRef<A, WorkerRef<A, L>> = distributed pool
- WorkerRef requires A: Sync (inherited from RemoteActorRef); streaming on remote is documented limitation
- VirtualPoolRef uses bounded mpsc channel (default 1024) with try_send backpressure
- VirtualPoolRef routes via boxed closures erasing generic message types
- VirtualPoolRef::stop() sets alive=false immediately before queuing Stop command
- WatchActor test harness uses application-level watch (not native runtime watch)
- Watch notifications release actors lock before notifying (no lock contention)
- Duplicate watch entries prevented at registration time
- Watch test uses polling loop (100ms × 20 = 2s) instead of fixed sleep
- E2E tests use 500ms timeouts for partition heal and concurrent ops robustness
- Node crash detection tests use graceful shutdown + kill (validates reachability)

### Multi-Model Review Process
Every PR was reviewed by 4 AI models (GPT-5.4, Gemini/Goldeneye, Claude Haiku 4.5, Claude Sonnet 4.5). Key bugs caught:
- Allocation DoS: no size limits on protobuf decode (fixed: MAX_SYSTEM_MSG_SIZE)
- Empty proto3 scalars accepted as valid (fixed: validate non-empty fields)
- Mutex held across await in ask_actor (fixed: clone ref, drop lock before await)
- stop_actor returned Ok on timeout (fixed: return Err after 1s)
- Test binaries ignored DACTOR_NODE_ID (fixed: with_node_id)
- Generated proto types leaked into public API (fixed: pub(crate))
- Process leak on build() panic (fixed: kill spawned processes before panic)
- Version match test didn't prove handler skip (fixed: panicking handler)

### All Work Complete
All planned work items from the v0.2 dev plan have been implemented:
- ✅ NA10: Transport routing
- ✅ SE6: Protobuf system serialization
- ✅ T1-T11: E2E integration tests + corner cases + version migration
- ✅ AP7: Distributed pool (WorkerRef)
- ✅ AP8: Virtual actor pool (VirtualPoolRef)

### Documentation Updated This Session
- `dactor-coerce/docs/implementation.md` — bounded mailbox parity table + limitations update

### Session 2026-04-05 Summary (PRs #80-#95)
- **SA5-SA8** (#81): KameoRuntime system actor wiring
- **SA10** (#82): CoerceRuntime system actor wiring + dual ID counter fix
- **NA1-NA4** (#83): Native ractor system actors — real ractor::Actor impls with mailboxes
- **NA5-NA8** (#84): Native kameo system actors — per-message-type kameo::message::Message impls
- **NA9** (#85): Runtime auto-start — start_system_actors() with lazy initialization
- **NR2-NR3** (#86): ClusterEvent emission wired into connect_peer/disconnect_peer
- **JH1-JH3** (#87): Lifecycle handles — await_stop, await_all, cleanup_finished + adapter impl docs
- **CP1-CP4,CP7-CP8** (#88): Coerce adapter rewrite — real coerce-rs actors replacing TestRuntime stub
- **CP10** (#89): Coerce parity tests — 29 new tests (interceptors, lifecycle, stream, feed, watch, events)
- **CP5-CP6** (#90): Coerce native system actors + runtime auto-start + Phase 14 broadcast plan
- **Rename** (#91): stream→expand, feed→reduce + cluster-behavior.md docs + handle_stream→handle_expand, handle_feed→handle_reduce
- **JH4-JH5** (#92): Panic propagation through await_stop + TestRuntime lifecycle handles
- **AS1-AS5,AS7** (#93): Async spawn — all spawn methods async, return Result, removed ractor std::thread bridge
- **TF1-TF3,TF5-TF7** (#94): TransformHandler — N→M streaming pattern completing the cardinality matrix
- **Rename** (#95): Transform type params Item→InputItem, Output→OutputItem (+ reduce/expand pending)

### Key Design Decisions Made This Session
- System actors are plain structs wrapped in native provider actors (not standalone)
- Native actor spawning is lazy via start_system_actors() — new() stays sync
- Dual struct+actor pattern: struct for backward-compat sync API, actor for transport routing
- register_factory() forwards to both struct and native actor via Arc-wrapped factory
- Lifecycle handles use oneshot channels (all backends), not JoinHandle (ractor-specific)
- on_stop() panics caught via catch_unwind, propagated through await_stop() as Err(String)
- Streaming API renamed for cardinality semantics: stream→expand(1→N), feed→reduce(N→1)
- TransformHandler is the 5th call pattern completing: tell(1→0), ask(1→1), expand(1→N), reduce(N→1), transform(N→M)
- ExpandHandler refactored: M::Reply → explicit OutputItem generic (completed)
- Coerce adapter upgraded from TestRuntime stub to real coerce-rs actors (coerce 0.8.11)
- DactorMsg uses Mutex<Option<Box<dyn Dispatch>>> for Sync safety in coerce (not unsafe impl)
- ClusterEvent emission uses catch_unwind for subscriber panic isolation
- connect_peer() preserves address on reconnect (address-only None doesn't overwrite)
- Pre-release: API changes done freely (no backward compat needed)
- Phase 14 broadcast plan: BroadcastRef<A> with tell(M: Clone) and ask(timeout) → Vec<BroadcastReceipt>

### Multi-Model Review Process
Every PR was reviewed by 4 AI models (GPT-5.4, Gemini/Goldeneye, Claude Haiku 4.5, Claude Sonnet 4.5).
Findings were consolidated, addressed, and verified before merge. Key patterns found:
- Dual node ID / counter collision in coerce stub (fixed with REMOTE_ID_OFFSET, then real actors)
- unsafe impl Sync for DactorMsg was UB — replaced with Mutex wrapper
- ActorType::Anonymous breaks coerce system shutdown — changed to Tracked
- handle_spawn_request() originally dropped created actor — now returns Result<(ActorId, Box<dyn Any>)>
- SpawnResult as Result promotes domain failures to kameo handler errors — replaced with SpawnOutcome enum
- wrap_stream_with_interception hardcoded SendMode::Expand — now takes send_mode parameter
- on_transform_complete ran after cancellation — now gated on cancelled flag
- Ractor stop_reason inference bug — track on_stop_panicked independently via local bool
- await_all() early return on first error — now collects first error but awaits ALL actors
- &'static str in conformance helpers required by Rust async closure lifetime rules

### Pending Work for Next Session
All v0.2 planned work items and stretch goals are now complete.

### New Work Items

#### PUB1: Usage Documentation for crates.io

**Goal:** Create a polished, comprehensive usage document before publishing dactor to crates.io.

**Design focus:**
- **What is dactor** — framework overview, provider-agnostic actor abstraction, multi-runtime support
- **Why need dactor** — pain points with existing actor frameworks, vendor lock-in, portability, testability
- **How to use dactor** — getting started guide, communication patterns, interceptors, persistence, pools, streaming, supervision, cluster management

**Deliverable:** `docs/usage-guide.md` (or `GUIDE.md` at workspace root) — a standalone guide suitable for crates.io documentation and GitHub landing page.

**Status:** ✅ Done

---

#### PUB2: Real-World Sample Application

**Goal:** Create a separate sample crate demonstrating a real-world hosting system built with dactor.

**Scope:**
- Standalone crate (e.g., `examples/dactor-sample-app/` or `dactor-sample/`)
- Demonstrates a realistic use case: e.g., distributed task queue, chat server, IoT device manager, or order processing pipeline
- Uses one or more adapters (ractor/kameo/coerce)
- Exercises core features: tell/ask, streaming, persistence, supervision, pools, interceptors, cluster events
- Includes README with architecture diagram and run instructions

**Status:** ✅ Done (PR #134 — task queue example)

---

#### PUB3: Cloud Hosting Discovery Crates

**Goal:** Define hosting utility crates that enable dactor to discover and manage nodes on cloud platforms.

**Crates:**
- `dactor-discover-k8s` — Kubernetes node discovery (AKS, EKS, GKE)
  - Uses Kubernetes API (pod labels, headless services, StatefulSets) for peer discovery
  - Implements `ClusterDiscovery` trait from dactor core
  - Works with AKS (Azure), EKS (Amazon), GKE (Google), and vanilla Kubernetes
- `dactor-discover-aws` — AWS Auto Scaling discovery (EC2, ECS)
  - Uses AWS APIs (Auto Scaling Groups, EC2 instance metadata) for peer discovery
  - Implements `ClusterDiscovery` trait
- `dactor-discover-azure` — Azure VMSS discovery
  - Uses Azure Instance Metadata Service + VMSS APIs for peer discovery
  - Implements `ClusterDiscovery` trait

**Each crate provides:**
- `ClusterDiscovery` implementation for the platform
- Node health checking via platform APIs
- Graceful shutdown integration (drain, deregister)
- Configuration via environment variables or builder pattern
- Example and documentation

**Status:** ✅ Done (PRs #135, #136, #138 — K8s, AWS, Azure crates)

---

#### PUB4: Async ClusterDiscovery API with Result

**Goal:** Update the `ClusterDiscovery` trait to be async-native and return `Result`.

The current `ClusterDiscovery` trait is synchronous and infallible:
```rust
pub trait ClusterDiscovery: Send + Sync + 'static {
    fn discover(&self) -> Vec<String>;
}
```

This has two problems:
1. Forces cloud discovery crates to use `block_on` workarounds for async APIs
2. Errors are silently swallowed — callers can't distinguish "no peers" from "API failure"

**New trait design:**
```rust
#[async_trait]
pub trait ClusterDiscovery: Send + Sync + 'static {
    async fn discover(&self) -> Result<Vec<String>, DiscoveryError>;
}
```

The dactor core (adapter runtimes) should log and ignore discovery errors
gracefully — a failed discovery attempt should not crash the node, just
retry on the next cycle.

**Changes:**
- Add `DiscoveryError` type to dactor core
- Update `ClusterDiscovery` trait: `async fn discover() -> Result<Vec<String>, DiscoveryError>`
- Update `StaticSeeds` implementation (infallible → always Ok)
- Update all adapter runtimes to handle `Result` (log errors, retry)
- Update `dactor-discover-k8s`: remove `block_on` workaround, return proper errors
- Update `dactor-discover-aws`: remove `block_on` workaround, return proper errors
- Update usage guide and docs

**Status:** ✅ Done (PR #137 — async trait with Result)

---

#### PUB5: Rolling Upgrade & Version Compatibility Design

**Goal:** Design and document the complete version compatibility story for dactor clusters during rolling upgrades.

**Two fundamental upgrade categories:**

##### Category 1: Infrastructure-level change (dactor framework / adapter version change)

When the dactor framework itself or the underlying actor library (ractor, kameo, coerce) changes its wire protocol, **node-to-node communication breaks completely** — including system actor communication (SpawnManager, WatchManager, etc.). Nodes running different dactor wire protocol versions **cannot form a single cluster**.

**Upgrade strategy:** Cluster split
- Old cluster and new cluster run **side by side** as separate clusters
- Traffic is gradually shifted from old cluster to new cluster
- Old cluster shrinks (nodes drained), new cluster grows (nodes added)
- No mixed-version cluster — nodes never attempt to communicate across protocol versions
- Similar to blue/green deployment at the cluster level

**Detection:** The dactor framework should include a **wire protocol version** in the initial handshake when two nodes connect. If versions are incompatible, the connection is rejected immediately with a clear error (not silently dropped).

##### Category 2: Application-level change (dactor version unchanged)

When only the application code changes but dactor and adapter versions remain the same, **node-to-node communication at the system actor level works fine**. Nodes can join the same cluster, system actors communicate normally (spawn, watch, cancel, peer management all work).

However, **remote actor calls may fail** depending on whether the application message schemas changed:
- **Backward-compatible change** (added optional fields, new message types): works — `MessageVersionHandler` can migrate, serde defaults handle missing fields
- **Breaking change** (removed fields, renamed types, changed semantics): remote actor calls from old→new or new→old nodes will fail with deserialization errors

**Upgrade strategy:** Rolling restart within one cluster
- Nodes upgraded one at a time
- Mixed-version cluster is expected and supported
- `MessageVersionHandler` handles schema migration between versions
- Incompatible messages are rejected with clear errors (not silent corruption)

**Detection:** The dactor framework should:
1. Include a **dactor wire protocol version** in the connect handshake → determines Category 1 vs 2
2. Include an **application version** (user-configured) in the handshake → enables logging and routing decisions
3. Include **message version** in WireEnvelope (already exists: `version` field) → enables per-message migration via `MessageVersionHandler`

**Scenarios to design for:**
- **Category 1 — Blue/Green cluster split**: complete traffic cutover between clusters
- **Category 2 — Rolling restart**: mixed-version single cluster with message migration
- **Category 2 — Canary deployment**: small % of traffic to new app version within same cluster
- **Rollback**: revert to old version (Category 1: switch back to old cluster; Category 2: roll back nodes)

**Design deliverables:**
- Wire protocol version in connect handshake (reject incompatible immediately)
- Application version in connect handshake (log, expose via ClusterState)
- Detection logic: auto-determine Category 1 vs 2 on connection attempt
- `MessageVersionHandler` upgrade playbook with examples for Category 2
- Framework version compatibility guarantees (semver policy for wire format)
- Cluster split orchestration guide for Category 1
- Error handling: clear error messages distinguishing protocol vs application incompatibility
- ClusterEvent: `NodeRejected { reason: IncompatibleProtocol | IncompatibleVersion }`
- Testing strategy: how to test mixed-version clusters (both categories)

**Output:** Design document `docs/version-compatibility.md`

**Status:** 🔲 Not started

### Documentation Created This Session
- `docs/cluster-behavior.md` — K8s/EKS/VMSS autoscale, simultaneous restart, graceful shutdown, split-brain
- `dactor-ractor/docs/implementation.md` — ractor adapter architecture + limitations
- `dactor-kameo/docs/implementation.md` — kameo adapter architecture + limitations
- `dactor-coerce/docs/implementation.md` — coerce adapter architecture + parity gaps
- `dactor-mock/docs/implementation.md` — mock cluster testing patterns

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
| 2026-03-31 | PR 13 | ExpandHandler, StreamSender, BoxStream, expand() on TypedActorRef |
| 2026-03-31 | PR 14 | FeedMessage, ReduceHandler, StreamReceiver, reduce() with drain task |
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
| T1 | Ractor 2-node: spawn + tell/ask cross-check | ractor | ✅ PR #113 |
| T2 | Ractor 3-node: crash node + watch notification | ractor | ✅ PR #113 |
| T3 | Ractor: partition + heal + verify recovery | ractor | ✅ PR #113 |
| T4 | Kameo 2-node: spawn + tell/ask | kameo | ✅ PR #114 |
| T5 | Kameo 3-node: crash + restart | kameo | ✅ PR #114 |
| T6 | Coerce 2-node: spawn + tell/ask | coerce | ✅ PR #114 |
| T7 | Happy path: 100 messages in order across nodes | all | ✅ PR #115 |
| T8 | Corner case: send to stopped actor | all | ✅ PR #115 |
| T9 | Corner case: concurrent asks from multiple callers | all | ✅ PR #115 |
| T10 | Corner case: stream with slow consumer | all | ✅ PR #115 |
| T11 | E2E: remote ask with version breaking change — sender v1, receiver v2, verify MessageVersionHandler migration or rejection | all | ✅ PR #116 |

### 3.4 Stream/Feed Batching (PR #42)

| # | Item | Status |
|---|------|--------|
| B1 | BatchConfig (max_items + max_delay + max_bytes) | ✅ Done |
| B2 | BatchWriter (generic push + Vec\<u8\>::push_bytes) | ✅ Done |
| B3 | BatchReader (unbatching) | ✅ Done |
| B4 | Merged expand()/expand_batched() → Option\<BatchConfig\> | ✅ Done |
| B5 | Merged reduce()/reduce_batched() → Option\<BatchConfig\> | ✅ Done |
| B6 | Removed FeedMessage trait (direct Item/Reply generics) | ✅ Done |
| B7 | Per-item on_expand_item interception with Disposition | ✅ Done |
| B8 | DropObserver trait for observing interceptor drops | ✅ Done |
| B9 | runtime_support module (shared OutboundPipeline helpers) | ✅ Done |
| B10 | ContentLength built-in header | ✅ Done |
| B11 | Example: batch_streaming | ✅ Done |
| B12 | Tests (count, bytes, deadline, oversized, pre-flush) | ✅ Done |

### Recommended Execution Order (Phase 3)

1. ~~**F1 + B1-B12**: Stream batching~~ ✅ Complete (PR #42)
2. ~~**E7-E11**: Sample code for remaining features~~ ✅ Complete (PR #43)
3. **T1-T3**: Ractor E2E tests (validates real multi-process) — ✅ Complete (PR #113)
4. **T4-T6**: Kameo/Coerce E2E tests — ✅ Complete (PR #114)
5. ~~**F3**: EventSourced/DurableState actor integration~~ ✅ Complete (PR #44)
6. ~~**F2**: Actor Pool~~ ✅ Complete (PR #45, local)
7. **T7-T10**: Cross-adapter corner case tests — ✅ Complete (PR #115)
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
| SA1 | dactor-ractor | SpawnManager wiring | RactorRuntime starts SpawnManager, routes incoming SpawnRequest via ractor's remote channel, calls create_actor + local spawn | ✅ PR #80 |
| SA2 | dactor-ractor | WatchManager wiring | RactorRuntime starts WatchManager, translates ractor's native watch into WatchManager entries, sends WatchNotification on termination | ✅ PR #80 |
| SA3 | dactor-ractor | CancelManager wiring | RactorRuntime registers CancellationTokens with CancelManager, handles incoming CancelRequest from remote nodes | ✅ PR #80 |
| SA4 | dactor-ractor | NodeDirectory wiring | RactorRuntime populates NodeDirectory from ractor_cluster membership, updates PeerStatus on connect/disconnect | ✅ PR #80 |
| SA5 | dactor-kameo | SpawnManager wiring | KameoRuntime starts SpawnManager, routes SpawnRequest via kameo's distributed actor layer | ✅ PR #81 |
| SA6 | dactor-kameo | WatchManager wiring | KameoRuntime wires kameo's actor lifecycle into WatchManager for remote watch delivery | ✅ PR #81 |
| SA7 | dactor-kameo | CancelManager wiring | KameoRuntime registers tokens with CancelManager, handles remote cancel | ✅ PR #81 |
| SA8 | dactor-kameo | NodeDirectory wiring | KameoRuntime populates NodeDirectory from kameo/libp2p peer discovery | ✅ PR #81 |
| SA9 | dactor-mock | System actor wiring | MockCluster wires SpawnManager + WatchManager + CancelManager + NodeDirectory, auto-connects peers | ✅ PR #78 |
| SA10 | dactor-coerce | System actor wiring | CoerceRuntime wires system actors (stub — depends on coerce sharding integration) | ✅ PR #82 |

### 4.3 Serialization & Schema

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| SE1 | MessageSerializer integration | §9.1 | Wire MessageSerializer into transport layer | ✅ PR #63 (JsonSerializer) |
| SE2 | TypeRegistry | §8.3, §9.2 | Map type names → deserializers for remote dispatch | ✅ PR #63 |
| SE3 | HeaderRegistry | §5.1 | Deserializer registry for remote headers | ✅ PR #63 |
| SE4 | Message versioning | §9.1 | MessageVersionHandler for schema evolution/migration | ✅ PR #63 (receive_envelope_body_versioned) |
| SE5 | ActorRef serialization | §9.3 | Serialize/deserialize ActorRef for cross-node passing | ✅ PR #72 (ActorRefEnvelope) |
| SE6 | Protobuf system serialization | §9.1 | Replace JSON with protobuf for all system-level serialization (SpawnRequest/Response, WatchRequest/Notification, CancelRequest/Response, WireEnvelope framing). Protobuf provides smaller payloads, faster ser/deser, and native schema evolution via field numbering. Application messages remain pluggable via MessageSerializer. | ✅ PR #112 |

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
| AP6 | LeastLoaded routing | §4.14 | Route to worker with fewest queued messages | ✅ PR #100 |
| AP7 | Distributed pool | §4.14 | Workers across nodes via remote ActorRef (Phase 4) | ✅ PR #120 (WorkerRef) |
| AP8 | Virtual actor pool (v2) | §4.14 | Redesign pool as virtual router actor (see below) | ✅ PR #121 (VirtualPoolRef) |

#### AP8: Virtual Actor Pool (Implemented — PR #121)

`PoolRef` is a passive data structure (Vec of refs + atomic counter)
that routes from the caller's thread. `VirtualPoolRef` addresses its limitations:

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
| AM5 | Outbound priority queue | §5.8 | Per-destination priority lanes | ✅ PR #74 |
| AM6 | Pluggable outbound comparer | §5.6, §5.8 | WireEnvelopeComparer trait + StrictPriorityWireComparer + AgingWireComparer, integrated into OutboundPriorityQueue via with_comparer() | ✅ PR #76 |
| AM7 | Stream item ordering guarantee | §4.11 | Stream/feed items bypass priority queue, always FIFO. Only tell/ask subject to priority. | ✅ PR #77 |

---

## Phase 7: Observability & Tooling

### 7.1 Metrics & Monitoring

| # | Feature | Design Section | Description | Status |
|---|---------|----------------|-------------|--------|
| O1 | MetricsInterceptor wiring | §11.2 | Wire built-in metrics into runtimes | ✅ PR #55 (per-actor ActorMetricsHandle, windowed buckets) |
| O2 | MetricsStore query API | §11.3 | Query per-actor and per-message-type metrics | ✅ PR #55 (MetricsRegistry with register/snapshot) |
| O3 | RuntimeMetrics | §11.6 | System-level: actor count, mailbox depth, uptime | ✅ PR #55 (message_rate, error_rate, windowed) |
| O4 | OtelInterceptor | §11.4 | ❌ Not planned — tracing should be added by application code, not framework (perf risk) |
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
| NR2 | ClusterEvent enum | §10.1, §10.4 | NodeJoined, NodeLeft push events | ✅ PR #86 (ractor, kameo; coerce deferred — stub) |
| NR3 | Cluster event handlers | §10.4 | Actors subscribe to membership changes | ✅ PR #86 (ractor, kameo; coerce deferred — stub) |
| NR4 | Processing groups | §2.2 | Actor group pub/sub (ractor pg, coerce sharding) | ✅ PR #103 |

---

## Phase 9: Native System Actors

### Background

SA1-SA10 wired the four system actors (SpawnManager, WatchManager, CancelManager,
NodeDirectory) into all adapter runtimes as **plain Rust structs** with direct
method calls. This was the right first step — it locked down the API surface and
enabled unit testing.

The design spec (§8.2) calls for system actors to be **real provider-native
actors** with mailboxes and message-based communication. This is necessary for:

1. **Transport integration** — remote nodes send `CancelRequest` etc. as wire
   messages; they need to land in a mailbox, not a direct method call.
2. **Concurrency safety** — native actors process messages single-threaded via
   their mailbox, eliminating `&mut self` contention.
3. **Backpressure** — mailbox depth limits prevent system actor overload.
4. **Supervision** — system actors can be supervised and restarted on failure.

### Plan

Each system actor becomes a native provider actor (ractor `Actor`, kameo `Actor`,
etc.) that wraps the existing struct and handles the corresponding request
messages.

| # | Feature | Description | Status |
|---|---------|-------------|--------|
| NA1 | Ractor native SpawnManager | `ractor::Actor` impl wrapping `SpawnManager`, handles `SpawnRequest` messages via mailbox | ✅ PR #83 |
| NA2 | Ractor native WatchManager | `ractor::Actor` impl wrapping `WatchManager`, handles `WatchRequest`/`UnwatchRequest` messages | ✅ PR #83 |
| NA3 | Ractor native CancelManager | `ractor::Actor` impl wrapping `CancelManager`, handles `CancelRequest` messages | ✅ PR #83 |
| NA4 | Ractor native NodeDirectory | `ractor::Actor` impl wrapping `NodeDirectory`, handles membership change messages | ✅ PR #83 |
| NA5 | Kameo native SpawnManager | `kameo::Actor` impl wrapping `SpawnManager` | ✅ PR #84 |
| NA6 | Kameo native WatchManager | `kameo::Actor` impl wrapping `WatchManager` | ✅ PR #84 |
| NA7 | Kameo native CancelManager | `kameo::Actor` impl wrapping `CancelManager` | ✅ PR #84 |
| NA8 | Kameo native NodeDirectory | `kameo::Actor` impl wrapping `NodeDirectory` | ✅ PR #84 |
| NA9 | Runtime auto-start | Each adapter runtime spawns its system actors during `new()` and holds refs | ✅ PR #85 |
| NA10 | Transport routing | Incoming `WireEnvelope` with system message types routed to the correct system actor mailbox | ✅ PR #111 |

### Design Notes

- **Struct stays as-is.** The existing `SpawnManager`, `WatchManager`, etc.
  become the **state** inside the native actor. The methods become handler
  bodies. No rewrite needed — just wrapping.
- **One actor per system role.** Each system actor is spawned independently
  (not a single "system actor" muxing all roles). This keeps handlers simple
  and enables per-role supervision.
- **Request/Response pattern.** System actor messages use ask semantics
  (e.g., `SpawnRequest` → `SpawnResponse`). The transport layer awaits the
  reply and sends it back over the wire.
- **Coerce/mock.** Coerce is a stub — native actors can be added when
  coerce crate integrates. MockCluster can optionally wrap in test actors or
  continue using direct method calls for simplicity.
- **Depends on:** SA1-SA10 (complete), transport integration (R4).
- **Priority:** High — required before remote operations work end-to-end.

---

## Phase 10: Actor Lifecycle Handles (JoinHandle / Await-Stop)

### Background

Each actor backend handles task lifecycle differently:

| Backend | Spawn Return | Await Completion | Stop Signal |
|---------|-------------|-----------------|-------------|
| **ractor** | `(ActorRef, JoinHandle)` | `.await` on JoinHandle | `actor_ref.stop()` |
| **kameo** | `ActorRef` only | Not exposed — lifecycle via hooks | Drop all refs or `stop()` |
| **coerce** | `ActorRef` only | Not exposed — tracked actors use `system.stop_actor()` | Drop refs (anonymous) or system API (tracked) |
| **dactor (current)** | `AdapterActorRef` | Not supported — JoinHandle discarded | `actor_ref.stop()` |

Ractor is the only backend that returns a `JoinHandle` from spawn. Kameo and
coerce manage actor lifecycle internally — there is no task-level handle to
await. Dactor currently discards ractor's JoinHandle (`_join` in
`spawn_internal`).

### Plan

| # | Feature | Description | Status |
|---|---------|-------------|--------|
| JH1 | Store ractor JoinHandle | `RactorRuntime` stores JoinHandle in an internal map (`ActorId → JoinHandle`), cleaned up on actor stop | ✅ PR #87 |
| JH2 | `runtime.await_stop(id)` | Backend-agnostic API: returns a future that resolves when the actor finishes. Ractor: awaits stored JoinHandle. Kameo/coerce: uses a oneshot channel wired into `on_stop()` | ✅ PR #87 |
| JH3 | `runtime.await_all()` | Await all spawned actors — useful for graceful shutdown | ✅ PR #87 |
| JH4 | Propagate panics | If an actor panics in `post_stop`, `await_stop()` should return the error instead of silently swallowing it | ✅ PR #92 |
| JH5 | TestRuntime support | Wire `await_stop` into TestRuntime for test teardown verification | ✅ PR #92 |

### Design Notes

- **Backend-agnostic abstraction is essential.** Exposing ractor's JoinHandle
  directly would leak the backend. The `await_stop(id)` API works for all
  backends by using different internal mechanisms.
- **Kameo/coerce workaround:** Since these backends don't return a JoinHandle,
  `await_stop` would wire a `tokio::sync::oneshot` into the actor's `on_stop()`
  hook. The adapter's `post_stop()` / lifecycle callback sends on the channel.
- **Priority:** Low — most users don't need to await actor completion. The
  primary use case is graceful shutdown in production and test teardown.
- **Depends on:** SA1-SA8 (system actor wiring) for lifecycle integration.

---

## Phase 11: Async Spawn API

### Background

All dactor adapter spawn methods are currently synchronous:

```rust
// ractor adapter — sync, but ractor::Actor::spawn() is async
pub fn spawn<A>(&self, name: &str, args: A::Args) -> RactorActorRef<A>

// kameo adapter — sync (kameo's Spawn::spawn_with_mailbox is also sync)
pub fn spawn<A>(&self, name: &str, args: A::Args) -> KameoActorRef<A>

// TestRuntime — sync
pub fn spawn<A>(&self, name: &str, args: A::Args) -> TestActorRef<A>
```

Each backend's native spawn has a different signature:

| Backend | Native Spawn | Async? | Notes |
|---------|-------------|--------|-------|
| **ractor** | `ractor::Actor::spawn()` | **async** | Returns `Result<(ActorRef, JoinHandle)>`. Dactor currently bridges via `std::thread::spawn` + `Handle::block_on` to keep the API sync. This is heavyweight and can panic if no tokio runtime is active. |
| **kameo** | `Spawn::spawn_with_mailbox()` | **sync** | Calls `tokio::spawn` internally, returns `ActorRef` immediately. No bridging needed. |
| **coerce** | `actor.into_actor(name, &system)` | **async** | Returns `Result<ActorRef>`. Would also need sync→async bridging. |
| **TestRuntime** | In-process task spawn | **sync** | No runtime needed, purely local. |

The sync API was chosen for simplicity but has drawbacks:

1. **Ractor sync bridge is fragile** — spawns a std::thread + `block_on` just
   to call an async function. This adds latency (~100μs per spawn), can panic
   without a tokio runtime, and is unusual Rust async code.
2. **Coerce can't be properly wired** — `into_actor()` is async, so a coerce
   adapter would also need the same heavyweight bridging.
3. **Error swallowing** — sync spawn panics on failure instead of returning
   `Result`, because there's no natural way to propagate async errors in a
   sync context.

### Plan

| # | Feature | Description | Status |
|---|---------|-------------|--------|
| AS1 | Async spawn for ractor | `pub async fn spawn<A>(...) -> Result<RactorActorRef<A>, RuntimeError>` — call `ractor::Actor::spawn().await` directly, remove std::thread bridge | ✅ PR #93 |
| AS2 | Async spawn for kameo | `pub async fn spawn<A>(...) -> Result<KameoActorRef<A>, RuntimeError>` — kameo is sync internally but async API is forward-compatible and consistent | ✅ PR #93 |
| AS3 | Async spawn for coerce | `pub async fn spawn<A>(...) -> Result<CoerceActorRef<A>, RuntimeError>` — enables direct `into_actor().await` | ✅ PR #93 |
| AS4 | Async spawn for TestRuntime | `pub async fn spawn<A>(...) -> Result<TestActorRef<A>, RuntimeError>` — consistent API, trivially wraps sync | ✅ PR #93 |
| AS5 | Return Result, not panic | All spawn methods return `Result<Ref, RuntimeError>` instead of panicking on failure | ✅ PR #93 |
| AS6 | Migration: keep sync wrappers | Provide `spawn_blocking()` sync wrappers that call `Handle::block_on(spawn())` for callers that need sync API. Deprecate the current sync `spawn()` over time | N/A — pre-release, no backward compat needed |
| AS7 | Update examples & tests | All examples, conformance tests, and adapter tests updated to use `.await` | ✅ PR #93 |

### Design Notes

- **Breaking change.** Moving spawn from sync to async changes every call site.
  Mitigation: keep `spawn_blocking()` sync wrappers during transition.
- **Kameo doesn't need async spawn** but should have it for API consistency.
  The async version would simply wrap the sync call.
- **TestRuntime** is sync by nature but async spawn is trivially
  `async { Ok(self.spawn_sync(name, args)) }`.
- **Error handling improves.** Async spawn can return `Result` naturally
  instead of panicking. This is especially important for remote spawn where
  network errors are expected.
- **Priority:** Medium — the sync bridge works but is a known wart. Worth
  doing before 1.0 but not blocking current development.
- **Depends on:** JH1 (storing JoinHandle) pairs naturally with async spawn.

---

## Phase 12: Coerce Adapter Parity

### Background

The dactor-coerce adapter is currently a **stub** wrapping `TestRuntime` as a
placeholder (PR #30). While it passes conformance tests and has SA10 system
actor wiring, it significantly lags behind the ractor and kameo adapters.

This phase brings coerce to feature parity with ractor/kameo.

### Feature Gap Analysis

| Feature | Ractor | Kameo | Coerce | Gap |
|---------|--------|-------|--------|-----|
| Real provider runtime | ✅ ractor::Actor | ✅ kameo::Actor | ❌ Wraps TestRuntime | Need coerce integration |
| Native system actors (NA) | ✅ PR #83 | ✅ PR #84 | ❌ | Need coerce native actors |
| Runtime auto-start (NA9) | ✅ PR #85 | ✅ PR #85 | ❌ | Need start_system_actors() |
| ClusterEvents impl | ✅ RactorClusterEvents | ✅ KameoClusterEvents | ❌ | Need CoerceClusterEvents |
| ClusterEvent emission (NR2) | ✅ PR #86 | ✅ PR #86 | ❌ | Need connect/disconnect wiring |
| Lifecycle handles (JH) | ✅ PR #87 | ✅ PR #87 | ❌ | Need await_stop/await_all |
| Outbound interceptors | ✅ Full pipeline | ✅ Full pipeline | ⚠️ Delegated to TestRuntime | |
| Watch/unwatch | ✅ Native | ✅ Native | ⚠️ Delegated to TestRuntime | |

### Plan

| # | Feature | Description | Priority | Status |
|---|---------|-------------|----------|--------|
| CP1 | Integrate real coerce | Replace TestRuntime with real `coerce::actor::Actor` spawning via `start_actor()`. Type-erased `DactorMsg` dispatch through coerce's mailbox. | High | ✅ Done |
| CP2 | CoerceClusterEvents | Implement `ClusterEvents` trait for coerce adapter (callback-based, same pattern as ractor/kameo) | High | ✅ Done |
| CP3 | ClusterEvent emission | Wire `connect_peer()`/`disconnect_peer()` to emit NodeJoined/NodeLeft via CoerceClusterEvents | High | ✅ Done |
| CP4 | Lifecycle handles | Implement `await_stop()`/`await_all()`/`cleanup_finished()` — wire oneshot into coerce actor stop lifecycle | High | ✅ Done |
| CP5 | Native system actors | Implement coerce native actors for SpawnManager, WatchManager, CancelManager, NodeDirectory | Medium | ✅ PR #90 |
| CP6 | Runtime auto-start | `start_system_actors()` spawns native coerce system actors | Medium | ✅ PR #90 |
| CP7 | Interceptor pipeline | Ensure inbound/outbound interceptor pipelines work with real coerce actors (not just TestRuntime delegation) | Medium | ✅ Done |
| CP8 | Watch/unwatch | Wire coerce's actor lifecycle into WatchManager for real DeathWatch notifications | Medium | ✅ Done |
| CP9 | Mailbox config | Wire coerce's mailbox options (bounded/unbounded) instead of ignoring MailboxConfig | Low | ✅ PR #102 |
| CP10 | Comprehensive tests | Adapter tests, system actor tests, lifecycle tests, conformance suite — all on real coerce runtime | High | ✅ Done |

### Design Notes

- **CP1 is complete** — TestRuntime replaced with real coerce actors using
  `CoerceDactorActor<A>` wrapper, type-erased `DactorMsg<A>` dispatch, and
  coerce's `start_actor()` for synchronous spawning.
- **CP2-CP4, CP7-CP8, CP10 also complete** — ClusterEvents, lifecycle handles,
  interceptor pipelines, watch/unwatch, and comprehensive tests all work on
  real coerce actors. All 41 tests pass (16 conformance + 24 system actor + 1 doc).
- **CP5-CP6 complete** — native coerce system actors with runtime auto-start.
- **CP9 complete** (PR #102) — bounded mailbox via front-buffer `mpsc` channel
  in front of coerce's unbounded internal mailbox.
- **Limitation:** dactor actors used with the coerce adapter must be `Send + Sync`
  (required by `coerce::actor::Actor`). This is satisfied by most actors.

---

## Phase 13: Transform Pattern & Streaming API Naming

### Background: Current Streaming Call Patterns

dactor currently supports three streaming call patterns:

| Pattern | Method | Direction | Description |
|---------|--------|-----------|-------------|
| **expand** | `actor_ref.expand(msg)` | Actor → Caller | Actor produces a stream of items to the caller |
| **reduce** | `actor_ref.reduce(input)` | Caller → Actor | Caller sends a stream of items to the actor, gets a single reply |
| **ask** | `actor_ref.ask(msg)` | Caller ↔ Actor | Single request, single reply |

Missing: **transform** — caller sends a stream of items, actor produces a
stream of transformed items (bidirectional streaming).

Currently listed under "Not Planned" as "Bidirectional streaming — composed
from reduce() + expand() at app level". However, the compose-it-yourself
approach has significant drawbacks:

1. **Two separate channels** — reduce() and expand() create independent
   mailbox entries, so ordering between input consumption and output
   production is not guaranteed.
2. **No backpressure coupling** — if the output stream is slow to consume,
   the input feed has no way to slow down.
3. **Lifecycle complexity** — the caller must coordinate two independent
   async flows (the feed drain task and the stream consumer).

A first-class `transform()` primitive solves all three issues.

### Proposed API

```rust
// Transform: stream-in, stream-out
let output: BoxStream<OutputItem> = actor_ref.transform(
    input,          // BoxStream<InputItem>
    buffer,         // output buffer size
    batch_config,   // Optional<BatchConfig>
    cancel,         // Optional<CancellationToken>
)?;

// The actor implements TransformHandler:
#[async_trait]
trait TransformHandler<Item: Send + 'static, Output: Send + 'static>: Actor {
    async fn on_item(
        &mut self,
        item: Item,
        sender: &StreamSender<Output>,
        ctx: &mut ActorContext,
    );

    async fn on_complete(
        &mut self,
        sender: &StreamSender<Output>,
        ctx: &mut ActorContext,
    ) {}
}
```

### Plan

| # | Feature | Description | Status |
|---|---------|-------------|--------|
| TF1 | TransformHandler trait | `on_item(item, sender, ctx)` + `on_complete(sender, ctx)` | ✅ PR #94 |
| TF2 | `actor_ref.transform()` | Wires input stream → actor → output stream with shared lifecycle | ✅ PR #94 |
| TF3 | Backpressure coupling | Output stream backpressure slows input consumption | ✅ PR #94 |
| TF4 | Batch support | `Option<BatchConfig>` for both input and output | ✅ PR #101 |
| TF5 | Interceptor integration | Outbound pipeline on each output item | ✅ PR #94 |
| TF6 | Cancellation | Single CancellationToken cancels both input and output | ✅ PR #94 |
| TF7 | Adapter wiring | Wire into ractor, kameo, coerce, TestRuntime | ✅ PR #94 |

### Streaming API Naming Review

The current naming (`stream`, `feed`, `transform`) is **verb-oriented** and
describes the communication direction. However, from a **stream processing**
perspective, these names may be confusing:

| Current Name | Stream Semantics | Proposed Name |
|-------------|------------------|--------------|
| `tell(msg)` → no reply | **Fire-and-forget** | `tell` (no change) |
| `ask(msg)` → single reply | **Request-Reply** (1→1) | `ask` (no change) |
| `expand(msg)` → stream of items | **Expand** (1→N) — one request expands into many items | `expand` |
| `reduce(input)` → single reply | **Reduce** (N→1) — consume stream, produce aggregate | `reduce` |
| `transform(input)` → stream of items | **Transform** (N→M) — consume stream, produce stream | `transform` |

**Proposed full naming scheme (stream-processing aligned):**

```
tell(msg)              → tell(msg)              // 1→0  fire-and-forget
ask(msg)               → ask(msg)               // 1→1  request-reply
expand(msg)            → expand(msg)            // 1→N  one request, many results
reduce(input)            → reduce(input)          // N→1  many inputs, one result
transform(input)       → transform(input)       // N→M  many inputs, many results
```

This naming scheme maps naturally to **cardinality**:

```
             Input
             1          N
Reply  ┌──────────┬──────────┐
  0    │ tell     │          │
  1    │ ask      │ reduce   │
  N    │ expand   │transform │
       └──────────┴──────────┘

  Broadcast (1→many actors):
  0×A  │ broadcast (fire-and-forget to all)
  1×A  │ broadcast_ask (collect receipts from all)
```

**Decision needed:** Renaming `stream` → `expand` and `feed` → `reduce` are
API changes.

Since **dactor is not yet released**, there are no external consumers to
break. This is the ideal time for naming changes.

**Recommendation:** Option 1 (rename now) — do a clean rename before the
first release. No aliases or deprecation needed since there are no
downstream users. Update all traits, impls, tests, examples, and docs
in a single PR.

---

## Phase 14: Broadcast

### Background

All current call patterns are **point-to-point** — one caller sends to one
actor. Broadcast sends the same message to **multiple actors** simultaneously.

Two variants:

1. **Fire-and-forget broadcast** — send a message to all actors in a group,
   no replies expected. Useful for cache invalidation, config reload, etc.

2. **Broadcast with receipts** — send a message to all actors and collect
   their replies, with a timeout for stragglers. Useful for distributed
   queries, health checks, consensus.

### Proposed API

```rust
// Fire-and-forget broadcast to all actors in a group
runtime.broadcast::<A, M>(group, msg)?;
// or via a BroadcastRef:
let group: BroadcastRef<A> = runtime.broadcast_group(actor_refs);
group.tell(msg)?;

// Broadcast with receipts (timeout for replies)
let receipts: Vec<BroadcastReceipt<M::Reply>> = group
    .ask(msg, timeout)
    .await;

// Each receipt indicates success, timeout, or error per actor
pub enum BroadcastReceipt<R> {
    /// Actor replied successfully within the timeout.
    Ok { actor_id: ActorId, reply: R },
    /// Actor did not reply within the timeout.
    Timeout { actor_id: ActorId },
    /// A send or transport-level failure prevented delivery to the actor.
    SendError { actor_id: ActorId, error: ActorSendError },
    /// The actor processed the message but the reply resolved to an error.
    ReplyError { actor_id: ActorId, error: RuntimeError },
}
```

### Plan

| # | Feature | Description | Status |
|---|---------|-------------|--------|
| BC1 | BroadcastRef<A> | A reference to a group of actors of the same type. Holds `Vec<R>` of actor references with `PhantomData<A>`. | ✅ PR #96 |
| BC2 | `broadcast.tell(msg)` | Fire-and-forget: clone and send `msg` to all actors. Requires `M: Clone`. Returns `BroadcastTellResult` with per-actor outcomes. | ✅ PR #96 |
| BC3 | `broadcast.ask(msg, timeout)` | Request-reply: clone and send `msg` to all actors, collect `BroadcastReceipt<R>` within timeout. Uses `tokio::time::timeout` per actor. | ✅ PR #96 |
| BC4 | BroadcastReceipt<R> | Per-actor result enum: Ok, Timeout, SendError, ReplyError. | ✅ PR #96,#98 |
| BC5 | Dynamic group membership | `group.add(actor_ref)`, `group.remove(actor_id)`. Owned (non-shared) `&mut self` API. Thread-safe shared membership (e.g. `Arc<RwLock>`) is a separate feature. | ✅ PR #96 |
| BC6 | Integration with actor pools | `PoolRef` can expose a `BroadcastRef` for sending to all workers. | ✅ PR #99 |
| BC7 | Interceptor support | Outbound interceptors run once per target actor (not once for the whole broadcast). | ✅ PR #99 |
| BC8 | Dead letter routing | Actors that are stopped get their messages routed to the dead letter handler. | ✅ PR #99 |
| BC9 | Tests | Broadcast tell/ask, partial failure, timeout, empty group, dynamic membership. | ✅ PR #96-#99 |

### Design Notes

- **`M: Clone` required** — the message must be cloneable since it's sent to
  multiple actors. This is a new constraint not present in tell/ask.
- **Concurrency** — all asks are dispatched concurrently via `join_all`,
  not sequentially. The timeout applies per-actor, not globally.
- **Backend-agnostic** — `BroadcastRef` works at the `ActorRef<A>` trait
  level, so it works with any adapter (ractor, kameo, coerce, mock).
- **Not a new actor** — `BroadcastRef` is a client-side utility, not a
  router actor. For router-actor semantics, see `PoolRef` (AP1-AP8).
- **Relation to processing groups (NR4)** — processing groups provide
  cluster-wide membership discovery; `BroadcastRef` provides the send
  primitive. They compose: `runtime.group("workers")` returns actors,
  `BroadcastRef::new(actors)` enables broadcast.
- **Priority:** Medium — useful for pub/sub patterns but not blocking
  other work.

---

## Not Planned / Out of Scope

| Feature | Reason |
|---------|--------|
| Hot code upgrade | Excluded from design (Erlang-only, §2.2) |
| Passivation | Not in design spec |
| Security (TLS, auth) | Deferred to interceptors + application code |
| CLI tools / dashboard | Not in design spec |
