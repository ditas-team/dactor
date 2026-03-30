# dactor v0.2 Design Document — Review 3

> Reviews conducted on 2026-03-29 using Claude Haiku 4.5, GPT-5.1, and GPT-5.4-mini.

---

## Consensus Findings (all reviewers agree)

| Finding | Severity | Section |
|---|---|---|
| `SpawnConfig` field name mismatch: `inbound_interceptors` vs `interceptors` in example | 🟡 | §4.7 / §5.2 |
| `ClusterEvents` / `ClusterEvent` referenced but never defined | 🔴 | §4.5 |
| `TimerHandle` referenced but never defined | 🟡 | §4.5 |
| `Outcome` enum differs between §5.2 (TellSuccess/AskSuccess) and §9.1 (Success) | 🟡 | §5.2 / §9.1 |
| Appendix A still references `ActorRef<M>` as Layer 1 core — contradicts §4 decision | 🟡 | Appendix A |
| `ValidationErrorCodec` example doesn't match new `ErrorCodec<E>` / `ErasedErrorCodec` API | 🟡 | §9.1 |

## Per-Reviewer Findings

### Haiku

| Finding | Severity |
|---|---|
| `ActorRef<M>` in research comparison table (§3) — ractor row shows message-typed | 🟢 Info (describes ractor, not dactor) |
| `NodeId` type referenced 96 times but never defined | 🔴 |
| `rgb()` color in §9.1 mermaid diagram may not render | 🟡 |
| `Outcome` enum needs single authoritative definition | 🟡 |

### GPT-5.1

| Finding | Severity |
|---|---|
| Layer diagram in Appendix A says `ActorRef<M>` is core — stale | 🟡 |
| `ValidationErrorCodec` impl/registration example stale | 🟡 |
| All mermaid diagrams verified clean (no stateDiagram, no invalid syntax) | ✅ |

### GPT-5.4-mini

| Finding | Severity |
|---|---|
| `ActorRef<M>` in Appendix A.5 "Layer 1 core" — contradicts §4 | 🟡 |
| `ErrorCodec` example still uses old `impl ErrorCodec for` not `impl ErrorCodec<E> for` | 🟡 |
| `header_name_static()` called in HeaderRegistry but trait only defines `header_name(&self)` | 🟡 |
| `PriorityFunction` referenced in §8.1 MailboxConfig but never defined | 🟡 |
| `WatchRequest`/`UnwatchRequest` mentioned in §10.2 but never defined | 🟢 |
| `HM` node reference in §10.1 mermaid diagram — node removed but ref remains | 🔴 |
| `Result‹Receipt, ActorError›` uses unusual guillemets in §11.4 mermaid | 🟢 |
| Complete Example (§4.8) uses `fn on_start(&mut self)` but trait requires `async fn on_start(&mut self, ctx)` | 🟡 |

---

## Action Items

1. ✅ Fixed: `SpawnConfig` field name in §5.2 example: `interceptors` → `inbound_interceptors`
2. ✅ Fixed: Added `NodeId` definition in §4.4 with assignment flow and `NodeIdMapper`
3. ✅ Fixed: Added `ClusterEvents` / `ClusterEvent` trait/enum definition in §4.5
4. ✅ Fixed: Added `TimerHandle` trait definition in §4.5
5. ✅ Fixed: Unified `Outcome` enum — removed §9.1 duplicate, §5.2 is authoritative
6. ✅ Fixed: Updated Appendix A Layer 1 text — notes it was superseded by §4 decision
7. ✅ Fixed: `ValidationErrorCodec` uses `ErrorCodec<ValidationErrors>` + `erased()` registration
8. ✅ Fixed: Removed stale `HM` from §10.1 mermaid diagram
9. ✅ Fixed: Removed `rgb()` from §9.1 mermaid
10. ✅ Fixed: §4.8 Complete Example uses `async fn on_start(&mut self, ctx: &mut ActorContext)`
11. ✅ Fixed: Removed `PriorityFunction` reference from §8.1
12. ✅ Fixed: `header_name_static()` → `Default`-based registration in HeaderRegistry
13. ✅ Fixed: `RuntimeCapabilities` struct → `RuntimeCapability` enum + `is_supported()`
14. ✅ Fixed: `NodeId` assignment documented — dactor runtime assigns, adapter maps to native ID
