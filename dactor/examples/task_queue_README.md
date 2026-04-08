# Task Queue Example

A task queue demonstrating **dactor** features in a realistic pipeline with workers, retries, and dead-letter routing. Runs locally using `TestRuntime` with a `PoolRef` worker pool.

## Features Demonstrated

| Feature | Where |
|---|---|
| **Actor pools** (RoundRobin) | 3 workers behind `PoolRef` |
| **Tell / Ask patterns** | Submit tasks (tell), query metrics (ask) |
| **Retry logic** | Dispatcher retries failed tasks up to `max_retries` |
| **Dead-letter routing** | Permanently failed tasks go to `DeadLetterCollector` |
| **Inbound interceptor** | Logging interceptor on every worker |
| **Lifecycle hooks** | `on_start` / `on_stop` on Workers, Dispatcher, Collector |
| **Timers** | Periodic metrics report via `send_interval` |
| **Bounded mailbox** | Workers use capacity-10 mailbox with `RejectWithError` |
| **Metrics** | Runtime-level metrics via `MetricsRegistry` |
| **Error handling** | Workers use `ErrorAction::Resume` to keep processing |

## Architecture

```
Client ──tell──▶ Dispatcher ──tell──▶ WorkerPool (RoundRobin, 3 workers)
                      ▲                     │
                      │          success ◀───┘
                      │            │
                      │            ▼
                      │       MetricsActor ◀── GetMetrics (ask)
                      │
                      │          failure ◀───┘
                      │            │
                      │     retry? ├──▶ back to WorkerPool
                      │            │
                      │     max    └──▶ DeadLetterCollector
                      │
        Timer ────────┘  (periodic metrics report)
```

## Task Types

- **Easy tasks** (10) — always succeed on first attempt
- **Hard tasks** (5) — fail on first attempt, succeed on retry
- **Impossible tasks** (5) — always fail, routed to dead-letter after exhausting retries

## How to Run

```bash
cargo run --example task_queue -p dactor --features test-support,metrics
```

## Expected Output

```
Tasks submitted:    20
Tasks completed:    15
Retries performed:  13
Dead-lettered:      5
Success rate:       75%
```

All 10 easy tasks succeed immediately. All 5 hard tasks fail once then recover.
All 5 impossible tasks exhaust retries and land in the dead-letter collector.
