use parking_lot::Mutex;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

/// A shareable cancellation signal.  Any holder can cancel; all holders see it.
#[derive(Clone, Debug)]
pub struct CancellationToken(Arc<CancellationState>);

#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
    children: Mutex<Vec<Weak<CancellationState>>>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self(Arc::new(CancellationState {
            cancelled: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
            children: Mutex::new(Vec::new()),
        }))
    }

    /// Create a request-local child. Parent cancellation propagates down;
    /// cancelling a child never cancels its parent or siblings.
    pub fn child_token(&self) -> Self {
        let child = Self::new();
        child.cancel_with(self);
        child
    }

    /// Also cancel this token when `parent` is cancelled. Local cancellation
    /// remains isolated from the parent.
    pub fn cancel_with(&self, parent: &Self) {
        if Arc::ptr_eq(&self.0, &parent.0) {
            return;
        }
        let mut children = parent.0.children.lock();
        if parent.is_cancelled() {
            self.cancel();
        } else {
            children.retain(|child| child.strong_count() != 0);
            let child = Arc::downgrade(&self.0);
            if !children.iter().any(|existing| existing.ptr_eq(&child)) {
                children.push(child);
            }
        }
    }

    /// Signal cancellation.
    pub fn cancel(&self) {
        if !self.0.cancelled.swap(true, Ordering::AcqRel) {
            self.0.notify.notify_waiters();
            for child in self
                .0
                .children
                .lock()
                .drain(..)
                .filter_map(|child| child.upgrade())
            {
                Self(child).cancel();
            }
        }
    }

    /// Returns `true` if cancellation has been signalled.
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
            if self.is_cancelled() {
                return;
            }
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::CancellationToken;

    #[tokio::test]
    async fn parent_cancels_children_without_child_cancelling_siblings() {
        let instance = CancellationToken::new();
        let first = instance.child_token();
        let second = instance.child_token();
        first.cancel();
        assert!(!instance.is_cancelled());
        assert!(!second.is_cancelled());
        instance.cancel();
        second.cancelled().await;
        assert!(second.is_cancelled());
        assert!(instance.child_token().is_cancelled());
    }
}
