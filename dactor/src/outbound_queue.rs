//! Per-destination priority queue for outbound remote messages.
//!
//! [`OutboundPriorityQueue`] sorts outbound [`WireEnvelope`]s by priority
//! before sending, ensuring high-priority messages are transmitted first
//! regardless of enqueue order.
//!
//! ## Priority lanes
//!
//! Messages are bucketed into lanes by their [`Priority`] header value:
//!
//! | Lane | Priority range | Example |
//! |------|---------------|---------|
//! | Critical | 0–63 | System health, circuit breaker |
//! | High | 64–127 | User requests, RPC |
//! | Normal | 128–191 | Default (no priority header) |
//! | Low | 192–254 | Analytics, logging |
//! | Background | 255 | Batch jobs, precomputation |
//!
//! Within a lane, messages are delivered in FIFO order.
//!
//! ## Starvation mitigation
//!
//! By default, strict priority ordering is used (highest-priority first).
//! Under sustained high-priority load, lower lanes may starve. To prevent
//! this, configure `max_consecutive` on the queue — after dequeuing that
//! many messages from a single lane, the queue round-robins to the next
//! non-empty lane regardless of priority.

use std::collections::VecDeque;

use crate::message::Priority;
use crate::remote::WireEnvelope;

/// Number of priority lanes.
const LANE_COUNT: usize = 5;

/// The well-known header name for priority (kept in sync with Priority::header_name).
const PRIORITY_HEADER_NAME: &str = "dactor.Priority";

/// Extracts the priority lane index from a WireEnvelope's headers.
fn lane_index(envelope: &WireEnvelope) -> usize {
    let priority_value = envelope
        .headers
        .get(PRIORITY_HEADER_NAME)
        .and_then(|bytes| bytes.first().copied())
        .unwrap_or(Priority::NORMAL.0);

    match priority_value {
        0..=63 => 0,    // Critical
        64..=127 => 1,  // High
        128..=191 => 2, // Normal
        192..=254 => 3, // Low
        255 => 4,       // Background
    }
}

/// Per-destination priority queue that orders outbound envelopes by
/// priority before transmission.
///
/// Higher-priority envelopes are dequeued first. Within the same
/// priority lane, FIFO order is preserved.
///
/// ## Starvation mitigation
///
/// Set `max_consecutive` to limit how many messages are dequeued from
/// a single lane before round-robining to the next. Default is `0`
/// (unlimited / strict priority).
///
/// ## Capacity
///
/// Set `capacity` to limit total envelopes across all lanes. When full,
/// `push()` returns the rejected envelope as `Err`.
pub struct OutboundPriorityQueue {
    lanes: [VecDeque<WireEnvelope>; LANE_COUNT],
    total: usize,
    /// Max envelopes across all lanes (0 = unlimited).
    capacity: usize,
    /// Max consecutive pops from one lane before round-robin (0 = strict priority).
    max_consecutive: usize,
    /// Tracks consecutive pops from current lane for fairness.
    consecutive_from_lane: usize,
    /// Current lane being drained (for round-robin fairness).
    current_lane: usize,
}

impl OutboundPriorityQueue {
    /// Create a priority queue with no capacity limit and strict priority ordering.
    pub fn new() -> Self {
        Self {
            lanes: Default::default(),
            total: 0,
            capacity: 0,
            max_consecutive: 0,
            consecutive_from_lane: 0,
            current_lane: 0,
        }
    }

    /// Create a priority queue with capacity limit and fairness controls.
    ///
    /// - `capacity`: max total envelopes (0 = unlimited)
    /// - `max_consecutive`: max pops from one lane before checking lower
    ///   lanes (0 = strict priority, no fairness)
    pub fn with_config(capacity: usize, max_consecutive: usize) -> Self {
        Self {
            capacity,
            max_consecutive,
            ..Self::new()
        }
    }

    /// Enqueue an envelope into the appropriate priority lane.
    ///
    /// Returns `Err(envelope)` if the queue is at capacity.
    #[allow(clippy::result_large_err)]
    pub fn push(&mut self, envelope: WireEnvelope) -> Result<(), WireEnvelope> {
        if self.capacity > 0 && self.total >= self.capacity {
            return Err(envelope);
        }
        let lane = lane_index(&envelope);
        self.lanes[lane].push_back(envelope);
        self.total += 1;
        debug_assert_eq!(
            self.total,
            self.lanes.iter().map(|l| l.len()).sum::<usize>()
        );
        Ok(())
    }

