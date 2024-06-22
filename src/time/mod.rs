use crate::{
    config,
    interrupt::svc,
    schedule,
    sync::{Access, AllowPendOp, Interruptable, RefCellSchedSafe, RunPendedOp, Spin},
    task::{Task, TaskListAdapter, TaskListInterfaces},
    unrecoverable::Lethal,
};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use heapless::mpmc::MpMcQueue;
use intrusive_collections::LinkedList;

struct Inner {
    time_sorted_queue: Spin<LinkedList<TaskListAdapter>>,
    delete_buffer: DeleteBuffer,
    time_to_wakeup: AtomicBool,
}

type DeleteBuffer = MpMcQueue<Arc<Task>, { config::MAX_TASK_NUMBER }>;

impl Inner {
    const fn new() -> Self {
        Self {
            time_sorted_queue: Spin::new(LinkedList::new(TaskListAdapter::NEW)),
            delete_buffer: DeleteBuffer::new(),
            time_to_wakeup: AtomicBool::new(false),
        }
    }
}

type SleepQueue = RefCellSchedSafe<Interruptable<Inner>>;

struct InnerFullAccessor<'a> {
    time_sorted_queue: &'a Spin<LinkedList<TaskListAdapter>>,
    delete_buffer: &'a DeleteBuffer,
    time_to_wakeup: &'a AtomicBool,
}

struct InnerPendAccessor<'a> {
    delete_buffer: &'a DeleteBuffer,
    time_to_wakeup: &'a AtomicBool,
}

/// Bind the accessor types.
impl<'a> AllowPendOp<'a> for Inner {
    type FullAccessor = InnerFullAccessor<'a>;
    type PendOnlyAccessor = InnerPendAccessor<'a>;
    fn full_access(&'a self) -> InnerFullAccessor<'a> {
        InnerFullAccessor {
            time_sorted_queue: &self.time_sorted_queue,
            delete_buffer: &self.delete_buffer,
            time_to_wakeup: &self.time_to_wakeup,
        }
    }
    fn pend_only_access(&'a self) -> InnerPendAccessor<'a> {
        InnerPendAccessor {
            delete_buffer: &self.delete_buffer,
            time_to_wakeup: &self.time_to_wakeup,
        }
    }
}

impl<'a> InnerFullAccessor<'a> {
    fn wake_expired_tasks(&self) {
        let cur_tick = TICKS.load(Ordering::SeqCst);
        let mut locked_queue = self.time_sorted_queue.lock_now_or_die();

        // remove also moves the cursor to the next element
        let mut cursor_mut = locked_queue.front_mut();
        while let Some(task) = cursor_mut.get() {
            if task.get_wake_tick() <= cur_tick {
                let task = cursor_mut.remove().unwrap_or_die();
                schedule::make_task_ready_and_enqueue(task);
            } else {
                break;
            }
        }

        while let Some(task) = self.delete_buffer.dequeue() {
            let removed = locked_queue.remove_task(&task);
            if removed {
                schedule::make_task_ready_and_enqueue(task);
            }
        }
    }
}

impl<'a> RunPendedOp for InnerFullAccessor<'a> {
    fn run_pended_op(&mut self) {
        if self.time_to_wakeup.load(Ordering::SeqCst) {
            self.wake_expired_tasks();
            self.time_to_wakeup.store(false, Ordering::SeqCst);
        }
    }
}

static SLEEP_TASK_QUEUE: SleepQueue = RefCellSchedSafe::new(Interruptable::new(Inner::new()));

/// The tick number of SysTick.
static TICKS: AtomicU32 = AtomicU32::new(0);

/// Advance the SysTick count by 1.
pub fn advance_tick() {
    TICKS.fetch_add(1, Ordering::SeqCst);
}

pub fn get_tick() -> u32 {
    TICKS.load(Ordering::SeqCst)
}

/// Wake up those sleeping tasks that have their sleeping time expired.
pub fn wake_sleeping_tasks() {
    SLEEP_TASK_QUEUE.lock().with_access(|access| match access {
        Access::Full { full_access } => {
            full_access.wake_expired_tasks();
        }
        Access::PendOnly { pend_access } => {
            pend_access.time_to_wakeup.store(true, Ordering::SeqCst)
        }
    });
}

/// Block the task for the given number of milliseconds.
#[inline]
pub fn sleep_ms(ms: u32) {
    let wake_at_tick = get_tick() + ms;

    // Using while loop to prevent spurious wakeup.
    // FIXME: while loop not necessary any more.
    while get_tick() < wake_at_tick {
        add_cur_task_to_sleep_queue(wake_at_tick);

        // Yield from the current task. Even if the current task has already
        // been woken up, yielding from it will not introduce deadlock.
        svc::svc_block_current_task();
    }

    // Outline the logic to reduce the stack frame size of `sleep_ms`.
    #[inline(never)]
    fn add_cur_task_to_sleep_queue(wake_at_tick: u32) {
        schedule::with_current_task_arc(|cur_task| {
            schedule::set_task_state_block(&cur_task);
            add_task_to_sleep_queue(cur_task, wake_at_tick);
        })
    }
}

pub(crate) fn add_task_to_sleep_queue(task: Arc<Task>, wake_at_tick: u32) {
    SLEEP_TASK_QUEUE
        .lock()
        .must_with_full_access(|full_access| {
            task.set_wake_tick(wake_at_tick);
            let mut locked_queue = full_access.time_sorted_queue.lock_now_or_die();
            locked_queue.push_back_tick_sorted(task);
        });
}

pub(crate) fn remove_task_from_sleep_queue_allow_isr(task: Arc<Task>) {
    SLEEP_TASK_QUEUE.lock().with_access(|access| match access {
        Access::Full { full_access } => {
            let mut locked_queue = full_access.time_sorted_queue.lock_now_or_die();
            let removed = locked_queue.remove_task(&task);
            if removed {
                schedule::make_task_ready_and_enqueue(task);
            }
        }
        Access::PendOnly { pend_access } => {
            pend_access.delete_buffer.enqueue(task).unwrap_or_die();
        }
    });
}

/// A time-based task barrier that allow a task to proceed at a given interval.
pub struct IntervalBarrier {
    /// The interval in milliseconds.
    interval_ms: u32,
    /// The next SysTick count to wake up the blocked task.
    next_tick_to_wake: u32,
}

impl IntervalBarrier {
    /// Create a new barrier that will unblock the task at the given interval
    /// in milliseconds. The first time that the barrier allows a task to
    /// proceed is the creation time plus the interval.
    pub fn new(interval_ms: u32) -> Self {
        let next_tick_to_wake = TICKS.load(Ordering::SeqCst) + interval_ms;
        Self {
            interval_ms,
            next_tick_to_wake,
        }
    }

    /// Block the current task until the interval is elapsed since the last time
    /// the task was resumed.
    /// FIXME: fix the case when the task cannot keep up with the interval of the
    /// barrier.
    pub fn wait(&mut self) {
        let cur_tick = TICKS.load(Ordering::SeqCst);
        if cur_tick < self.next_tick_to_wake {
            let ms_to_sleep = self.next_tick_to_wake - cur_tick;
            sleep_ms(ms_to_sleep);
        }
        self.next_tick_to_wake += self.interval_ms;
    }
}
