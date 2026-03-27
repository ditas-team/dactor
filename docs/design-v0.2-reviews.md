# dactor v0.2 Design Document — Review Comments

> Reviews conducted on 2026-03-27 using multiple AI models.
> Gemini 3 Pro (Preview) was unavailable — reviews from Claude Haiku 4.5 and GPT-5.1.

---

## Review 1: Claude Haiku 4.5

### 1. Strengths

**1.1 Principled Architecture**
The **superset rule** with graceful degradation (§0) is excellently articulated. Rather than fragmenting adapters into incompatible variants, dactor abstracts the common capabilities of 2+ frameworks and documents where each adapter either implements, shims, or returns `NotSupported`. This is pragmatic and scales well as new frameworks are added.

**1.2 Comprehensive Capability Matrix**
The detailed capability tables (§0, §6) provide exceptional clarity on adapter support. Using three explicit strategies (Library Native, Adapter Implemented, Not Supported) prevents ambiguity. The matrix is well-researched and honestly reflects framework limitations (e.g., ractor's lack of bounded mailboxes, coerce's unbounded design).

**1.3 Thoughtful Error Model**
The `ActorError` design (§3.14) is sophisticated and production-ready:
- **Serializable structure** with machine-readable `ErrorCode` (inspired by gRPC, aligns with distributed system best practices)
- **Captures error chains** as strings (respects the fact that `dyn Error` can't cross the wire)
- **Structured details** allow rich context without requiring a custom error type per failure mode
- The three-layer error taxonomy (business error inside `Ok`, runtime error, infrastructure error) is clear and gives callers actionable granularity

**1.4 Message Envelope & Interceptors**
The `Envelope<M>` + `Headers` + `Interceptor` pipeline (§3.1, §3.2) is elegant:
- **Type-keyed headers** (using `TypeId`) eliminate string-based lookups and reduce downcasting
- **Opaque `HeaderValue` trait** allows external crates (like `dcontext`) to plug in tracing/correlation IDs without dactor knowing about them
- **Interceptor traits** with `on_receive`, `on_complete`, `on_stream_item` give three distinct observation points, enabling metrics, logging, and tracing without handler modification

**1.5 Streaming Abstraction**
The `StreamRef<M, R>` design (§3.5) is well-motivated:
- Grounded in real use cases (pagination, subscriptions, progressive results)
- Uses Rust's standard `Stream` trait from `futures-core`, avoiding custom abstractions
- **Backpressure** via bounded `mpsc` channels prevents unbounded memory growth
- **Cancellation semantics** are clean — dropping the stream signals the actor to stop
- The adapter pattern (create channel, wrap receiver, pass sender to handler) is straightforward to implement

**1.6 Mock Cluster Crate (`dactor-mock`)**
This is a standout feature (§3.13):
- **Multi-node in one process** without requiring external infrastructure
- **Forced serialization** catches wire-only bugs in unit tests
- **Rich fault injection API** (partition, latency, jitter, drop, corruption, duplication, reordering, node crashes) enables deterministic chaos testing
- **Per-node `TestClock`** + coordinated `advance_time()` + clock skew provide deterministic time control
- **Inspection API** (`in_flight_count()`, `dropped_count()`, `flush()`) enables assertions on system behavior

**1.7 Actor-Typed References with Compile-Time Reply Safety**
The decision to adopt the Kameo/Coerce pattern (§10.6) is well-justified:
- **`ActorRef<A>` typed to the actor, not the message** — compile-time reply type inference
- **Each message gets its own handler impl** — cleaner than a monolithic pattern-match
- **Messages are reusable** across actors
- **Aligns with 2 of 3 backend libraries** (kameo, coerce)

**1.8 Macro Support Path**
The proc-macro design (§10.7) reduces boilerplate while generating the exact same traits underneath.

**1.9 Lifecycle Hooks on Actor Trait**
Placing lifecycle hooks directly on the `Actor` trait is clean. Default no-op implementations let simple actors ignore them.

**1.10 Dependency Cleanup**
Making `serde` optional (§7) is correct — the core crate shouldn't force a serialization dependency on users who only do local actor communication.

### 2. Concerns

**2.1 Trait Explosion Risk in Adapters**
The design introduces multiple traits: `ActorRef<M>`, `AskRef<M, R>`, `StreamRef<M, R>`, `Interceptor`, `SupervisionStrategy`. Each adapter must implement custom logic for several of these. Risk of subtle incompleteness in one trait's implementation across adapters.

> **Mitigation:** Add comprehensive integration tests in each adapter crate that exercise all traits together.

**2.2 Mock Cluster Codec Abstraction Feels Loose**
The `MessageCodec` trait assumes all cross-node messages use a single codec. Real distributed systems often have codec versioning or per-link codec negotiation. The design doesn't model "serialization failed" as distinct from "message corrupted."

**2.3 ActorContext Lacks Runtime Access**
The `ActorContext` provides `headers` but the design doesn't show how handlers access the runtime for spawning child actors, sending messages, etc. The `// ...` is unspecified.

**2.4 Streaming Cancellation Semantics Could Be More Explicit**
- What happens if the actor continues producing after the channel closes?
- Whether `on_complete` is called with `StreamCancelled` if the actor keeps producing
- Error handling patterns for actors that don't check `tx.send()` return value

> **Suggestion:** Consider whether `StreamSender::send()` should be `#[must_use]`.

**2.5 Interceptor `on_complete` for Streams — Behavior Unclear**
- If the actor sends 100 items and consumer drops, does `items_emitted` report 100 or the last consumed count?
- If the actor crashes mid-stream, is `on_complete` called with `HandlerError`?
- Can the interceptor observe actual items, or only the count?

**2.6 Priority Mailbox Interaction with Backpressure Unclear**
- Does a priority queue with no capacity limit ignore overflow policies?
- How does priority interact with `tell_envelope()`?
- Starvation: can low-priority messages be starved indefinitely?

**2.7 Handler Signature Limits Multi-Message Per Call**
No built-in support for batching or correlating multiple messages (unlike Erlang's `receive after`). Likely fine but worth noting.

**2.8 Error Chain Capture as Strings Loses Structure**
Local `ask()` calls that fail could preserve the full `Box<dyn Error>` chain for better debugging. Consider an `ErrorChainHint` enum for intermediate error types.

**2.9 MailboxConfig as Runtime-Time Decision**
No way to query or modify an actor's mailbox config after spawn. Should there be `get_actor_config` / `update_actor_config` methods?

**2.10 Watch/Unwatch Notifications Aren't Typed**
Does the watcher need a `ChildTerminated` message handler, or is it injected synthetically? The notification delivery mechanism needs clarification.

**2.11 Serialization Assumptions for Remote Calls**
- How do adapters enforce `Serialize + Deserialize` bounds?
- Cross-node delivery protocol not specified
- Type matching across nodes not addressed

**2.12 No Mention of Backpressure in `tell()`**
What happens with many `tell()` to a slow actor with unbounded mailbox? OOM risk.

### 3. Suggestions

1. Define the complete `ActorContext` struct (headers, self_ref, actor_id, runtime access)
2. Clarify interceptor failure semantics for streams — `Reject` should return `Err` immediately
3. Add timeout support to `ask()` and `stream()`
4. Document mailbox config interaction with overflow for priority queues
5. Extend `MessageCodec` with version parameter for testing rolling deployments
6. Add deterministic testing helpers (`assert_message_delivered`, `inspect_mailbox`)
7. Specify interceptor ordering guarantees (registration order, first rejection wins)
8. Consider `RestartWithState` variant in `ErrorAction` for partial recovery
9. Add `StreamGuard` helper type for cancellation safety
10. Add `Serializable` marker trait for cross-node messages (compile-time enforcement)
11. Extend `InterceptContext` with `remote: bool` and `origin_node: Option<NodeId>`
12. Consider making `on_error` fallible (`Result<ErrorAction, ActorError>`)

### 4. Questions

1. How does the adapter distinguish local vs remote actor references?
2. Who assigns the `SupervisionStrategy`? Is there a `SupervisorConfig`?
3. Can interceptors maintain internal state (e.g., rate-limiting counters)?
4. Are messages from the same sender delivered in order? Are handlers sequential or concurrent?
5. How long will deprecated `send()` alias be supported?
6. Should `cluster`, `supervision`, `streaming` be behind optional features?
7. How do timers work with `TestClock` in `MockCluster`?
8. Can actors from different runtimes (ractor + kameo) exchange references?
9. How is message versioning handled for remote calls?
10. Is there a dead letter queue for dropped/overflow messages?

---

## Review 2: GPT-5.1

### 1. Strengths

- Clear superset principle, with explicit capability matrices.
- Actor/Message/Handler + `ActorRef<A>` API is Rust-idiomatic and type-safe.
- Interceptor + typed Headers design is powerful and non-opinionated.
- Streaming API has good backpressure/cancellation story.
- Error model (`RuntimeError` + `ActorError`) is structured and remote-friendly.
- Mock cluster crate is ambitious but very well thought-through.
- Proc-macro story is ergonomic yet maps to explicit traits.

### 2. Concerns

- **`Actor::on_error` signature is inconsistent:** §3.6 uses `&ActorError`, §10.6 uses `Box<dyn Error>`; this conflicts with the `ActorError`-centric model.
- **Dual ask/stream abstractions** (adapter-level `AskRef`/`StreamRef` vs `ActorRef<A>::ask/stream`) risk divergence or confusion.
- **Superset/`NotSupported` is purely runtime;** no capability discovery or compile-time guarantees, so "works on my backend" surprises are likely.
- **Priority mailbox:** starvation/fairness semantics and interaction with `OverflowStrategy` are underspecified; `DropOldest` exists in the API but is intentionally unimplementable in all adapters.
- **Interceptor `Drop`/`Reject` behavior for `tell`** (silently swallowed) may hide serious failures such as auth or validation.

### 3. Suggestions

- **Normalize `on_error` signature:** use `on_error(&ActorError) -> ErrorAction` everywhere, and make adapter-captured panics always go through `ActorError`.
- **Collapse ask/stream into a single conceptual layer:** keep adapter traits internal and expose only `ActorRef<A>::ask/stream` in the public API to avoid two mental models.
- **Add capability introspection:** e.g., `ActorRuntime::capabilities()` or per-ref flags so libraries can pre-flight requirements instead of discovering `NotSupported` mid-flight.
- **Revisit mailbox API:** either drop `DropOldest` entirely or mark it `#[non_exhaustive]`/experimental; specify fairness guarantees for priority scheduling.
- **Tighten interceptor semantics:** document that `Reject` for `tell` is for non-critical cases only, or provide an opt-in "strict tell" mode that surfaces rejections.
- **For `MessageCodec`:** consider a typed codec boundary (e.g., `Codec<M: Serialize>`) rather than raw `[u8]` ↔ `[u8]`.
- **Ensure proc-macros emit friendly error messages** when methods are generic, return `impl Trait`, or use unsupported patterns.

### 4. Questions

- How will a user reliably know at startup that a chosen runtime supports a required set of capabilities (ask, stream, watch, priority mailbox)?
- What is the story for schema evolution of remote messages (versioning, backwards compatibility) with the default bincode codec?
- Is there a recommended mapping table from each backend's native error types into the shared `ErrorCode` values?
- For remote business errors, is the canonical pattern `Message::Reply = Result<T, ActorError>` (application-level) plus `RuntimeError::Actor(ActorError)` (infrastructure/handler panic), or are other combinations expected?

---

## Review 3: Gemini 3 Pro (Preview)

> ⚠️ **Unavailable** — model returned "not supported" error. Review not conducted.

---

## Cross-Review Summary

### Consensus (both reviewers agree)

| Finding | Category |
|---|---|
| Superset rule + capability matrix is well-designed | ✅ Strength |
| `ActorError` with gRPC-inspired error codes is production-ready | ✅ Strength |
| Mock cluster with fault injection is a standout differentiator | ✅ Strength |
| Interceptor + Headers design is powerful and non-opinionated | ✅ Strength |
| `on_error` signature is inconsistent between §3.6 and §10.6 | ⚠️ Fix needed |
| `ActorContext` internals are underspecified | ⚠️ Gap |
| No capability discovery / introspection API | ⚠️ Suggestion |
| `DropOldest` is in the API but unsupported everywhere | ⚠️ Consider removing |
| Timeout support for `ask()` is missing | ⚠️ Gap |
| Interceptor `Reject` for `tell` silently swallowed — potentially dangerous | ⚠️ Needs documentation |
| Remote serialization protocol/enforcement not specified | ⚠️ Gap |
| Priority mailbox starvation/fairness not addressed | ⚠️ Gap |

### Unique insights per reviewer

| Reviewer | Unique finding |
|---|---|
| **Haiku** | Watch/unwatch notification delivery mechanism unspecified — does the watcher need a `ChildTerminated` handler? |
| **Haiku** | Dead letter queue concept missing — what happens to dropped messages? |
| **Haiku** | `StreamSender::send()` should be `#[must_use]` for safety |
| **Haiku** | `InterceptContext` should include `remote: bool` and `origin_node` for cluster-aware policies |
| **Haiku** | Dynamic mailbox config changes not supported (no `update_actor_config`) |
| **GPT** | Dual `AskRef`/`StreamRef` vs `ActorRef<A>::ask()` creates two mental models — collapse into one |
| **GPT** | Typed codec boundary (`Codec<M: Serialize>`) better than raw `[u8]` ↔ `[u8]` |
| **GPT** | Need a mapping table from each backend's native errors to `ErrorCode` values |
| **GPT** | `#[non_exhaustive]` on `OverflowStrategy` / `ErrorCode` for future-proofing |