    /// Dequeue the highest-priority envelope, with optional fairness.
    ///
    /// With `max_consecutive = 0` (default): strict priority — always
    /// returns the highest-priority available envelope.
    ///
    /// With `max_consecutive > 0`: after dequeuing that many from one
    /// lane, skips to the next non-empty lane (prevents starvation).
    pub fn pop(&mut self) -> Option<WireEnvelope> {
        if self.total == 0 {
            return None;
        }

        if self.max_consecutive == 0 {
            // Strict priority: scan Critical → Background
            return self.pop_strict();
        }

        // Fairness mode: check if we've exhausted consecutive budget
        if self.consecutive_from_lane >= self.max_consecutive {
            // Move to next non-empty lane
            self.consecutive_from_lane = 0;
            self.current_lane = self.next_non_empty_lane(self.current_lane + 1)?;
        } else if self.lanes[self.current_lane].is_empty() {
            // Current lane empty, find highest-priority non-empty
            self.consecutive_from_lane = 0;
            self.current_lane = self.next_non_empty_lane(0)?;
        }

        let envelope = self.lanes[self.current_lane].pop_front()?;
        self.total -= 1;
        self.consecutive_from_lane += 1;
        Some(envelope)
    }

    /// Strict priority pop (no fairness).
    fn pop_strict(&mut self) -> Option<WireEnvelope> {
        for lane in &mut self.lanes {
            if let Some(envelope) = lane.pop_front() {
                self.total -= 1;
                return Some(envelope);
            }
        }
        None
    }

    /// Find the next non-empty lane starting from `start` (wrapping).
    fn next_non_empty_lane(&self, start: usize) -> Option<usize> {
        for i in 0..LANE_COUNT {
            let idx = (start + i) % LANE_COUNT;
            if !self.lanes[idx].is_empty() {
                return Some(idx);
            }
        }
        None
    }

    /// Peek at the highest-priority envelope without removing it.
    pub fn peek(&self) -> Option<&WireEnvelope> {
        for lane in &self.lanes {
            if let Some(envelope) = lane.front() {
                return Some(envelope);
            }
        }
        None
    }

    /// Drain all envelopes in priority order (Critical lanes first, FIFO within).
    pub fn drain_ordered(&mut self) -> Vec<WireEnvelope> {
        let mut result = Vec::with_capacity(self.total);
        for lane in &mut self.lanes {
            result.extend(lane.drain(..));
        }
        self.total = 0;
        self.consecutive_from_lane = 0;
        result
    }

    /// Total envelopes across all lanes.
    pub fn len(&self) -> usize {
        self.total
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Number of envelopes in each lane (Critical, High, Normal, Low, Background).
    pub fn lane_counts(&self) -> [usize; LANE_COUNT] {
        [
            self.lanes[0].len(),
            self.lanes[1].len(),
            self.lanes[2].len(),
            self.lanes[3].len(),
            self.lanes[4].len(),
        ]
    }

    /// Clear all lanes.
    pub fn clear(&mut self) {
        for lane in &mut self.lanes {
            lane.clear();
        }
        self.total = 0;
    }
}

impl Default for OutboundPriorityQueue {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interceptor::SendMode;
    use crate::message::HeaderValue;
    use crate::node::{ActorId, NodeId};
    use crate::remote::WireHeaders;

    fn envelope_with_priority(priority: Priority) -> WireEnvelope {
        let mut headers = WireHeaders::new();
        if let Some(bytes) = priority.to_bytes() {
            headers.insert(priority.header_name().to_string(), bytes);
        }
        WireEnvelope {
            target: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::Msg".into(),
            send_mode: SendMode::Tell,
            headers,
            body: vec![priority.0],
            request_id: None,
            version: None,
        }
    }

    fn envelope_no_priority() -> WireEnvelope {
        WireEnvelope {
            target: ActorId {
                node: NodeId("n1".into()),
                local: 1,
            },
            target_name: "test".into(),
            message_type: "test::Msg".into(),
            send_mode: SendMode::Tell,
            headers: WireHeaders::new(),
            body: vec![],
            request_id: None,
            version: None,
        }
    }

