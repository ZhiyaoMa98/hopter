use super::idle;
use crate::{
    config,
    interrupt::svc,
    sync::{Access, AllowPendOp, Interruptable, RunPendedOp, Spin},
    task::{Task, TaskCtxt, TaskListAdapter, TaskListInterfaces, TaskState},
    unrecoverable::Lethal,
};
use alloc::sync::Arc;
use core::{
    arch::asm,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicUsize, Ordering},
};
use heapless::mpmc::MpMcQueue;
use intrusive_collections::LinkedList;

/// A ready task queue. Ready tasks will be popped out with respect to
/// their priorities.
type ReadyQueue = Interruptable<Inner>;

/// The inner content of a ready task queue.
struct Inner {
    /// The lock-free circular buffer holding `Arc<Task>`s which are not yet
    /// linked into the ready linked list.
    insert_buffer: InsertBuffer,
    /// Ready tasks linked together as a linked list, allowing us to remove the
    /// one with the highest priority. This linked list is *not* sorted.
    ready_linked_list: Spin<LinkedList<TaskListAdapter>>,
}

/// A lock-free circular buffer holding `Arc<Task>`. When the ready queue is
/// under contention, new ready tasks will be placed into this buffer and will
/// be linked into the ready linked list at a later time.
type InsertBuffer = MpMcQueue<Arc<Task>, { config::MAX_TASK_NUMBER }>;

impl Inner {
    const fn new() -> Self {
        Self {
            insert_buffer: InsertBuffer::new(),
            ready_linked_list: Spin::new(LinkedList::new(TaskListAdapter::NEW)),
        }
    }
}

/// Representing full access to the queue.
struct InnerFullAccessor<'a> {
    insert_buffer: &'a InsertBuffer,
    ready_linked_list: &'a Spin<LinkedList<TaskListAdapter>>,
}

/// Representing pend-only access to the queue. Using this accessor one can only
/// enqueue a task struct `Arc` into the circular buffer. Later the buffer will
/// be consumed and get the tasks linked into the linked list.
struct InnerPendAccessor<'a> {
    insert_buffer: &'a InsertBuffer,
}

/// If the insert buffer is not empty, we should pop these tasks out and get
/// them linked.
impl<'a> RunPendedOp for InnerFullAccessor<'a> {
    fn run_pended_op(&mut self) {
        super::with_current_task(|cur_task| {
            let mut locked_list = self.ready_linked_list.lock_now_or_die();
            while let Some(task) = self.insert_buffer.dequeue() {
                if task.should_preempt(cur_task) {
                    request_context_switch();
                }
                task.set_state(TaskState::Ready);
                locked_list.push_back(task);
            }
        })
    }
}

/// Bind the accessor types.
impl<'a> AllowPendOp<'a> for Inner {
    type FullAccessor = InnerFullAccessor<'a>;
    type PendOnlyAccessor = InnerPendAccessor<'a>;
    fn full_access(&'a self) -> Self::FullAccessor {
        InnerFullAccessor {
            insert_buffer: &self.insert_buffer,
            ready_linked_list: &self.ready_linked_list,
        }
    }
    fn pend_only_access(&'a self) -> Self::PendOnlyAccessor {
        InnerPendAccessor {
            insert_buffer: &self.insert_buffer,
        }
    }
}

/// The ready task queue.
static READY_TASK_QUEUE: ReadyQueue = ReadyQueue::new(Inner::new());

/// Existing task number.
static EXIST_TASK_NUM: AtomicUsize = AtomicUsize::new(0);

