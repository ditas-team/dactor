# dactor v0.2 Design Document — Review 2

> Reviews conducted on 2026-03-29 using Claude Haiku 4.5 and GPT-5.1.
> Gemini 3 Pro (Preview) was unavailable (model not supported).

---

## Review 1: GPT-5.1

### Inconsistencies

- `ActorRef`/`ActorRuntime` are conflicting: actor-typed in §4.1/4.3/6.1 vs message-typed trait in §4.4, closure-based `spawn()` in §4.5 vs actor-based `spawn_*` in §4.1/§11.2.
- `RuntimeCapabilities` fields differ (§2.4 vs §4.5).
- `SpawnConfig` uses `inbound_interceptors` vs `interceptors` in `Default` (§4.7).
- Health: `HealthMonitor` system actor (§10.1–10.2) contradicts "provider owns health" (§12.3).
- Stale concepts: `tell_envelope`/`tell_with_priority` (§5.1–5.3) and outbound "user lane/outbound-priority" hints (§8.1–8.2) contradict §5.3/§8.3/§19.1.

### Completeness Gaps

- Streaming: `ActorRef::stream` (§6.3) depends on `Handler<M>` (§4.3) but no stream-specific handler/trait (how does `StreamSender` reach the actor?).
- Several referenced types lack definitions: `TimerHandle`, `ClusterEvents`, `GroupError`, `Clock` (§4.5, §8, §9, §10.1).
- Error/version plumbing (`MessageVersionHandler` registration, `ErrorCodec` registry, CancelManager bookkeeping) is described behaviorally but not with concrete API signatures.

### Feasibility Concerns

- `ErrorCodec` has an associated type but is stored as `Box<dyn ErrorCodec>` in a heterogeneous registry (§9.1); that's not object-safe in Rust. Needs either a type-erased payload codec or a generic registry keyed by payload type.
- Serialization of `ActorRef<A>` via a blanket `impl Serialize` on the ref (§11.3) is hand-wavy; with adapter-specific routing and no concrete `ActorRef` type here, this requires a concrete wrapper type per runtime.
- Remote spawn requires `Actor + Serialize + DeserializeOwned` (§11.2/§11.3), which is awkward for actors with non-serializable state; the later Args/Deps split (§4.1) solves this but the remote-spawn section still assumes serializable actor structs.

### API Ergonomics

- Multiple spawn APIs (`spawn`, `spawn_with_deps`, `spawn_with_config`, closure-based `spawn`, `spawn_pool`, remote `spawn_with_config`) with inconsistent parameter order and semantics (§4.1, §4.5, §4.7, §8.4, §11.2) will confuse users; consolidating on Args/Deps + a single config-taking family would help.
- `ActorRef` shifting between actor-typed and message-typed across sections is a big mental tax; pick the Kameo/Coerce pattern and delete message-typed remnants.
- Interceptor traits' example implementations don't match the signatures (e.g., `LoggingInterceptor::on_receive` missing the `message: &dyn Any` arg, §5.2), which will frustrate users copying code.

### Summary

Overall the design is strong and Rust-feasible, but the document mixes an older, message-typed/closure-based runtime with the newer actor-typed/Args+Deps model. Normalize `ActorRef`/`ActorRuntime` and `SpawnConfig`/`RuntimeCapabilities`, delete stale health/transport/priority concepts, and add a precise stream-handler story and object-safe error/codec registries.

---

## Review 2: Claude Haiku 4.5

### Inconsistencies

1. **§5.1 vs §6.1 — Envelope API confusion**: §5.1 defines `Envelope<M>` as internal, §6.1 shows `tell_envelope()`/`tell_with_priority()` methods, but §5.3 says outbound interceptors eliminate the need for these. Are they public API or not?

2. **§8.3 — Network priority policy confusion**: States "dactor does not implement or re-wrap the network layer" but references an optional "outbound-priority feature". Either dactor manages outbound priority or it doesn't.

3. **§7.1 vs §4.1 — `on_error` invocation**: §4.1 shows `on_error(&ActorError)` but when exactly is it called vs handler errors? Lifecycle sequencing unclear.

4. **§8.1 vs §4.7 — `comparer` field missing from `SpawnConfig`**: §8.1 shows `comparer: Some(...)` in SpawnConfig but §4.7's SpawnConfig struct doesn't have this field.

5. **§9.1 — Local vs remote error handling**: Describes `ActorError` as "crosses node boundaries" but examples show it used locally. Should handlers return custom errors (via ErrorCodec) or `ActorError` directly?

6. **§10.1 diagram incomplete**: `CancelManager` appears in system actors table but not in the architecture diagram.

7. **§11.2 — Args/Deps serialization**: Unclear whether `Args` are always serialized or only for remote spawning.

### Completeness Gaps

