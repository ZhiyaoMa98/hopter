use super::{
    Access, AllowPendOp, Interruptable, Lockable, RefCellSchedSafe, RunPendedOp, Spin,
    UnlockableGuard,
};
use crate::{
    interrupt::svc,
    schedule,
    task::{TaskListAdapter, TaskListInterfaces},
    unrecoverable,
};
use core::sync::atomic::{AtomicUsize, Ordering};
use intrusive_collections::LinkedList;

/// Queue for blocked tasks waiting for notification.
///
/// FIXME: what should happen when a wait queue gets dropped?
pub struct WaitQueue {
    inner: RefCellSchedSafe<Interruptable<Inner>>,
}

/// The inner content of a wait queue.
struct Inner {
    /// A buffer containing reference counted task pointers. The buffer will
    /// be sorted based on task priority when popping out tasks. The spin lock
    /// around it is only for sanity check.
    queue: Spin<LinkedList<TaskListAdapter>>,
    /// When an ISR is trying to dequeue a task when the queue is already locked,
    /// it increments the notification counter, so that the lock holder can later
    /// dequeue the task on behalf of the ISR.
    notify_cnt: AtomicUsize,
}

/// Representing full access to the queue.
struct InnerFullAccessor<'a> {
    queue: &'a Spin<LinkedList<TaskListAdapter>>,
    notify_cnt: &'a AtomicUsize,
}

/// Representing pend-only access to the queue. Using this accessor one can only
/// increment the notification counter indicating that one more task needs to be
/// notified.
struct InnerPendAccessor<'a> {
    notify_cnt: &'a AtomicUsize,
}

