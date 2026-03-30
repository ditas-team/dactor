# Design Document Review — Round 6

**Date:** 2026-03-29
**Document:** design-v0.2.md (~6900 lines)
**Reviewers:** Claude Haiku 4.5, GPT-5.2, Claude Sonnet 4.5

---

## Summary

Three independent reviewers analyzed the full document. Total issues found: **16 unique** (after deduplication). All issues have been addressed.

---

## Issues Found and Resolution

### CRITICAL (1)

| # | Reviewer | Issue | Resolution |
|---|---|---|---|
| 1 | Haiku, GPT | Line 1069 says "three communication patterns" but §4.12 adds `feed()` as a fourth pattern | ✅ Fixed: Updated to "four", added feed to mermaid diagram |

### IMPORTANT (6)

| # | Reviewer | Issue | Resolution |
|---|---|---|---|
| 2 | Sonnet | §4.13 Cancellation subsections numbered 4.12.1–4.12.8 instead of 4.13.1–4.13.8 | ✅ Fixed: Renumbered all 8 subsections |
| 3 | Sonnet | Line 6083: §5.3 reference should be §5.4 (Dead Letter Handling) | ✅ Fixed |
| 4 | Sonnet | Line 5881: §10.4 reference should be §9.5 (Sending to Unavailable Actors) | ✅ Fixed |
| 5 | Haiku | §9.1 error reference should be §7.1 (ActorError) at line 2756 | ✅ Fixed |
| 6 | GPT | `InboundContext.send_mode` doc says "tell or ask" — omits Stream and Feed | ✅ Fixed: Updated to "Tell, Ask, Stream, or Feed" |
| 7 | GPT | §5.2 Interceptor lifecycle docs cover Tell/Ask/Stream but not Feed | ✅ Fixed: Added Feed lifecycle section |

### MINOR (9)

| # | Reviewer | Issue | Resolution |
|---|---|---|---|
| 8 | Haiku | No forward reference from pattern matrix (line 1385) to §4.12 Feed | ✅ Fixed: Added forward reference |
| 9 | Haiku | `feed()` missing from adapter support tables (§14.3–14.5) | ✅ Fixed: Added feed() rows to all 4 adapter tables |
| 10 | Haiku | `feed()` missing from cancellation outcomes table (§4.13.5) | ✅ Fixed: Added feed column to table |
| 11 | GPT | ActorContext send_mode comment says "(Tell, Ask, Stream)" — omits Feed | ✅ Fixed: Updated to include Feed |
| 12 | GPT | §9.1 serializer table omits feed items/request/reply | ✅ Fixed: Added 3 feed rows to table |
| 13 | GPT | §9.1 MessageSerializer doc comment omits "feed" | ✅ Fixed: Added "feed" to list |
| 14 | GPT | Remote send example hardcodes `SendMode::Tell` | ✅ Fixed: Parameterized with `send_mode` argument |
| 15 | GPT | `on_complete` doc says "Tell/Ask" — omits Feed | ✅ Fixed: Updated to "Tell/Ask/Feed" |
| 16 | Sonnet | SpawnConfig.comparer field missing from §4.7 | ❌ False positive: Already defined at line 984 |

---

## Non-Issues / False Positives

| Reviewer | Claimed Issue | Actual Status |
|---|---|---|
| Sonnet | `register_error_codec()` missing from ActorRuntime | Already defined at line 902 |
| Sonnet | `add_outbound_interceptor()` missing from ActorRuntime | Already defined at line 898 |
| Sonnet | `SpawnConfig.comparer` missing | Already defined at line 984 |
| GPT | Missing subsections §5.9, §7.3, §7.4, §9.6, §10.5, §12.5, §13.4 | Document structure evolved; not all originally-planned subsections were needed |

---

## Verified Correct (All 3 Reviewers Agree)

- ✅ `ActorRef<A>` consistently typed to actor (never `ActorRef<M>`)
- ✅ `SendMode` enum has all 4 variants: Tell, Ask, Stream, Feed
- ✅ `Disposition` enum has all 4 variants: Continue, Delay, Drop, Reject
- ✅ `ErrorCode` enum complete with all documented codes
- ✅ `Priority` constants consistent: CRITICAL=0, HIGH=64, NORMAL=128, LOW=192, BACKGROUND=255
- ✅ `RuntimeError` variants match §2.3 and §7 definitions
- ✅ Code blocks properly closed throughout
- ✅ Mermaid diagrams properly formatted
- ✅ No stale "three pattern" references remain

---

## Reviewer Assessments

**Haiku 4.5:** "The design document is thorough and well-structured. Resolution of the communication patterns count discrepancy (CRITICAL) is required before implementation."

**GPT-5.2:** "Found 54 §-references; none are broken. A few misleading references and Feed coverage gaps in interceptor/remote sections."

**Sonnet 4.5:** "Overall assessment: comprehensive and well-structured with a few critical numbering issues. No stale content found. Design strengths: comprehensive trait hierarchy, error handling, lifecycle guarantees, interceptor pipeline."

**Conclusion:** All issues addressed. Document is implementation-ready.