1. **§5.1 — WireEnvelope definition location**: Mentioned in §5.1 but full struct not shown until §11.1.
2. **§5.4 — DeadLetterHandler execution context**: What thread? Is it async? Can it send messages?
3. **§6.4 — CPU-bound handler cancellation**: Does caller wait or return immediately?
4. **§6.3 — Stream handler type signature**: No concrete `impl Handler` shown for streaming.
5. **§7.2 — ChildTerminated delivery ordering**: Are they priority? In-order with other messages?
6. **§8.4 — PoolRef definition missing**: Says it implements ActorRef but no struct/trait shown.
7. **§9.1 — `register_error_codec()` missing from ActorRuntime trait**.
8. **§10.3 — Registry query methods undefined**.
9. **§12.1 — ClusterDiscovery lifecycle**: Does `start()` block? Runs on separate task?
10. **§13.3 — MetricsStore thread safety unspecified**.
11. **§15.1 — Proc-macro error examples**: Show invalid code that triggers each error.

### Feasibility Concerns

1. **§4.5 — GAT usage**: `type Ref<M: Send + 'static>: ActorRef<M>` uses GATs; test with complex scenarios.
2. **§5.1 — Header name uniqueness**: Two types with same `header_name()` would collide on wire.
3. **§5.2 — Message immutability in interceptors**: `&dyn Any` (not `&mut`) — document this contract.
4. **§6.3 — StreamSender Send bound**: `StreamSender<T>` requires `T: Send`; handlers with `Rc` won't work.
5. **§8.1 — QueuedMessageMeta.age staleness**: Age should be recalculated per-dequeue, not stored.
6. **§9.1 — ErrorCodec trait object safety**: `Self::Error` associated type makes `Box<dyn ErrorCodec>` non-object-safe.
7. **§11.1 — Version migration chain**: v1→v2→v3 migration ordering unclear.
8. **§14.2 — Corrupt message behavior**: Silent drop or error?

### API Ergonomics

1. **§4.5 — Spawn overload burden**: Three methods; add "choosing the right spawn" guide.
2. **§4.1 — Args/Deps defaults**: Provide "when to override" table.
3. **§5.2/5.3 — Two context structs**: `InterceptContext` vs `OutboundContext` — could unify?
4. **§6.2 — `cancel_after()` location**: Where is it importable from?
5. **§7.1 — Supervision strategies extensibility**: Clarify that custom strategies are possible via trait.
6. **§8.4 — `deps_factory` flexibility**: Closure limits expressiveness; consider trait.
7. **§13.3 — `busiest_actors()` sort key**: What metric? Document clearly.
8. **§14.2 — MockCluster builder order**: Does `default_link` apply to already-added nodes?

### Summary

| Category | Count | Severity |
|---|---|---|
| Inconsistencies | 7 | 🟡 Medium |
| Completeness Gaps | 11 | 🟡 Medium |
| Feasibility Concerns | 8 | 🟢 Low–🟡 Medium |
| API Ergonomics | 8 | 🟢 Low |

The design is **comprehensive and implementable** with no fundamental blockers. Main action items: eliminate stale `tell_envelope` references, complete missing type definitions (`PoolRef`, `SpawnConfig.comparer`), add missing trait methods (`register_error_codec`), and clarify ambiguous flows (stream handler, version migration, error handling path).

---

## Review 3: Gemini 3 Pro (Preview)

> ⚠️ **Unavailable** — model returned "not supported" error. Review not conducted.

---

## Cross-Review Summary

### Consensus (both reviewers agree)

| Finding | Category | Severity |
|---|---|---|
| Stale `tell_envelope`/`tell_with_priority` references remain | Inconsistency | 🟡 |
| `ActorRef` mixes actor-typed (§4) and message-typed (§4.4/§4.5) patterns | Inconsistency | 🔴 |
| `SpawnConfig` missing `comparer` field | Completeness | 🟡 |
| `HealthMonitor` still referenced in §10 despite delegation to provider (§12.3) | Inconsistency | 🟡 |
| `ErrorCodec` trait not object-safe due to associated type | Feasibility | 🟡 |
| Stream handler type signature not shown | Completeness | 🟡 |
| `register_error_codec()` missing from `ActorRuntime` trait | Completeness | 🟡 |
| Multiple spawn overloads confusing — need guidance | Ergonomics | 🟢 |
| Interceptor example code doesn't match current signatures | Inconsistency | 🟡 |
| `RuntimeCapabilities` fields differ between sections | Inconsistency | 🟡 |

### Unique insights per reviewer

| Reviewer | Finding |
|---|---|
| **GPT** | Remote spawn section still assumes serializable actor structs despite Args/Deps split |
| **GPT** | `ActorRef<A>` blanket `impl Serialize` is hand-wavy — needs concrete wrapper per runtime |
| **GPT** | Consolidate spawn APIs to single config-taking family |
| **Haiku** | `QueuedMessageMeta.age` should be recalculated per-dequeue, not stored |
| **Haiku** | `DeadLetterHandler` execution context (thread, async, can it send?) unspecified |
| **Haiku** | `ClusterDiscovery::start()` lifecycle unclear — blocks? spawns task? |
| **Haiku** | `ChildTerminated` delivery ordering relative to other messages unspecified |
| **Haiku** | Header name collision risk — need uniqueness enforcement |
| **Haiku** | Version migration chain (v1→v2→v3) ordering unclear |
| **Haiku** | `MetricsStore` thread safety model unspecified |