    #[test]
    fn empty_queue() {
        let mut q = OutboundPriorityQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert!(q.pop().is_none());
        assert!(q.peek().is_none());
    }

    #[test]
    fn priority_ordering() {
        let mut q = OutboundPriorityQueue::new();

        // Enqueue in reverse priority order
        q.push(envelope_with_priority(Priority::BACKGROUND))
            .unwrap();
        q.push(envelope_with_priority(Priority::LOW)).unwrap();
        q.push(envelope_with_priority(Priority::NORMAL)).unwrap();
        q.push(envelope_with_priority(Priority::HIGH)).unwrap();
        q.push(envelope_with_priority(Priority::CRITICAL)).unwrap();

        assert_eq!(q.len(), 5);

        // Dequeue should be in priority order (Critical first)
        assert_eq!(q.pop().unwrap().body, vec![Priority::CRITICAL.0]);
        assert_eq!(q.pop().unwrap().body, vec![Priority::HIGH.0]);
        assert_eq!(q.pop().unwrap().body, vec![Priority::NORMAL.0]);
        assert_eq!(q.pop().unwrap().body, vec![Priority::LOW.0]);
        assert_eq!(q.pop().unwrap().body, vec![Priority::BACKGROUND.0]);
        assert!(q.is_empty());
    }

    #[test]
    fn fifo_within_same_priority() {
        let mut q = OutboundPriorityQueue::new();

        // Push 3 normal-priority messages
        let mut e1 = envelope_with_priority(Priority::NORMAL);
        e1.body = vec![1];
        let mut e2 = envelope_with_priority(Priority::NORMAL);
        e2.body = vec![2];
        let mut e3 = envelope_with_priority(Priority::NORMAL);
        e3.body = vec![3];

        q.push(e1).unwrap();
        q.push(e2).unwrap();
        q.push(e3).unwrap();

        // FIFO order within the lane
        assert_eq!(q.pop().unwrap().body, vec![1]);
        assert_eq!(q.pop().unwrap().body, vec![2]);
        assert_eq!(q.pop().unwrap().body, vec![3]);
    }

    #[test]
    fn no_priority_header_defaults_to_normal() {
        let mut q = OutboundPriorityQueue::new();
        q.push(envelope_no_priority()).unwrap();

        let counts = q.lane_counts();
        assert_eq!(counts[2], 1); // Normal lane
        assert_eq!(counts[0], 0); // Critical
    }

    #[test]
    fn high_priority_preempts_normal() {
        let mut q = OutboundPriorityQueue::new();

        // Enqueue normal first, then high
        q.push(envelope_with_priority(Priority::NORMAL)).unwrap();
        q.push(envelope_with_priority(Priority::HIGH)).unwrap();

        // High should come out first
        assert_eq!(q.pop().unwrap().body, vec![Priority::HIGH.0]);
        assert_eq!(q.pop().unwrap().body, vec![Priority::NORMAL.0]);
    }

    #[test]
    fn peek_returns_highest_priority() {
        let mut q = OutboundPriorityQueue::new();
        q.push(envelope_with_priority(Priority::LOW)).unwrap();
        q.push(envelope_with_priority(Priority::CRITICAL)).unwrap();

        let peeked = q.peek().unwrap();
        assert_eq!(peeked.body, vec![Priority::CRITICAL.0]);
        assert_eq!(q.len(), 2); // peek doesn't remove
    }

