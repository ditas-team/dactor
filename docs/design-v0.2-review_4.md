# dactor v0.2 Design Document — Review 4

> Reviews conducted on 2026-03-29 using GPT-5.1, Claude Haiku 4.5, and Claude Sonnet 4.6.

---

## Consensus Findings (2+ reviewers agree)

| Finding | Severity | Reviewers |
|---|---|---|
| Stale §8.1/§8.3 refs (should be §5.6/§5.8) — ~10 occurrences | 🔴 | All 3 |
| `InboundInterceptor` defined twice with conflicting signatures (§5.1 has `RuntimeHeaders`, §5.2 does not) | 🔴 | Sonnet + GPT |
| `OutboundInterceptor` same duplication (§5.1 vs §5.3) | 🔴 | Sonnet + GPT |
| §11 interceptor examples missing `message: &dyn Any` parameter | 🟡 | Sonnet + GPT |
| `Outcome::Success` used in examples but enum has `TellSuccess` | 🟡 | All 3 |
| `spawn_with_config` examples pass wrong number of args (missing `deps`) | 🟡 | Sonnet + GPT |
| `ActorContext::spawn` takes `actor: A` but should take `args: A::Args` | 🟡 | Sonnet + Haiku |
| Stale §13.x refs (should be §11.x) | 🟡 | Sonnet + GPT |
| `SubscriptionId` referenced but undefined | 🟡 | Sonnet + GPT |

## Per-Reviewer Unique Findings

### Haiku
| Finding | Severity |
|---|---|
| `RuntimeMetrics` referenced (line 932) but never defined | 🟡 |
| `SendMode` defined late (line 2309), should be in §4 | 🟢 |
| `Cancelled` variant not in main `ErrorCode` enum (§7.1) | 🟢 |
| `PoolRef<A>` interface methods not specified | 🟡 |
| Handler return type `Result<T, ActorError>` vs bare `T` — needs clarification | 🟢 |

### Sonnet
| Finding | Severity |
|---|---|
| `AdapterCluster::connect` defined twice with different signatures (§10.2 vs §10.3) | 🔴 |
| `RuntimeError` defined twice (§2.3 without `Actor`, §7.1 with `Actor`) | 🟡 |
| `ActorSendError`, `GroupError`, `ClusterError` never defined | 🟡 |
| `ActorError::from_error` missing `payload: None` field | 🟢 |
| `Outcome::AskSuccess { reply: &dyn Any }` needs lifetime `<'a>` | 🟡 |
| Duplicate Tell/Ask/Stream mermaid diagram (lines ~1086–1130) | 🟡 |
| §7.1 error flow mermaid has broken `participant` after `Note` | 🟡 |
| `Envelope` vs `WireEnvelope` table missing header row | 🟢 |
| `ActorContext::encode_error()` and `runtime.decode_error()` referenced but not in trait | 🟢 |
| `runtime.lookup()` used but deferred to v0.4 | 🟢 |
| `Clock` trait in capability matrix but never defined | 🟢 |
| `ProcessingGroup` in capability matrix but never defined in doc | 🟢 |

### GPT
| Finding | Severity |
|---|---|
| `OutboundQueueConfig` referenced but undefined | 🟢 |
| `SlidingWindow` used in throttle example but undefined | 🟢 |

---

## Action Items

### Critical (fix before implementation)
1. Fix all stale §8.1/§8.3 → §5.6/§5.8 references (~10 occurrences)
2. Remove duplicate `InboundInterceptor` trait from §5.1 (§5.2 is canonical)
3. Remove duplicate `OutboundInterceptor` trait from §5.1 (§5.3 is canonical)
4. Remove duplicate `AdapterCluster::connect` — pick §10.2 signature
5. Fix stale §13.x → §11.x references
6. Fix stale §10.2/§11.1/§12.3/§12.4 references per Sonnet's findings

### Important (fix for consistency)
7. Fix all §11 interceptor examples to include `message: &dyn Any`
8. Fix `Outcome::Success` → `Outcome::TellSuccess` in examples
9. Fix `spawn_with_config` examples to include `deps` parameter
10. Fix `ActorContext::spawn` to take `args: A::Args` not `actor: A`
11. Remove duplicate `RuntimeError` from §2.3 (§7.1 is canonical)
12. Add `SubscriptionId` definition
13. Add `ActorSendError`, `GroupError`, `ClusterError` definitions
14. Add `Cancelled` to main `ErrorCode` enum in §7.1
15. Add `RuntimeMetrics` struct definition
16. Add `PoolRef<A>` method signatures
17. Fix `ActorError::from_error` — add `payload: None`
18. Remove duplicate mermaid diagram
19. Fix broken §7.1 mermaid diagram
