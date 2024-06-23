use super::{Access, AllowPendOp, Interruptable, RefCellSchedSafe, RunPendedOp, Spin};
use crate::{interrupt::svc, schedule, task::Task, time, unrecoverable};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// A synchronization primitive that allows a task to wait for a notification
/// until timeout. [`Mailbox`] allows synchronization between tasks or between
/// a task and interrupt handlers.
///
/// However, unlike the [`Semaphore`](super::Semaphore), a [`Mailbox`] allows
/// *only one* task to wait on it. A task will panic if it tries to
/// [`wait`](Mailbox::wait) on a [`Mailbox`] that already has a waiting task on
/// it. Such restriction enables the [`Mailbox`] to provide the
/// [`wait_until_timeout`](Mailbox::wait_until_timeout) method.
/// It is still allowed to have multiple tasks or interrupt handlers to notify
/// on the same [`Mailbox`].
///
/// Like the [`Semaphore`](super::Semaphore), a [`Mailbox`] counts the number
/// of received notifications. For example, if an interrupt handler notifies
/// on a [`Mailbox`] before the task waits on the [`Mailbox`], an internal
/// counter will be incremented to record the notification. Later when the task
/// [`wait`](Mailbox::wait) on the [`Mailbox`], the task decrements the counter
/// and returns immediately. A task blocks on [`wait`](Mailbox::wait) only when
/// the notification counter is zero.
pub struct Mailbox {
    inner: RefCellSchedSafe<Interruptable<Inner>>,
}

struct Inner {
    /// The number of notifications posted but not yet received by the waiting
    /// task.
    count: AtomicUsize,
    /// When the [`Mailbox`] is under contention, new notifications will first
    /// post to this field, and will later be moved to [`count`](Self::count).
    pending_count: AtomicUsize,
    /// The task waiting on this [`Mailbox`]. The spin lock around it is only
    /// for sanity check. This field should not be accessed concurrently.
    wait_task: Spin<Option<Arc<Task>>>,
    /// Whether the [`notify_allow_isr`](Mailbox::notify_allow_isr) has been
    /// invoked. This is used to distinguish between waking up a task by
    /// notification and by timeout.
    task_notified: AtomicBool,
}

/// Representing full access to all fields of the [`Mailbox`].
struct InnerFullAccessor<'a> {
    count: &'a AtomicUsize,
    pending_count: &'a AtomicUsize,
    wait_task: &'a Spin<Option<Arc<Task>>>,
    task_notified: &'a AtomicBool,
}

/// Representing pend-only access to the [`Mailbox`]. Only the fields that
/// expect concurrent access are granted by this accessor.
struct InnerPendAccessor<'a> {
    pending_count: &'a AtomicUsize,
}

/// Bind the accessor types.
impl<'a> AllowPendOp<'a> for Inner {
    type FullAccessor = InnerFullAccessor<'a>;
    type PendOnlyAccessor = InnerPendAccessor<'a>;
    fn full_access(&'a self) -> Self::FullAccessor {
        Self::FullAccessor {
            count: &self.count,
            pending_count: &self.pending_count,
            wait_task: &self.wait_task,
            task_notified: &self.task_notified,
        }
    }

    fn pend_only_access(&'a self) -> Self::PendOnlyAccessor {
        Self::PendOnlyAccessor {
            pending_count: &self.pending_count,
        }
    }
}

/// Resolve contention of multiple concurrent access.
impl<'a> RunPendedOp for InnerFullAccessor<'a> {
    fn run_pended_op(&mut self) {
        // Move the values from `pending_count` to `count`.
        //
        // NOTE: Since `pending_count` allows concurrent access, it is possible
        // that when we are executing this function another preempting
        // interrupt handler again increments the `pending_count`. We must use
        // the `swap` method to read the current value and reset it to 0
        // atomically. In contrast, using a separate `load` and then `store`
        // will lead to race condition if an interrupt handler preempts in
        // between.
        let pending_count = self.pending_count.swap(0, Ordering::SeqCst);
        self.count.fetch_add(pending_count, Ordering::SeqCst);

        // When `run_pended_op` is invoked, a pend-only accessor must have been
        // previously granted, and thus the `pending_count` must have been
        // incremented to be greater than zero. (See `notify_allow_isr`.) It
        // follows that now `count` must also be greater than zero. Thus, as
        // long as there is a waiting task, we should notify it.
        if let Some(wait_task) = self.wait_task.lock_now_or_die().take() {
            time::remove_task_from_sleep_queue_allow_isr(wait_task);
            self.count.fetch_sub(1, Ordering::SeqCst);
            self.task_notified.store(true, Ordering::SeqCst);
        }
    }
}

impl Inner {
    const fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
            pending_count: AtomicUsize::new(0),
            wait_task: Spin::new(None),
            task_notified: AtomicBool::new(false),
        }
    }
}

