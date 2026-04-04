use std::cmp::Ordering;

/// Mailbox capacity configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[derive(Default)]
pub enum MailboxConfig {
    /// Unbounded mailbox — no capacity limit (default).
    #[default]
    Unbounded,
    /// Bounded mailbox with a fixed capacity.
    ///
    /// **Note:** `capacity` must be > 0. A zero-capacity mailbox would
    /// reject all messages immediately.
    Bounded {
        /// Maximum number of messages the mailbox can hold.
        capacity: usize,
        /// Strategy when the mailbox is full.
        overflow: OverflowStrategy,
    },
}

impl MailboxConfig {
    /// Create a bounded mailbox config. Panics if capacity is 0.
    pub fn bounded(capacity: usize, overflow: OverflowStrategy) -> Self {
        assert!(capacity > 0, "bounded mailbox capacity must be > 0");
        Self::Bounded { capacity, overflow }
    }
}


/// What happens when a bounded mailbox is full.
///
/// `DropOldest` is intentionally omitted — no provider supports queue eviction
/// efficiently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowStrategy {
    /// Block the sender until space is available.
    ///
    /// Note: `tell()` is synchronous, so Block is only effective for the test
    /// runtime's internal dispatch. Real adapters handle this natively.
    Block,
    /// Reject the message with an error.
    RejectWithError,
    /// Drop the newest message (the one being sent) silently.
    DropNewest,
}

// ---------------------------------------------------------------------------
// Priority mailbox comparison
// ---------------------------------------------------------------------------

/// Custom ordering for priority-based mailbox scheduling.
///
/// Adapter crates that implement a priority mailbox can use this trait
/// to customise how priorities are compared. The core crate provides
/// [`StrictPriorityComparer`] as the default implementation.
pub trait MessageComparer: Send + Sync + 'static {
    /// Compare two priority values.
    ///
    /// The returned [`Ordering`] is from the perspective of `a`:
    /// - `Less` means `a` has higher priority (should be dequeued first).
    /// - `Greater` means `b` has higher priority.
    /// - `Equal` means they have the same priority (FIFO within tier).
    fn compare(&self, a_priority: u8, b_priority: u8) -> Ordering;
}

/// Default priority comparer: lower numeric value = higher priority.
///
/// This matches the [`Priority`](crate::message::Priority) constants where
/// `CRITICAL = 0` and `BACKGROUND = 255`.
pub struct StrictPriorityComparer;

impl MessageComparer for StrictPriorityComparer {
    fn compare(&self, a_priority: u8, b_priority: u8) -> Ordering {
        a_priority.cmp(&b_priority)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_comparer_lower_value_is_higher_priority() {
        let cmp = StrictPriorityComparer;
        assert_eq!(cmp.compare(0, 128), Ordering::Less);
        assert_eq!(cmp.compare(128, 0), Ordering::Greater);
        assert_eq!(cmp.compare(64, 64), Ordering::Equal);
    }

    #[test]
    fn strict_comparer_priority_constants() {
        use crate::message::Priority;
        let cmp = StrictPriorityComparer;
        assert_eq!(cmp.compare(Priority::CRITICAL.0, Priority::HIGH.0), Ordering::Less);
        assert_eq!(cmp.compare(Priority::HIGH.0, Priority::NORMAL.0), Ordering::Less);
        assert_eq!(cmp.compare(Priority::NORMAL.0, Priority::LOW.0), Ordering::Less);
        assert_eq!(cmp.compare(Priority::LOW.0, Priority::BACKGROUND.0), Ordering::Less);
    }

    #[test]
    fn custom_comparer() {
        /// Inverted comparer: higher numeric value = higher priority.
        struct InvertedComparer;
        impl MessageComparer for InvertedComparer {
            fn compare(&self, a_priority: u8, b_priority: u8) -> Ordering {
                b_priority.cmp(&a_priority)
            }
        }

        let cmp = InvertedComparer;
        assert_eq!(cmp.compare(0, 128), Ordering::Greater);
        assert_eq!(cmp.compare(255, 0), Ordering::Less);
    }
}