/// Bind the accessor types.
impl<'a> AllowPendOp<'a> for Inner {
    type FullAccessor = InnerFullAccessor<'a>;
    type PendOnlyAccessor = InnerPendAccessor<'a>;
    fn full_access(&'a self) -> InnerFullAccessor<'a> {
        InnerFullAccessor {
            queue: &self.queue,
            notify_cnt: &self.notify_cnt,
        }
    }
    fn pend_only_access(&'a self) -> InnerPendAccessor<'a> {
        InnerPendAccessor {
            notify_cnt: &self.notify_cnt,
        }
    }
}

/// If the notification counter is non-zero, we should notify tasks as many times
/// as indicated by the counter.
impl<'a> RunPendedOp for InnerFullAccessor<'a> {
    fn run_pended_op(&mut self) {
        let mut locked_queue = self.queue.lock_now_or_die();
        let cnt = self.notify_cnt.swap(0, Ordering::SeqCst);
        for _ in 0..cnt {
            if let Some(task) = locked_queue.pop_highest_priority() {
                schedule::make_task_ready_and_enqueue(task);
            } else {
                break;
            }
        }
    }
}

impl Inner {
    const fn new() -> Self {
        Self {
            queue: Spin::new(LinkedList::new(TaskListAdapter::NEW)),
            notify_cnt: AtomicUsize::new(0),
        }
    }
}

impl WaitQueue {
    /// Create a new empty wait queue.
    pub const fn new() -> Self {
        Self {
            inner: RefCellSchedSafe::new(Interruptable::<Inner>::new(Inner::new())),
        }
    }

    /// Put the current task into the queue and block it. Wait until some other
    /// task notifies it.
    ///
    /// Important: *must not* call this method in ISR context.
    #[allow(dead_code)]
    #[inline]
    pub fn wait(&self) {
        unrecoverable::die_if_in_isr();

        add_cur_task_to_block_queue(self);

        // We have put the current task to the wait queue.
        // Tell the scheduler to run another task.
        svc::svc_block_current_task();

        // Outline the logic to reduce the stack frame size of `.wait()`.
        #[inline(never)]
        fn add_cur_task_to_block_queue(wq: &WaitQueue) {
            // Should always grant full access to a task.
            wq.inner.lock().must_with_full_access(|full_access| {
                // Put the current task into the queue.
                schedule::with_current_task_arc(|cur_task| {
                    schedule::set_task_state_block(&cur_task);
                    let mut locked_queue = full_access.queue.lock_now_or_die();
                    locked_queue.push_back(cur_task);
                });
            });
        }
    }

    /// Put the current task into the queue and block it. Wait until some other
    /// task notifies it and the condition is also met. When the condition is met,
    /// the `condition` predicate function should return `Some`, otherwise `None`.
    /// When the predicate function returns `Some`, the task resumes and gets
    /// the returned value contained in `Some`.
    ///
    /// Important: *must not* call this method in ISR context.
    ///
    /// Important: the tasks waiting for nofitication are ordered with respect to
    /// their priorities. If the highest priority task gets notified but does not
    /// have its condition met, it will be placed back to the queue. No other task
    /// will be in turn notified, i.e., the notification is treated as spurious and
    /// is discarded.
    #[inline]
    pub fn wait_until<F, R>(&self, mut condition: F) -> R
    where
        F: FnMut() -> Option<R>,
    {
        unrecoverable::die_if_in_isr();

        // Keep blocking until the predicate is satisfied.
        loop {
            let res = add_cur_task_to_block_queue_with_condition(self, &mut condition);

            // If the condition is met, we return.
            if let Some(ret) = res {
                return ret;
            }

            // Otherwise, we have put the current task to the wait queue.
            // Tell the scheduler to run another task.
            svc::svc_block_current_task();
        }

        // Outline the logic to reduce the stack frame size of `.wait_until()`.
        #[inline(never)]
        fn add_cur_task_to_block_queue_with_condition<F, R>(
            wq: &WaitQueue,
            condition: &mut F,
        ) -> Option<R>
        where
            F: FnMut() -> Option<R>,
        {
            // Should always grant full access to a task.
            wq.inner.lock().must_with_full_access(|full_access| {
                // Must lock the queue here before evaluating the condition to
                // prevent deadlock.
                let mut locked_queue = full_access.queue.lock_now_or_die();

                // Check if the predicate is satisfied and if yes return the value
                // contained in `Some`.
                if let Some(ret) = condition() {
                    return Some(ret);
                }

                // Otherwise, put the current task into the queue.
                schedule::with_current_task_arc(|cur_task| {
                    schedule::set_task_state_block(&cur_task);
                    locked_queue.push_back(cur_task);
                });

                None
            })
        }
    }

    /// Put the current task into the queue and block it. Wait until some other
    /// task notifies it and the condition is also met. When the condition is met,
    /// the `condition` predicate function should return `Some`, otherwise `None`.
    /// When the predicate function returns `Some`, the task resumes and gets
    /// the returned value contained in `Some`. The lock guard passed in will be
    /// released atomically when the task blocks and get locked again when the
    /// task resumes.
    ///
    /// Important: *must not* call this method in ISR context.
    ///
    /// Important: the tasks waiting for nofitication are ordered with respect to
    /// their priorities. If the highest priority task gets notified but does not
    /// have its condition met, it will be placed back to the queue. No other task
    /// will be in turn notified, i.e., the notification is treated as spurious and
    /// is discarded.
    #[inline]
    pub fn wait_until_with_lock<'a, F, G, R, L>(&self, mut guard: G, mut condition: F) -> (G, R)
    where
        F: FnMut(&mut G) -> Option<R>,
        G: UnlockableGuard<'a, LockType = L>,
        L: Lockable<GuardType<'a> = G> + 'a,
    {
        unrecoverable::die_if_in_isr();

        // Keep blocking until the predicate is satisfied.
        loop {
            let res = add_cur_task_to_block_queue_and_unlock(self, guard, &mut condition);

            match res {
                // If the condition is met, we return the lock guard and condition
                // function's return value.
                Err(ret) => return ret,
                // Otherwise, we have put the current task to the wait queue. Tell
                // the scheduler to run another task. After this task is scheduled
                // again, take back the lock and try again.
                Ok(mutex) => {
                    svc::svc_block_current_task();
                    guard = mutex.lock_and_get_guard();
                }
            }
        }

        // Outline the logic to reduce the stack frame size of `.wait_until_with_lock()`.
        #[inline(never)]
        fn add_cur_task_to_block_queue_and_unlock<'a, F, G, R, L>(
            wq: &WaitQueue,
            mut guard: G,
            condition: &mut F,
        ) -> Result<&'a L, (G, R)>
        where
            F: FnMut(&mut G) -> Option<R>,
            G: UnlockableGuard<'a, LockType = L>,
            L: Lockable<GuardType<'a> = G> + 'a,
        {
            // Should always grant full access to a task.
            wq.inner.lock().must_with_full_access(|full_access| {
                // Must lock the queue here before evaluating the condition and
                // releasing the `guard` passed in argument to prevent deadlock.
                let mut locked_queue = full_access.queue.lock_now_or_die();

                // Check if the predicate is satisfied and if yes return the value
                // contained in `Some` with the lock guard.
                if let Some(ret) = condition(&mut guard) {
                    return Err((guard, ret));
                }

                // Otherwise, release the lock guard and get the lock itself.
                let mutex = guard.unlock_and_into_lock_ref();

                // Put the current task into the queue.
                schedule::with_current_task_arc(|cur_task| {
                    schedule::set_task_state_block(&cur_task);
                    locked_queue.push_back(cur_task);
                });

                Ok(mutex)
            })
        }
    }

    /// Pop a task (if exists) from the queue and mark its state as ready.
    /// This method is allowed in ISR context. The popped the task is the one
    /// with the highest priority (smallest numerical value) in the queue.
    ///
    /// Important: the tasks waiting for nofitication are ordered with respect to
    /// their priorities. If the highest priority task gets notified but does not
    /// have its condition met, it will be placed back to the queue. No other task
    /// will be in turn notified, i.e., the notification is treated as spurious and
    /// is discarded.
    pub fn notify_one_allow_isr(&self) {
        self.inner.lock().with_access(|access| match access {
            // If we have full access to the inner components, we directly operate
            // on the queue to make the popped task ready.
            Access::Full { full_access } => {
                let mut locked_queue = full_access.queue.lock_now_or_die();
                if let Some(task) = locked_queue.pop_highest_priority() {
                    schedule::make_task_ready_and_enqueue(task);
                }
            }
            // If other context is running with the full access and we preempt it,
            // we get pend-only access. We increment the counter so that the full
            // access owner can later pop out the task on our behalf.
            Access::PendOnly { pend_access } => {
                pend_access.notify_cnt.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
}