impl Mailbox {
    /// Create a new [`Mailbox`] with the notification counter initialized to
    /// zero.
    pub const fn new() -> Self {
        Self {
            inner: RefCellSchedSafe::new(Interruptable::new(Inner::new())),
        }
    }

    /// Block the calling task if the notification counter is currently zero.
    /// The blocking task will be woken up if other tasks or ISRs notify on the
    /// mailbox.
    ///
    /// Otherwise, if the counter is currently positive, the calling task to
    /// this method decrements the counter and continues its execution.
    ///
    /// NOTE: *must not* call this method in ISR context.
    pub fn wait(&self) {
        // Just wait with a very large timeout. This has very little overhead
        // on scheduling. Continue to wait until the task is woken up by a
        // notification rather than a timeout.
        while !self.wait_until_timeout(100_000_000) {}
    }

    /// Block the calling task if the notification counter is currently zero.
    /// The blocking task will be woken up if other tasks or ISRs notify on the
    /// mailbox or if the elapsed waiting time reaches timeout.
    ///
    /// Otherwise, if the counter is currently positive, the calling task to
    /// this method decrements the counter and continues its execution. In this
    /// case the calling task is considered to be notified.
    ///
    /// Arguments:
    /// - `timeout_ms`: Waiting timeout in milliseconds.
    ///
    /// Return:
    /// - `true` if the waiting task is woken up by notification, or `false` if
    ///   by timeout.
    ///
    /// NOTE: *must not* call this method in ISR context.
    pub fn wait_until_timeout(&self, timeout_ms: u32) -> bool {
        unrecoverable::die_if_in_isr();

        let mut should_block = true;

        // Suspend scheduling and acquire full access to the mailbox fields.
        self.inner.lock().must_with_full_access(|full_access| {
            let mut locked_wait_task = full_access.wait_task.lock_now_or_die();

            // A sanity check to prevent more than one task to try to wait on
            // the same mailbox.
            assert!(locked_wait_task.is_none());

            // If the counter is currently positive, decrement the counter and
            // do not block.
            if full_access.count.load(Ordering::SeqCst) > 0 {
                full_access.count.fetch_sub(1, Ordering::SeqCst);
                should_block = false;
                return;
            }

            // Otherwise the task is going to be blocked. Reset the flag.
            full_access.task_notified.store(false, Ordering::SeqCst);

            schedule::with_current_task_arc(|cur_task| {
                schedule::set_task_state_block(&cur_task);

                // Record the waiting task on this mailbox.
                *locked_wait_task = Some(Arc::clone(&cur_task));

                // Add the waiting task to the sleeping queue.
                // FIXME: This assumes 1ms tick interval.
                let wake_at_tick = time::get_tick() + timeout_ms;
                time::add_task_to_sleep_queue(cur_task, wake_at_tick);
            });
        });

        if should_block {
            // If the task should block, request a context switch.
            svc::svc_block_current_task();

            // We reach here if either the waiting task is notified or the
            // waiting time reaches timeout.

            // Suspend scheduling and acquire full access to the mailbox fields.
            self.inner.lock().must_with_full_access(|full_access| {
                // Clear the waiting task field. This field was not cleared if
                // the task wakes up because of the timeout.
                full_access.wait_task.lock_now_or_die().take();

                // Return whether the task wakes up because of notification.
                full_access.task_notified.load(Ordering::SeqCst)
            })
        } else {
            // If the task need not block, it consumed a notification count and
            // is considered to be notified.
            true
        }
    }

    /// Make the waiting task ready to run if there is a waiting task on the
    /// [`Mailbox`], or otherwise increment the counter if there is not current
    /// waiting task.
    ///
    /// This method is allowed in ISR context.
    pub fn notify_allow_isr(&self) {
        // Suspend scheduling and get access to the mailbox fields.
        self.inner.lock().with_access(|access| match access {
            // If we have full access to the inner fields, we directly wake up
            // the waiting task or increment the counter.
            Access::Full { full_access } => match full_access.wait_task.lock_now_or_die().take() {
                // If there is a waiting task, wake it up.
                Some(wait_task) => {
                    time::remove_task_from_sleep_queue_allow_isr(wait_task);
                    full_access.task_notified.store(true, Ordering::SeqCst);
                }
                // If there is not a waiting task, increment the counter.
                None => {
                    full_access.count.fetch_add(1, Ordering::SeqCst);
                    full_access.task_notified.store(true, Ordering::SeqCst);
                }
            },
            // If other context is running with the full access and we preempt
            // it, we get pend-only access. We increment the `pending_count` so
            // that the full access owner can later help us update the counter
            // or notify the waiting task on behalf.
            Access::PendOnly { pend_access } => {
                pend_access.pending_count.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
}