    #[test]
    fn drain_ordered_returns_all_by_priority() {
        let mut q = OutboundPriorityQueue::new();
        q.push(envelope_with_priority(Priority::LOW)).unwrap();
        q.push(envelope_with_priority(Priority::CRITICAL)).unwrap();
        q.push(envelope_with_priority(Priority::NORMAL)).unwrap();

        let drained = q.drain_ordered();
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0].body, vec![Priority::CRITICAL.0]);
        assert_eq!(drained[1].body, vec![Priority::NORMAL.0]);
        assert_eq!(drained[2].body, vec![Priority::LOW.0]);
        assert!(q.is_empty());
    }

    #[test]
    fn lane_counts() {
        let mut q = OutboundPriorityQueue::new();
        q.push(envelope_with_priority(Priority::CRITICAL)).unwrap();
        q.push(envelope_with_priority(Priority::CRITICAL)).unwrap();
        q.push(envelope_with_priority(Priority::HIGH)).unwrap();
        q.push(envelope_with_priority(Priority::NORMAL)).unwrap();
        q.push(envelope_with_priority(Priority::LOW)).unwrap();
        q.push(envelope_with_priority(Priority::BACKGROUND))
            .unwrap();

        let counts = q.lane_counts();
        assert_eq!(counts, [2, 1, 1, 1, 1]);
    }

    #[test]
    fn clear_empties_all_lanes() {
        let mut q = OutboundPriorityQueue::new();
        q.push(envelope_with_priority(Priority::CRITICAL)).unwrap();
        q.push(envelope_with_priority(Priority::HIGH)).unwrap();
        q.push(envelope_with_priority(Priority::NORMAL)).unwrap();

        q.clear();
        assert!(q.is_empty());
        assert_eq!(q.lane_counts(), [0, 0, 0, 0, 0]);
    }

    #[test]
    fn mixed_priority_interleaving() {
        let mut q = OutboundPriorityQueue::new();

        // Simulate real traffic: mostly normal, some high, rare critical
        q.push(envelope_with_priority(Priority::NORMAL)).unwrap();
        q.push(envelope_with_priority(Priority::NORMAL)).unwrap();
        q.push(envelope_with_priority(Priority::HIGH)).unwrap();
        q.push(envelope_with_priority(Priority::NORMAL)).unwrap();
        q.push(envelope_with_priority(Priority::CRITICAL)).unwrap();

        // Critical first, then high, then normals in FIFO
        assert_eq!(q.pop().unwrap().body, vec![Priority::CRITICAL.0]);
        assert_eq!(q.pop().unwrap().body, vec![Priority::HIGH.0]);
        assert_eq!(q.pop().unwrap().body, vec![Priority::NORMAL.0]);
        assert_eq!(q.pop().unwrap().body, vec![Priority::NORMAL.0]);
        assert_eq!(q.pop().unwrap().body, vec![Priority::NORMAL.0]);
    }

    #[test]
    fn lane_boundary_values() {
        let mut q = OutboundPriorityQueue::new();
        q.push(envelope_with_priority(Priority(63))).unwrap(); // Critical
        q.push(envelope_with_priority(Priority(64))).unwrap(); // High
        q.push(envelope_with_priority(Priority(127))).unwrap(); // High
        q.push(envelope_with_priority(Priority(128))).unwrap(); // Normal
        q.push(envelope_with_priority(Priority(191))).unwrap(); // Normal
        q.push(envelope_with_priority(Priority(192))).unwrap(); // Low
        q.push(envelope_with_priority(Priority(254))).unwrap(); // Low
        q.push(envelope_with_priority(Priority(255))).unwrap(); // Background

        let counts = q.lane_counts();
        assert_eq!(counts[0], 1); // Critical: 63
        assert_eq!(counts[1], 2); // High: 64, 127
        assert_eq!(counts[2], 2); // Normal: 128, 191
        assert_eq!(counts[3], 2); // Low: 192, 254
        assert_eq!(counts[4], 1); // Background: 255
    }

    #[test]
    fn capacity_rejects_when_full() {
        let mut q = OutboundPriorityQueue::with_config(3, 0);
        q.push(envelope_with_priority(Priority::NORMAL)).unwrap();
        q.push(envelope_with_priority(Priority::NORMAL)).unwrap();
        q.push(envelope_with_priority(Priority::NORMAL)).unwrap();
        let result = q.push(envelope_with_priority(Priority::CRITICAL));
        assert!(result.is_err());
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn fairness_prevents_starvation() {
        let mut q = OutboundPriorityQueue::with_config(0, 2);
        for _ in 0..5 {
            q.push(envelope_with_priority(Priority::CRITICAL)).unwrap();
        }
        for _ in 0..3 {
            q.push(envelope_with_priority(Priority::NORMAL)).unwrap();
        }

        // Pop 2 critical (consecutive limit)
        assert_eq!(q.pop().unwrap().body, vec![Priority::CRITICAL.0]);
        assert_eq!(q.pop().unwrap().body, vec![Priority::CRITICAL.0]);
        // 3rd pop: fairness rotates past critical lane
        let third = q.pop().unwrap();
        // Should come from a different lane (normal, since high/low/bg are empty)
        assert_eq!(third.body, vec![Priority::NORMAL.0]);
    }
}
