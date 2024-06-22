use super::{Access, AllowPendOp, Interruptable, RefCellSchedSafe, RunPendedOp, Spin};
use crate::{interrupt::svc, schedule, task::Task, time, unrecoverable};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub struct Mailbox {
    inner: RefCellSchedSafe<Interruptable<Inner>>,
}

struct Inner {
    count: AtomicUsize,
    pending_count: AtomicUsize,
    wait_task: Spin<Option<Arc<Task>>>,
    has_timeout: AtomicBool,
    task_notified: AtomicBool,
}

struct InnerFullAccessor<'a> {
    count: &'a AtomicUsize,
    pending_count: &'a AtomicUsize,
    wait_task: &'a Spin<Option<Arc<Task>>>,
    has_timeout: &'a AtomicBool,
    task_notified: &'a AtomicBool,
}

struct InnerPendAccessor<'a> {
    pending_count: &'a AtomicUsize,
}

impl<'a> AllowPendOp<'a> for Inner {
    type FullAccessor = InnerFullAccessor<'a>;
    type PendOnlyAccessor = InnerPendAccessor<'a>;
    fn full_access(&'a self) -> Self::FullAccessor {
        Self::FullAccessor {
            count: &self.count,
            pending_count: &self.pending_count,
            wait_task: &self.wait_task,
            has_timeout: &self.has_timeout,
            task_notified: &self.task_notified,
        }
    }

    fn pend_only_access(&'a self) -> Self::PendOnlyAccessor {
        Self::PendOnlyAccessor {
            pending_count: &self.pending_count,
        }
    }
}

impl<'a> RunPendedOp for InnerFullAccessor<'a> {
    fn run_pended_op(&mut self) {
        let pending_count = self.pending_count.swap(0, Ordering::SeqCst);
        self.count.fetch_add(pending_count, Ordering::SeqCst);

        if let Some(wait_task) = self.wait_task.lock_now_or_die().take() {
            let has_timeout = self.has_timeout.load(Ordering::SeqCst);
            if has_timeout {
                let removed = time::remove_task_from_sleep_queue(&wait_task);
                if removed {
                    schedule::make_task_ready_and_enqueue(wait_task);
                    self.count.fetch_sub(1, Ordering::SeqCst);
                    self.task_notified.store(true, Ordering::SeqCst);
                }
            } else {
                schedule::make_task_ready_and_enqueue(wait_task);
                self.count.fetch_sub(1, Ordering::SeqCst);
                self.task_notified.store(true, Ordering::SeqCst);
            }
        }
    }
}

impl Inner {
    const fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
            pending_count: AtomicUsize::new(0),
            wait_task: Spin::new(None),
            has_timeout: AtomicBool::new(false),
            task_notified: AtomicBool::new(false),
        }
    }
}

impl Mailbox {
    pub const fn new() -> Self {
        Self {
            inner: RefCellSchedSafe::new(Interruptable::new(Inner::new())),
        }
    }

    pub fn wait(&self) {
        die_if_in_isr();

        let should_block = self.inner.lock().must_with_full_access(|full_access| {
            if full_access.count.load(Ordering::SeqCst) > 0 {
                full_access.count.fetch_sub(1, Ordering::SeqCst);
                return false;
            }

            full_access.has_timeout.store(false, Ordering::SeqCst);
            full_access.task_notified.store(false, Ordering::SeqCst);

            schedule::with_current_task_arc(|cur_task| {
                schedule::set_task_state_block(&cur_task);
                let mut locked_wait_task = full_access.wait_task.lock_now_or_die();
                *locked_wait_task = Some(cur_task);
            });

            true
        });

        if should_block {
            svc::svc_block_current_task();
        }
    }

    pub fn wait_until_timeout(&self, timeout_ms: u32) -> bool {
        die_if_in_isr();

        let should_block = self.inner.lock().must_with_full_access(|full_access| {
            if full_access.count.load(Ordering::SeqCst) > 0 {
                full_access.count.fetch_sub(1, Ordering::SeqCst);
                return false;
            }

            full_access.has_timeout.store(true, Ordering::SeqCst);
            full_access.task_notified.store(false, Ordering::SeqCst);

            schedule::with_current_task_arc(|cur_task| {
                schedule::set_task_state_block(&cur_task);

                let mut locked_wait_task = full_access.wait_task.lock_now_or_die();
                *locked_wait_task = Some(Arc::clone(&cur_task));

                let wake_at_tick = time::get_tick() + timeout_ms;
                time::add_task_to_sleep_queue(cur_task, wake_at_tick);
            });

            true
        });

        if should_block {
            svc::svc_block_current_task();
        }

        self.inner
            .lock()
            .must_with_full_access(|full_access| full_access.task_notified.load(Ordering::SeqCst))
    }

    pub fn notify_allow_isr(&self) {
        self.inner.lock().with_access(|access| match access {
            Access::Full { full_access } => match full_access.wait_task.lock_now_or_die().take() {
                Some(wait_task) => {
                    let has_timeout = full_access.has_timeout.load(Ordering::SeqCst);
                    if has_timeout {
                        let removed = time::remove_task_from_sleep_queue(&wait_task);
                        if removed {
                            schedule::make_task_ready_and_enqueue(wait_task);
                            full_access.task_notified.store(true, Ordering::SeqCst);
                        } else {
                            full_access.count.fetch_add(1, Ordering::SeqCst);
                        }
                    } else {
                        schedule::make_task_ready_and_enqueue(wait_task);
                        full_access.task_notified.store(true, Ordering::SeqCst);
                    }
                }
                None => {
                    full_access.count.fetch_add(1, Ordering::SeqCst);
                    full_access.task_notified.store(true, Ordering::SeqCst);
                }
            },
            Access::PendOnly { pend_access } => {
                pend_access.pending_count.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
}

fn die_if_in_isr() {
    if schedule::is_running_in_isr() {
        unrecoverable::die();
    }
}
