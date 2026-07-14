use std::collections::VecDeque;

use crate::agent::submission::{Submission, SubmissionParser};
use crate::channels::IncomingMessage;

pub(super) const DEFERRED_MESSAGE_LIMIT: usize = 256;
pub(super) const DEFERRED_QUEUE_FULL_RESPONSE: &str = "Agent is busy and its deferred message queue is full. Try again after the active turn finishes.";

pub(super) fn is_priority_interrupt(message: &IncomingMessage) -> bool {
    matches!(
        SubmissionParser::parse(&message.content),
        Submission::Interrupt
    )
}

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

    #[cfg(test)]
    fn with_capacity_for_test(capacity: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            capacity,
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

    use super::{DeferredMessages, is_priority_interrupt};

    fn message(content: &str) -> IncomingMessage {
        IncomingMessage::new("gateway", "user", content)
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
    fn deferred_messages_are_fifo_and_bounded() {
        let mut queue = DeferredMessages::with_capacity_for_test(2);
        assert!(queue.defer(message("first")).is_none());
        assert!(queue.defer(message("second")).is_none());
        let rejected = queue.defer(message("third")).expect("queue is full");
        assert_eq!(rejected.content, "third");
        assert_eq!(
            queue.pop_front().map(|message| message.content),
            Some("first".into())
        );
        assert_eq!(
            queue.pop_front().map(|message| message.content),
            Some("second".into())
        );
    }
}
