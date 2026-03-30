# dactor v0.2 Design Document — Review 5 (Final)

> Reviews conducted on 2026-03-29 using GPT-5.1, Claude Haiku 4.5, and Claude Sonnet 4.6.

---

## Overall Verdict

| Reviewer | Verdict |
|---|---|
| **Haiku** | ✅ Ready for implementation. 1 minor fix (ToggleableInterceptor delegation — already fixed). |
| **GPT** | ✅ Ready with minor gaps. Core traits consistent enough to begin. |
| **Sonnet** | ⚠️ Ready with caveats. Several signature/type gaps need fixing. |

---

## Findings

### Stale Section References

| Finding | Reviewer | Severity |
|---|---|---|
| GPT found no stale refs | GPT | ✅ |
| Haiku found no stale refs | Haiku | ✅ |
| §10.1 for WireEnvelope → should be §9.1 | Sonnet | 🟢 |
| §9.1 for ActorError → should be §7.1 (in Outcome comment) | Sonnet | 🟢 |
| §14.1-14.4 refs in testing context → should be §12.1-12.4 | Sonnet | 🟢 |

### Signature Inconsistencies

| Finding | Reviewers | Severity |
|---|---|---|
| `ask()` called with 1 arg in examples (missing `cancel` param) — ~6 sites | Sonnet | 🟡 |
| `runtime.capabilities()` used but method is `is_supported()` | Sonnet + GPT | 🟡 |
| `Actor` impl blocks missing `create()` in examples | Sonnet | 🟡 |
| `ctx.actor_id` moved from `&InboundContext` — needs `.clone()` | Sonnet | 🟢 |

### Missing / Undefined Types

| Type | Reviewers | Severity |
|---|---|---|
| `OutboundQueueConfig` — used but undefined | Sonnet + GPT | 🟡 |
| `ctx.encode_error()` / `runtime.decode_error()` — used but not on traits | Sonnet + GPT | 🟡 |
| `ctx.spawn_with_config()` — used but not in ActorContext | Sonnet | 🟡 |
| `runtime.lookup()` — used but deferred to v0.4 | Sonnet | 🟢 |
| `runtime.set_cluster_discovery()` / `register_version_handler()` — not on trait | Sonnet | 🟢 |
| `WireHeaders` struct — used but never defined | Sonnet | 🟢 |
| `CodecError` — used in MessageCodec but undefined | Sonnet | 🟢 |
| `AskRef`/`StreamRef` in roadmap/layout — stale from prior design | Sonnet | 🟢 |
| `MockRuntime`/`MockClusterEvents` — referenced but undefined | GPT | 🟢 |

### Mermaid / Markdown

| Finding | Reviewers | Severity |
|---|---|---|
| `&`-grouped edges (`A & B --> C`) may not render in all renderers | GPT | 🟡 |
| Table missing header row (~line 2013) | Sonnet | 🟢 |
| `NodeId(node-2)` literal in mermaid — invalid Rust but ok for diagram | Sonnet | 🟢 |

### Rust Validity

| Finding | Reviewer | Severity |
|---|---|---|
| `Outcome::AskSuccess { reply: &dyn Any }` needs lifetime `<'a>` | Sonnet | 🟡 |
| `impl<M> Message for M` blanket impl in ClosureActor — conflicts | Sonnet | 🟡 |
| ToggleableInterceptor missing `on_complete`/`on_stream_item` delegation | Haiku | ✅ Fixed |

---

## Summary

The document has been through 5 review rounds. The remaining issues are:

- **Cosmetic:** ~4 stale section refs, table header missing
- **Example consistency:** `ask()` missing cancel param, `capabilities()` vs `is_supported()`, `create()` in Actor impls
- **Undefined helpers:** `encode_error`/`decode_error`, `OutboundQueueConfig`, `spawn_with_config` on ctx
- **Rust correctness:** `Outcome` needs lifetime, ClosureActor blanket impl invalid

**All core design decisions are finalized.** The issues are example-level, not architectural. The document is ready for implementation — these items can be resolved during coding.