/// Make a new task ready. Return `Ok(())` if the new task is ready to be run
/// by the scheduler, otherwise `Err(())` if the maximum number of tasks has been
/// reached or the `id` is not acceptable.
pub(super) fn make_new_task_ready(id: u8, task: Arc<Task>) -> Result<(), ()> {
    // ID 0 is reserved for the idle task.
    if id == 0 {
        return Err(());
    }

    // Error if reached maximum task number.
    if EXIST_TASK_NUM.load(Ordering::SeqCst) >= config::MAX_TASK_NUMBER {
        return Err(());
    }

    READY_TASK_QUEUE.must_with_full_access(|full_access| {
        let mut locked_list = full_access.ready_linked_list.lock_now_or_die();
        locked_list.push_back(task);
        EXIST_TASK_NUM.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
}

/// The pointer to the struct in memory that is used to save task's registers
/// upon exception. This pointer is used by the assembly of exception entry
/// functions.
#[no_mangle]
pub(crate) static CUR_TASK_REGS: AtomicPtr<TaskCtxt> = AtomicPtr::new(core::ptr::null_mut());

/// The ID of the currently running task.
#[no_mangle]
static CUR_TASK_ID: AtomicU8 = AtomicU8::new(0);

/// Choose the next task to run. The chosen task is indicated by the returned pointer
/// to its preserved registers in memory. The pointer has exclusive mutable access
/// to the contents.
#[no_mangle]
pub(crate) extern "C" fn schedule() -> *mut TaskCtxt {
    if SCHEDULER_SUSPEND_CNT.load(Ordering::SeqCst) > 0 {
        loop {}
    }

    // Release the lock on the current task registers.
    // Safety: The lock was acquired when the current task was previously scheduled (see
    // below). There is no active reference to the content.
    unsafe {
        super::release_cur_task_regs();
    }

    {
        READY_TASK_QUEUE.must_with_full_access(|full_access| {
            let mut locked_list = full_access.ready_linked_list.lock_now_or_die();

            super::with_current_task_arc(|cur_task| {
                if cur_task.get_state() == TaskState::Running {
                    cur_task.set_state(TaskState::Ready);
                    locked_list.push_back(cur_task);
                }
            });

            // Get a task to run. Since the ready task is always present, the
            // queue is guaranteed to be non-empty.
            let candidate_task = locked_list.pop_highest_priority().unwrap_or_die();

            // Set its state as running.
            candidate_task.set_state(TaskState::Running);

            // Get ID of the candidate task.
            let cand_id = candidate_task.get_id();

            // Fetch the previous task ID, and then set the candidate task ID
            // as the one currently running.
            let prev_id = CUR_TASK_ID.swap(cand_id, Ordering::SeqCst);

            // Invoke idle callbacks.
            {
                let locked_callbacks = idle::lock_idle_callbacks();

                // When the idle task is switched out of CPU.
                if prev_id == 0 {
                    for callback in locked_callbacks.iter() {
                        callback.idle_end_callback();
                    }
                }

                // When the idle task is switched on to the CPU.
                if cand_id == 0 {
                    for callback in locked_callbacks.iter() {
                        callback.idle_begin_callback();
                    }
                }
            }

            // Update the current task pointer.
            super::set_cur_task(candidate_task);

            // Lock the task registers and set the pointers to it. The pointer
            // will be used upon context switch to preserve task registers into
            // the memory.
            let cur_task_regs = super::lock_cur_task_regs();

            // Store the pointer to a global variable so that the assembly sequence
            // in PendSV can find it.
            CUR_TASK_REGS.store(cur_task_regs, Ordering::SeqCst);

            CONTEXT_SWITCH_REQUESTED.store(false, Ordering::SeqCst);

            cur_task_regs
        })
    }
}

/// Start the scheduler and start to run tasks.
///
/// Safety: This function should only be called at system initialization stage
/// when the system is still running with MSP.
pub unsafe fn start_scheduler() -> ! {
    let mut idle_task = Task::build_idle();

    let stack_bottom = idle_task.get_sp();
    let idle_stk_bound = idle_task.get_stk_bound();

    // Set the idle task as the currently running task.
    super::set_cur_task(Arc::new(idle_task));

    // Lock the idle task's registers and set the pointers to it.
    // The pointer will be used upon exception to preserve idle task's
    // registers into the memory.
    let cur_task_regs = super::lock_cur_task_regs();
    CUR_TASK_REGS.store(cur_task_regs, Ordering::SeqCst);

    // The idle task is an existing task.
    EXIST_TASK_NUM.fetch_add(1, Ordering::SeqCst);

    unsafe {
        // Run the idle task.
        asm!(
            // Set the idle task's stack pointer.
            "msr psp, r0",
            // Set the idle task's stacklet boundary.
            "ldr r0, ={stklet_boundary_mem_addr}",
            "str r1, [r0]",
            // Start to use PSP instead of MSP.
            // PSP is for running tasks.
            // MSP is for the kernel.
            "mrs r0, control",
            "orr r0, #2",
            "msr control, r0",
            // Let MSP point to the kernel stack bottom.
            "ldr r0, ={kern_stk_bottom}",
            "msr msp, r0",
            // Execute a floating point instruction, so that the CPU will
            // have the internal floating point context bit set, and later
            // upon SVC the CPU will push a trap frame with floating point
            // registers. Just enabling FPU is NOT enough for the CPU to push
            // floating point registers upon exception.
            "vmov.f32 s0, s0",
            // Jump to the idle function.
            "b {idle}",
            idle = sym super::idle,
            stklet_boundary_mem_addr = const config::STACKLET_BOUNDARY_MEM_ADDR,
            kern_stk_bottom = const config::CONTIGUOUS_STACK_BOTTOM,
            in("r0") stack_bottom,
            in("r1") idle_stk_bound,
            options(noreturn)
        )
    }
}

/// Destroy the task struct of the currently running task and switch to another
/// ready task.
pub(crate) fn destroy_current_task_and_schedule() {
    EXIST_TASK_NUM.fetch_sub(1, Ordering::SeqCst);

    // Mark the task state as `Empty` so that the scheduler will drop the task struct.
    super::with_current_task(|cur_task| cur_task.set_state(TaskState::Initializing));

    // Just request a context switch without putting the currently running task
    // back to the ready queue. Although `svc_task_block()` is implemented in
    // the same way, the difference is that prior to some task calling to block,
    // the task struct of the blocking task would have been put onto a wait queue
    // and thus not get dropped.
    cortex_m::peripheral::SCB::set_pendsv()
}

pub(crate) fn is_running_in_isr() -> bool {
    let ipsr: u32;

    unsafe {
        asm!(
            "mrs {}, ipsr",
            out(reg) ipsr,
            options(nomem, nostack)
        );
    }

    ipsr != 0
}

pub(crate) fn is_running_in_pendsv() -> bool {
    let ipsr: u32;

    unsafe {
        asm!(
            "mrs {}, ipsr",
            out(reg) ipsr,
            options(nomem, nostack)
        );
    }

    ipsr == 14
}

static SCHEDULER_STARTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn is_scheduler_started() -> bool {
    SCHEDULER_STARTED.load(Ordering::SeqCst)
}

pub(super) fn mark_scheduler_started() {
    SCHEDULER_STARTED.store(true, Ordering::SeqCst)
}

static SCHEDULER_SUSPEND_CNT: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn increment_suspend_count() {
    SCHEDULER_SUSPEND_CNT.fetch_add(1, Ordering::SeqCst);
}

pub(crate) fn decrement_suspend_count() {
    SCHEDULER_SUSPEND_CNT.fetch_sub(1, Ordering::SeqCst);
}

pub(crate) fn yield_cur_task_from_isr() {
    cortex_m::peripheral::SCB::set_pendsv()
}

pub(crate) fn block_cur_task_from_isr() {
    yield_cur_task_from_isr()
}

pub fn yield_current_task() {
    svc::svc_yield_current_task();
}

static CONTEXT_SWITCH_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn request_context_switch() {
    CONTEXT_SWITCH_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn yield_for_preemption() {
    if !CONTEXT_SWITCH_REQUESTED.load(Ordering::SeqCst)
        || SCHEDULER_SUSPEND_CNT.load(Ordering::SeqCst) > 0
    {
        return;
    }

    if !is_running_in_isr() {
        svc::svc_yield_current_task();
    } else if !is_running_in_pendsv() {
        yield_cur_task_from_isr();
    }
}

pub(crate) fn make_task_ready_and_enqueue(task: Arc<Task>) {
    READY_TASK_QUEUE.with_access(|access| match access {
        Access::Full { full_access } => super::with_current_task(|cur_task| {
            if task.should_preempt(cur_task) {
                request_context_switch();
            }
            task.set_state(TaskState::Ready);
            let mut locked_list = full_access.ready_linked_list.lock_now_or_die();
            locked_list.push_back(task);
        }),
        Access::PendOnly { pend_access } => {
            pend_access.insert_buffer.enqueue(task).unwrap_or_die();
        }
    })
}

/// Set the task state to [`Blocked`](TaskState::Blocked). When a blocked task
/// is switched out of the CPU, the scheduler will not add it back to the
/// ready queue. The code blocking the task should add the task either to a
/// waiting queue i.e. [`WaitQueue`](crate::sync::WaitQueue) or to the sleeping
/// queue in [`time`](crate::time).
pub(crate) fn set_task_state_block(task: &Task) {
    task.set_state(TaskState::Blocked);
}
