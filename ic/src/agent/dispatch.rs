use std::collections::{HashSet, VecDeque};

use crate::agent::submission::{Submission, SubmissionParser};
use crate::channels::IncomingMessage;

pub(super) const DEFERRED_MESSAGE_LIMIT: usize = 256;
pub(super) const MAX_PARALLEL_GATEWAY_TURNS: usize = 4;
pub(super) const DEFERRED_QUEUE_FULL_RESPONSE: &str = "Agent is busy and its deferred message queue is full. Try again after the active turn finishes.";

/// Submissions that must be handled immediately rather than deferred behind
/// the message scheduler: interrupts (abort the running turn) and steer notes
/// (must reach the running turn, so they are useless if deferred behind it).
pub(super) fn is_priority_control(message: &IncomingMessage) -> bool {
    matches!(
        SubmissionParser::parse(&message.content),
        Submission::Interrupt | Submission::Steer { .. }
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

/// Identity of a user's conversation lane.
///
/// A priority control cancels only queued turns for the same channel, user,
/// and conversation scope: a different thread, room, or sender is never
/// affected by an interrupt the user aimed at one lane.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct LaneKey {
    channel: String,
    user_id: String,
    scope: Option<String>,
}

impl LaneKey {
    pub(super) fn from_message(message: &IncomingMessage) -> Self {
        Self {
            channel: message.channel.clone(),
            user_id: message.user_id.clone(),
            scope: message.conversation_scope().map(str::to_string),
        }
    }
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

    /// Cancel queued messages in one conversation lane.
    ///
    /// Used by priority controls: an interrupt that overtakes a queued prompt
    /// must stop that prompt from running afterward, or the user gets
    /// "Nothing to interrupt." followed by the very turn they cancelled.
    /// Returns the number of queued messages removed.
    pub(super) fn cancel_lane(&mut self, lane: &LaneKey) -> usize {
        let before = self.queue.len();
        self.queue
            .retain(|queued| LaneKey::from_message(&queued.message) != *lane);
        before - self.queue.len()
    }

    pub(super) fn has_active(&self) -> bool {
        self.exclusive_active || !self.active_parallel.is_empty()
    }

    pub(super) fn is_drained(&self) -> bool {
        self.queue.is_empty() && !self.has_active()
    }
}

#[cfg(test)]
mod tests {
    use crate::channels::IncomingMessage;

    use super::{LaneKey, MessageScheduler, is_priority_control};

    fn message(content: &str) -> IncomingMessage {
        IncomingMessage::new("gateway", "user", content)
    }

    fn thread_message(user_id: &str, thread_id: &str, content: &str) -> IncomingMessage {
        IncomingMessage::new("gateway", user_id, content).with_thread(thread_id)
    }

    #[test]
    fn only_exact_interrupt_submissions_are_priority() {
        assert!(is_priority_control(&message("/interrupt")));
        assert!(is_priority_control(&message("/stop")));
        assert!(is_priority_control(&message(" /STOP ")));
        assert!(!is_priority_control(&message("please interrupt this")));
        assert!(!is_priority_control(&message("please /stop after this")));
        assert!(!is_priority_control(&message("/interrupt later")));
        assert!(!is_priority_control(&message("/stop now")));
        assert!(!is_priority_control(&message("/clear")));
    }

    #[test]
    fn steer_submissions_are_priority() {
        // A steer note deferred behind the running turn would arrive after the
        // turn ended and find nothing to steer — it must jump the queue.
        assert!(is_priority_control(&message("/steer use the other API")));
        assert!(is_priority_control(&message(" /STEER refocus ")));
        assert!(!is_priority_control(&message("/steer")));
        assert!(!is_priority_control(&message("/steering the topic")));
        assert!(!is_priority_control(&message("please steer left")));
    }

    #[test]
    fn cancel_lane_removes_only_the_matching_conversation_lane() {
        let mut scheduler = MessageScheduler::with_limits_for_test(8, 1);
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
                .defer(thread_message("other", "one", "other-user"), true)
                .is_none()
        );

        // Interrupt aimed at thread "one" for user "user": cancels only that
        // lane, leaving thread "two" and the other user's turn untouched.
        let lane = LaneKey::from_message(&thread_message("user", "one", "/interrupt"));
        assert_eq!(scheduler.cancel_lane(&lane), 1);

        let first = scheduler.next_ready().expect("thread two is ready");
        assert_eq!(first.message.content, "second");
        scheduler.complete(&first.ticket);

        let second = scheduler.next_ready().expect("other user is ready");
        assert_eq!(second.message.content, "other-user");
        scheduler.complete(&second.ticket);

        assert!(scheduler.is_drained());
    }

    #[test]
    fn cancel_lane_without_scope_matches_unscoped_messages_only() {
        let mut scheduler = MessageScheduler::with_limits_for_test(4, 1);
        assert!(scheduler.defer(message("plain"), false).is_none());
        assert!(
            scheduler
                .defer(thread_message("user", "one", "scoped"), true)
                .is_none()
        );

        // An unscoped interrupt (no thread id) cancels only the unscoped
        // queued message.
        let lane = LaneKey::from_message(&message("/interrupt"));
        assert_eq!(scheduler.cancel_lane(&lane), 1);

        let next = scheduler.next_ready().expect("scoped message remains");
        assert_eq!(next.message.content, "scoped");
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
