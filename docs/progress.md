# dactor v0.2 — Implementation Progress

> Tracks PR status for the v0.2 implementation. See [dev-plan.md](dev-plan.md) for full plan details.

---

## Progress Summary

| Milestone | PRs | Status |
|-----------|-----|--------|
| v0.2.0-alpha.1 — Core API + Test Harness | PR 1–3 | 🟡 In progress (1/3) |
| v0.2.0-alpha.2 — Communication (tell/ask) | PR 4–6 | 🔲 Not started |
| v0.2.0-alpha.3 — Messaging & Mailbox | PR 7–11 | 🔲 Not started |
| v0.2.0-beta.1 — Streaming & Cancellation | PR 12–15 | 🔲 Not started |
| v0.2.0-beta.2 — Error Model & Persistence | PR 16–18 | 🔲 Not started |
| v0.2.0-rc.1 — Observability & Remote | PR 19–21 | 🔲 Not started |

---

## PR Tracker

| PR | Title | Branch | Status | Tests | Notes |
|----|-------|--------|--------|-------|-------|
| 1 | Module reorganization & cleanup | impl/pr-01-module-reorg | ✅ PR #3 | 46/46 pass | Green-to-green refactor |
| 2 | Actor trait & ActorId | | 🔲 Not started | | |
| 3 | **Integration test harness (gRPC)** | | 🔲 Not started | | Prerequisite for e2e tests |
| 4 | Message, Handler, ActorRef\<A\> | | 🔲 Not started | | |
| 5 | Tell (fire-and-forget) | | 🔲 Not started | | |
| 6 | Ask (request-reply) | | 🔲 Not started | | |
| 7 | Envelope, Headers, RuntimeHeaders | | 🔲 Not started | | |
| 8 | Interceptor pipeline (Inbound) | | 🔲 Not started | | |
| 9 | Interceptor pipeline (Outbound) | | 🔲 Not started | | |
| 10 | Lifecycle hooks & ErrorAction | | 🔲 Not started | | |
| 11 | MailboxConfig & OverflowStrategy | | 🔲 Not started | | |
| 12 | Supervision & DeathWatch | | 🔲 Not started | | |
| 13 | Stream (server-streaming) | | 🔲 Not started | | |
| 14 | Feed (client-streaming) | | 🔲 Not started | | |
| 15 | Cancellation (CancellationToken) | | 🔲 Not started | | |
| 16 | Error model (ActorError, ErrorCodec) | | 🔲 Not started | | |
| 17 | Persistence (EventSourced + DurableState) | | 🔲 Not started | | |
| 18 | Dead letters, Delay, Throttle | | 🔲 Not started | | |
| 19 | Observability (MetricsInterceptor) | | 🔲 Not started | | |
| 20 | Conformance suite & MockCluster | | 🔲 Not started | | |
| 21 | Remote actors & cluster stubs | | 🔲 Not started | | |

---

## Test Coverage

| Layer | Target | Current |
|-------|--------|---------|
| Core unit tests | ~150 | 0 |
| Adapter unit tests | ~80 | 44 (v0.1) |
| Conformance tests | ~50 | 0 |
| Integration tests | ~30 | 0 |
| **Total** | **~310** | **44** |

---

## Change Log

| Date | PR | Change |
|------|-----|--------|
| 2026-03-30 | PR 1 | Module reorganization: split traits/runtime.rs, feature-gate test_support, NodeId(String), serde optional |
