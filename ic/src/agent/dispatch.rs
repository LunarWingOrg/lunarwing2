use std::collections::{HashSet, VecDeque};

use crate::agent::submission::{Submission, SubmissionParser};
use crate::channels::IncomingMessage;

pub(super) const DEFERRED_MESSAGE_LIMIT: usize = 256;
pub(super) const MAX_PARALLEL_GATEWAY_TURNS: usize = 4;
pub(super) const DEFERRED_QUEUE_FULL_RESPONSE: &str = "Agent is busy and its deferred message queue is full. Try again after the active turn finishes.";

pub(super) fn is_priority_interrupt(message: &IncomingMessage) -> bool {
    matches!(
        SubmissionParser::parse(&message.content),
        Submission::Interrupt
    )
}

/// Only isolated Engine V2 gateway threads are safe to run in parallel.
///
/// Other channels can carry routing state that is shared by their adapters,
/// and control submissions must retain the globally serialized ordering
/// contract. Gateway requests have a mandatory thread ID, so distinct threads
/// for one authenticated user form independent scheduling lanes.
pub(super) fn allows_parallel_dispatch(message: &IncomingMessage) -> bool {
    message.channel == "gateway"
        && message.conversation_scope().is_some()
        && crate::bridge::should_route_to_engine_v2(&message.channel)
        && matches!(
            SubmissionParser::parse(&message.content),
            Submission::UserInput { .. }
        )
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ParallelMessageGroup {
    channel: String,
    user_id: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ParallelMessageKey {
    group: ParallelMessageGroup,
    scope: String,
}

impl ParallelMessageKey {
    fn from_message(message: &IncomingMessage) -> Option<Self> {
        Some(Self {
            group: ParallelMessageGroup {
                channel: message.channel.clone(),
                user_id: message.user_id.clone(),
            },
            scope: message.conversation_scope()?.to_string(),
        })
    }
}

struct QueuedMessage {
    message: IncomingMessage,
    parallel_key: Option<ParallelMessageKey>,
}

impl QueuedMessage {
    fn new(message: IncomingMessage, allow_parallel: bool) -> Self {
        let parallel_key = allow_parallel
            .then(|| ParallelMessageKey::from_message(&message))
            .flatten();
        Self {
            message,
            parallel_key,
        }
    }
}

#[derive(Debug)]
pub(super) struct DispatchTicket {
    parallel_key: Option<ParallelMessageKey>,
}

pub(super) struct ScheduledMessage {
    pub(super) message: IncomingMessage,
    pub(super) ticket: DispatchTicket,
}

/// Bounded scheduler for ordinary messages.
///
/// Messages remain FIFO except that a queued message for an already-active
/// gateway thread does not block a later independent thread from the same
/// authenticated user. Different users and exclusive messages are fairness
/// barriers and are never overtaken.
pub(super) struct MessageScheduler {
    queue: VecDeque<QueuedMessage>,
    active_parallel: HashSet<ParallelMessageKey>,
    active_group: Option<ParallelMessageGroup>,
    exclusive_active: bool,
    capacity: usize,
    max_parallel: usize,
}

impl MessageScheduler {
    pub(super) fn new() -> Self {
        Self::with_limits(DEFERRED_MESSAGE_LIMIT, MAX_PARALLEL_GATEWAY_TURNS)
    }

    fn with_limits(capacity: usize, max_parallel: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            active_parallel: HashSet::new(),
            active_group: None,
            exclusive_active: false,
            capacity,
            max_parallel: max_parallel.max(1),
        }
    }

    #[cfg(test)]
    fn with_limits_for_test(capacity: usize, max_parallel: usize) -> Self {
        Self::with_limits(capacity, max_parallel)
    }

    pub(super) fn defer(
        &mut self,
        message: IncomingMessage,
        allow_parallel: bool,
    ) -> Option<IncomingMessage> {
        if self.queue.len() >= self.capacity {
            return Some(message);
        }
        self.queue
            .push_back(QueuedMessage::new(message, allow_parallel));
        None
    }

    pub(super) fn next_ready(&mut self) -> Option<ScheduledMessage> {
        if self.exclusive_active {
            return None;
        }
        if self.active_parallel.is_empty() {
            let queued = self.queue.pop_front()?;
            return Some(self.reserve(queued));
        }
        if self.active_parallel.len() >= self.max_parallel {
            return None;
        }

        let active_group = self.active_group.as_ref()?;
        let mut ready_position = None;
        for (position, queued) in self.queue.iter().enumerate() {
            let Some(key) = queued.parallel_key.as_ref() else {
                break;
            };
            if &key.group != active_group {
                break;
            }
            if !self.active_parallel.contains(key) {
                ready_position = Some(position);
                break;
            }
        }

        let queued = self.queue.remove(ready_position?)?;
        Some(self.reserve(queued))
    }

    fn reserve(&mut self, queued: QueuedMessage) -> ScheduledMessage {
        if let Some(key) = queued.parallel_key.as_ref() {
            self.active_group.get_or_insert_with(|| key.group.clone());
            self.active_parallel.insert(key.clone());
        } else {
            self.exclusive_active = true;
        }

        ScheduledMessage {
            message: queued.message,
            ticket: DispatchTicket {
                parallel_key: queued.parallel_key,
            },
        }
    }

    pub(super) fn complete(&mut self, ticket: &DispatchTicket) {
        if let Some(key) = ticket.parallel_key.as_ref() {
            self.active_parallel.remove(key);
            if self.active_parallel.is_empty() {
                self.active_group = None;
            }
        } else {
            self.exclusive_active = false;
        }
    }

    pub(super) fn has_active(&self) -> bool {
        self.exclusive_active || !self.active_parallel.is_empty()
    }

    pub(super) fn is_drained(&self) -> bool {
        self.queue.is_empty() && !self.has_active()
    }
}

// Temporary compatibility adapter while Agent::run is migrated to
// MessageScheduler in the following incremental change.
pub(super) struct DeferredMessages {
    queue: VecDeque<IncomingMessage>,
    capacity: usize,
}

impl DeferredMessages {
    pub(super) fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            capacity: DEFERRED_MESSAGE_LIMIT,
        }
    }

    pub(super) fn defer(&mut self, message: IncomingMessage) -> Option<IncomingMessage> {
        if self.queue.len() >= self.capacity {
            return Some(message);
        }
        self.queue.push_back(message);
        None
    }

    pub(super) fn pop_front(&mut self) -> Option<IncomingMessage> {
        self.queue.pop_front()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use crate::channels::IncomingMessage;

    use super::{MessageScheduler, is_priority_interrupt};

    fn message(content: &str) -> IncomingMessage {
        IncomingMessage::new("gateway", "user", content)
    }

    fn thread_message(user_id: &str, thread_id: &str, content: &str) -> IncomingMessage {
        IncomingMessage::new("gateway", user_id, content).with_thread(thread_id)
    }

    #[test]
    fn only_exact_interrupt_submissions_are_priority() {
        assert!(is_priority_interrupt(&message("/interrupt")));
        assert!(is_priority_interrupt(&message("/stop")));
        assert!(is_priority_interrupt(&message(" /STOP ")));
        assert!(!is_priority_interrupt(&message("please interrupt this")));
        assert!(!is_priority_interrupt(&message("please /stop after this")));
        assert!(!is_priority_interrupt(&message("/interrupt later")));
        assert!(!is_priority_interrupt(&message("/stop now")));
        assert!(!is_priority_interrupt(&message("/clear")));
    }

    #[test]
    fn distinct_threads_for_one_gateway_user_run_in_parallel() {
        let mut scheduler = MessageScheduler::with_limits_for_test(4, 2);
        assert!(
            scheduler
                .defer(thread_message("user", "one", "first"), true)
                .is_none()
        );
        assert!(
            scheduler
                .defer(thread_message("user", "two", "second"), true)
                .is_none()
        );
        assert!(
            scheduler
                .defer(thread_message("user", "three", "third"), true)
                .is_none()
        );

        let first = scheduler.next_ready().expect("first thread is ready");
        let second = scheduler.next_ready().expect("second thread is ready");
        assert_eq!(first.message.content, "first");
        assert_eq!(second.message.content, "second");
        assert!(scheduler.next_ready().is_none());

        scheduler.complete(&first.ticket);
        let third = scheduler
            .next_ready()
            .expect("slot released for third thread");
        assert_eq!(third.message.content, "third");
        scheduler.complete(&second.ticket);
        scheduler.complete(&third.ticket);
        assert!(scheduler.is_drained());
    }

    #[test]
    fn same_gateway_thread_remains_serialized() {
        let mut scheduler = MessageScheduler::with_limits_for_test(4, 2);
        assert!(
            scheduler
                .defer(thread_message("user", "one", "first"), true)
                .is_none()
        );
        assert!(
            scheduler
                .defer(thread_message("user", "one", "second"), true)
                .is_none()
        );

        let first = scheduler.next_ready().expect("first message is ready");
        assert!(scheduler.next_ready().is_none());
        scheduler.complete(&first.ticket);

        let second = scheduler.next_ready().expect("follow-up is now ready");
        assert_eq!(second.message.content, "second");
        scheduler.complete(&second.ticket);
        assert!(scheduler.is_drained());
    }

    #[test]
    fn different_user_is_a_fairness_barrier() {
        let mut scheduler = MessageScheduler::with_limits_for_test(4, 2);
        assert!(
            scheduler
                .defer(thread_message("alice", "one", "alice-first"), true)
                .is_none()
        );
        assert!(
            scheduler
                .defer(thread_message("bob", "two", "bob"), true)
                .is_none()
        );
        assert!(
            scheduler
                .defer(thread_message("alice", "three", "alice-later"), true)
                .is_none()
        );

        let alice = scheduler.next_ready().expect("alice starts");
        assert!(scheduler.next_ready().is_none());
        scheduler.complete(&alice.ticket);

        let bob = scheduler.next_ready().expect("bob starts next");
        assert_eq!(bob.message.content, "bob");
        assert!(scheduler.next_ready().is_none());
        scheduler.complete(&bob.ticket);

        let alice_later = scheduler
            .next_ready()
            .expect("later alice turn starts last");
        assert_eq!(alice_later.message.content, "alice-later");
    }

    #[test]
    fn exclusive_message_is_not_overtaken() {
        let mut scheduler = MessageScheduler::with_limits_for_test(4, 2);
        assert!(
            scheduler
                .defer(thread_message("user", "one", "first"), true)
                .is_none()
        );
        assert!(scheduler.defer(message("/clear"), false).is_none());
        assert!(
            scheduler
                .defer(thread_message("user", "two", "later"), true)
                .is_none()
        );

        let first = scheduler.next_ready().expect("parallel turn starts");
        assert!(scheduler.next_ready().is_none());
        scheduler.complete(&first.ticket);

        let control = scheduler.next_ready().expect("exclusive control starts");
        assert_eq!(control.message.content, "/clear");
        assert!(scheduler.next_ready().is_none());
        scheduler.complete(&control.ticket);

        let later = scheduler.next_ready().expect("later parallel turn starts");
        assert_eq!(later.message.content, "later");
    }

    #[test]
    fn deferred_messages_are_fifo_and_bounded() {
        let mut scheduler = MessageScheduler::with_limits_for_test(2, 1);
        assert!(scheduler.defer(message("first"), false).is_none());
        assert!(scheduler.defer(message("second"), false).is_none());
        let rejected = scheduler
            .defer(message("third"), false)
            .expect("queue is full");
        assert_eq!(rejected.content, "third");

        let first = scheduler.next_ready().expect("first message is ready");
        assert_eq!(first.message.content, "first");
        assert!(scheduler.next_ready().is_none());
        scheduler.complete(&first.ticket);

        let second = scheduler.next_ready().expect("second message is ready");
        assert_eq!(second.message.content, "second");
    }
}
