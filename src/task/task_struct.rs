use super::{
    priority::TaskPriority,
    segmented_stack::{self, HotSplitAlleviationBlock},
    trampoline::{self, EntryClosureArg},
};
use crate::{
    config,
    interrupt::{svc, trap_frame::TrapFrame},
    sync::{AtomicCell, Spin, SpinGuard},
    unrecoverable::{self, Lethal},
};
use alloc::{boxed::Box, sync::Arc};
use core::{
    alloc::Layout,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU8, Ordering},
};
use intrusive_collections::{intrusive_adapter, LinkedListAtomicLink};
use static_assertions::const_assert;

#[cfg(feature = "unwind")]
use alloc::sync::Weak;
#[cfg(feature = "unwind")]
use core::any::Any;

#[repr(u8)]
#[derive(PartialEq, Clone, Copy)]
/// All possible states of a task.
pub(crate) enum TaskState {
    /// The task is under initialization. Not ready to run.
    Initializing,
    /// The task is waiting for an event, e.g., a semaphore notification
    /// or a timer expiration.
    Blocked,
    /// The task is ready to run.
    Ready,
    /// The task is running on the CPU.
    Running,
}

#[repr(C)]
#[derive(Default)]
/// Callee-saved general purpose registers on Cortex-M.
struct CalleeSavedGPRegs {
    r4: u32,
    r5: u32,
    r6: u32,
    r7: u32,
    r8: u32,
    r9: u32,
    r10: u32,
    r11: u32,
}

#[repr(C)]
#[derive(Default)]
/// Callee-saved floating point registers on Cortex-M.
struct CalleeSavedFPRegs {
    s16: u32,
    s17: u32,
    s18: u32,
    s19: u32,
    s20: u32,
    s21: u32,
    s22: u32,
    s23: u32,
    s24: u32,
    s25: u32,
    s26: u32,
    s27: u32,
    s28: u32,
    s29: u32,
    s30: u32,
    s31: u32,
}

#[repr(C)]
#[derive(Default)]
/// The context of a task managed by the kernel. The struct does not store
/// caller-saved registers because these registers are pushed to the user stack
/// before context switch.
pub(crate) struct TaskCtxt {
    /// The boundary address of the top stacklet.
    stklet_bound: u32,
    /// The stack pointer value.
    sp: u32,
    /// Preserved callee-saved general purpose registers.
    gp_regs: CalleeSavedGPRegs,
    /// Preserved callee-saved floating point registers.
    fp_regs: CalleeSavedFPRegs,
}

/// The struct representing a task.
pub(crate) struct Task {
    /// The task context preserved in the kernel. When a task is scheduled to
    /// run on the CPU, the spin lock will be acquired during the running
    /// period. Accidental attempt to modify the context of a running task
    /// will resultg in a deadlock, which can help us track down the bug
    /// faster. The spin lock will be released when the running task is
    /// switched out during context switch.
    ///
    /// Note that this field remains being locked when the task makes an SVC.
    /// The SVC handlers should instead use
    /// [`TaskSVCCtxt`](crate::interrupt::TaskSVCCtxt)
    /// to read or modify the task's context.
    ctxt: Spin<TaskCtxt>,
    /// The initial stacklet of a task. Semantically, this pointer owns the
    /// piece of memory it points to. The drop handler must free the memory
    /// to avoid memory leak.
    initial_stklet: AtomicPtr<u8>,
    /// Number of bytes in the initial stacklet. Can be zero.
    init_stklet_size: usize,
    /// The task ID. 0 is reserved for the idle task. Other tasks can take
    /// from 1 to 255.
    id: AtomicU8,
    /// See [`TaskState`].
    state: AtomicCell<TaskState>,

    /*** Fields for unwinding. ***/
    /// Set only when the task is unwinding.
    #[cfg(feature = "unwind")]
    is_unwinding: AtomicBool,
    /// Set when a panicked task has been restarted with a new concurrent task
    /// context.
    #[cfg(feature = "unwind")]
    has_restarted: AtomicBool,

    /*** Fields present only for restartable tasks. ***/
    /// An `Arc` pointing to the bundled struct containing the tak entry
    /// closure and arguments. The types of the closure and arguments are
    /// erased using `Arc<dyn Any>`, so that all task structs will have an
    /// identical type `Task`, rather than `Task<F, A>` with different `F` and
    /// `A`.
    #[cfg(feature = "unwind")]
    entry_closure_arg: Option<Arc<dyn Any + Send + Sync + 'static>>,
    /// A function that can cast the `entry_closure_arg` field from an
    /// `Arc<dyn Any>` to `*const u8`. The resulting raw pointer is used in
    /// the task entry trampoline function.
    #[cfg(feature = "unwind")]
    downcast_func: Option<fn(&(dyn Any + Send + Sync + 'static)) -> *const u8>,
    /// Set when the task is a restarted instance of another panicked task.
    #[cfg(feature = "unwind")]
    restarted_from: Option<Weak<Task>>,
    /// The entry trampoline function the restarted task should run after
    /// being created.
    #[cfg(feature = "unwind")]
    restart_entry_trampoline: Option<extern "C" fn(*const u8)>,

    /*** Fields for segmented stack hot-split alleviation. ***/
    /// The recorded information used to alleviate the hot-split problem of
    /// segmented stacks.
    hsab: Spin<HotSplitAlleviationBlock>,

    /*** Fields for priority scheduling and sleeping. ***/
    /// See [`TaskPriority`].
    priority: AtomicCell<TaskPriority>,
    /// The tick number when a sleeping task should be woken up. This field is
    /// meaningful only the task is sleeping.
    wake_at_tick: AtomicU32,

    /*** Fields for task linked list. ***/
    /// The link field for this struct to form an intrusive linked list.
    /// Invariant: a task struct can be inside at most one intrusive linked
    /// list. Trying to push a task which is already in a linked list into
    /// another linked list will result in a panic.
    pub(super) linked_list_link: LinkedListAtomicLink,
}

// Make sure the `AtomicCell`s used in `Task`'s fields are lock-free to prevent
// deadlocks.
const_assert!(AtomicCell::<TaskState>::is_lock_free());
const_assert!(AtomicCell::<TaskPriority>::is_lock_free());

/// Task struct builder functions.
impl Task {
    /// Build a new task struct. Return `Ok(())` if successful, otherwise
    /// `Err(())`. When the built task panics during its execution, the task's
    /// stack will be unwound, and then the task will be *terminated*.
    ///
    /// - `id`: The ID of the new task. Cannot be 0.
    /// - `entry_closure`: The entry closure for the new task, i.e., the code
    ///   where the new task starts to execute.
    /// - `entry_arg`: The arguments to the closure.
    /// - `init_stklet_size`: The size in bytes of the initial stacklet of the
    ///   new task. When it is set to 0, the entry closure will always request
    ///   for a new stacklet before execution.
    /// - `priority`: The priority of the task. Smaller numerical values
    ///   represent higher priority.
    pub(crate) fn build<F, A>(
        id: u8,
        entry_closure: F,
        entry_arg: A,
        init_stklet_size: usize,
        priority: u8,
    ) -> Result<Self, ()>
    where
        F: FnOnce(A) + Send + 'static,
        A: Send + 'static,
    {
        let mut task = Self::new();
        task.initialize(id, entry_closure, entry_arg, init_stklet_size, priority)?;
        Ok(task)
    }

    /// Build a new restartable task struct. Return `Ok(())` if successful,
    /// otherwise `Err(())`. When the built task panics during its execution,
    /// the task's stack will be unwound, and then the task will be *restarted*.
    /// The restarted task will start its execution again from the entry
    /// closure using the same entry arguments.
    ///
    /// - `id`: The ID of the new task. Cannot be 0.
    /// - `entry_closure`: The entry closure for the new task, i.e., the code
    ///   where the new task starts to execute.
    /// - `entry_arg`: The arguments to the closure.
    /// - `init_stklet_size`: The size in bytes of the initial stacklet of the
    ///   new task. When it is set to 0, the entry closure will always request
    ///   for a new stacklet before execution.
    /// - `priority`: The priority of the task. Smaller numerical values
    ///   represent higher priority.
    #[cfg(feature = "unwind")]
    pub(crate) fn build_restartable<F, A>(
        id: u8,
        entry_closure: F,
        entry_arg: A,
        reserve_stack_size: usize,
        priority: u8,
    ) -> Result<Self, ()>
    where
        F: FnOnce(A) + Send + Sync + Clone + 'static,
        A: Send + Sync + Clone + 'static,
    {
        let mut task = Self::new();
        task.initialize_restartable(id, entry_closure, entry_arg, reserve_stack_size, priority)?;
        Ok(task)
    }

    /// Build a new task struct as the restarted instance of a previously
    /// panicked task. The new task will start its execution from the same
    /// closure using the same arguments as the panicked task.
    #[cfg(feature = "unwind")]
    pub(crate) fn build_restarted(prev_task: Arc<Task>) -> Result<Self, ()> {
        let mut new_task = Self::new();
        new_task.restart_from(prev_task.clone())?;

        // Reduce the priority of the previously panicked task, so that the
        // unwinding procedure of the panicked task uses only otherwise idle
        // CPU time.
        // FIXME: should we place this statement here or elsewhere?
        prev_task.reduce_priority_for_unwind();

        Ok(new_task)
    }

    /// Build the task struct of the idle task.
    pub(crate) fn build_idle() -> Self {
        // Make sure the idle task is built only once. It is an unrecoverable
        // error if attempt to build it twice.
        static IDLE_CREATED: AtomicBool = AtomicBool::new(false);
        let created = IDLE_CREATED.swap(true, Ordering::SeqCst);
        unrecoverable::die_if(|| created);

        let mut idle_task = Self::new();

        // Create the idle task. The closure passed in `.initialize()` is
        // actually not used. The `idle()` function is invoked through the
        // assembly sequence when starting the scheduler.
        idle_task
            .initialize(
                0,
                |_| unrecoverable::die(),
                (),
                config::IDLE_TASK_INITIAL_STACK_SIZE,
                config::IDLE_TASK_PRIORITY,
            )
            .unwrap_or_die();

        // We are about to transmute the current thread as the idle task.
        // Mark the idle task as `Running`.
        idle_task.set_state(TaskState::Running);

        idle_task
    }

    /// Create a new task struct, with all the fields set to their default
    /// values.
    fn new() -> Self {
        Self {
            ctxt: Spin::new(TaskCtxt::default()),
            id: AtomicU8::new(0),
            state: AtomicCell::new(TaskState::Initializing),
            initial_stklet: AtomicPtr::new(core::ptr::null_mut()),
            #[cfg(feature = "unwind")]
            is_unwinding: AtomicBool::new(false),
            #[cfg(feature = "unwind")]
            has_restarted: AtomicBool::new(false),
            #[cfg(feature = "unwind")]
            entry_closure_arg: None,
            #[cfg(feature = "unwind")]
            downcast_func: None,
            #[cfg(feature = "unwind")]
            restart_entry_trampoline: None,
            #[cfg(feature = "unwind")]
            restarted_from: None,
            init_stklet_size: 0,
            hsab: Spin::new(HotSplitAlleviationBlock::default()),
            priority: AtomicCell::new(TaskPriority::new_intrinsic(
                config::TASK_PRIORITY_LEVELS - 1,
            )),
            linked_list_link: LinkedListAtomicLink::new(),
            wake_at_tick: AtomicU32::new(u32::MAX),
        }
    }

    /// The common part of initializing a task struct.
    ///
    /// - `id`: The ID of the new task. Cannot be 0 unless it is the idle task.
    /// - `entry_closure_arg_ptr`: Raw pointer to the bundled entry closure and
    ///   arguments.
    /// - `entry_trampoline`: The address of the trampoline function of the new
    ///   task.
    /// - `init_stklet_size`: The size in bytes of the initial stacklet of the
    ///   new task. When it is set to 0, the entry closure will always request
    ///   for a new stacklet before execution.
    /// - `priority`: The priority of the task. Smaller numerical values
    ///   represent higher priority.
    fn initialize_common(
        &mut self,
        id: u8,
        entry_closure_arg_ptr: usize,
        entry_trampoline: usize,
        init_stklet_size: usize,
        priority: u8,
    ) -> Result<(), ()> {
        // Check priority number validity.
        if priority >= config::TASK_PRIORITY_LEVELS {
            return Err(());
        }

        // Allocate the initial stacklet. `stklet_begin` points to the
        // beginning of the allocated memory chunk, and can be used to call
        // `alloc::alloc::dealloc()` to free the memory. `stklet_end` points to
        // the ending of the memory chunk. The allocated memory chunk is *not*
        // zero-initialized.
        let (stklet_begin, stklet_end) = segmented_stack::alloc_initial_stacklet(init_stklet_size);

        // Store stacklet to the task struct.
        self.initial_stklet.store(stklet_begin, Ordering::SeqCst);
        self.init_stklet_size = init_stklet_size;

        // Let the stack pointer points to the bottom of the initial stacklet.
        let mut sp = stklet_end;

        // Normal tasks (id != 0) are started by an exception return. The
        // initial state is indicated by the trap frame stored on the task's
        // stack. In the following we will build the initial trap frame which
        // is placed at the bottom of the initial stacklet.
        //
        // However, the idle task (id == 0) cannot be started by an exception
        // return, because after boot and during initialization, the CPU runs
        // in thread mode with MSP. It will trigger a HardFault if we try to
        // perform an exception return in thread mode. Thus, we will manually
        // switch the stack pointer to PSP and jump to the idle function with
        // the assembly code in `start_scheduler()`. We need not put a trap
        // frame in idle task's initial stacklet.
        if id != 0 {
            // Move the stack pointer to make space for the trap frame.
            // Safety: The size of the initial stacklet is guaranteed to be
            // larger than the size of a trap frame. Thus, the pointer offset
            // must be within bounds.
            unsafe {
                sp = sp.sub(core::mem::size_of::<TrapFrame>());
            }

            // Initialize the trap frame to its default values.
            //
            // Safety: The stack memory is just allocated, so the current code
            // has exclusive access to the memory.
            let tf_ptr = sp as *mut TrapFrame;
            unsafe {
                tf_ptr.write(TrapFrame::default());
            };

            {
                // Safety: The stack memory is just allocated, so the current code
                // has exclusive access to the memory. The memory is initialized
                // above.
                let tf = unsafe { &mut *tf_ptr };

                // The only bit we need to set in the program state register (PSR)
                // is the Thumb bit (the 24th). Cortex-M4F always run in Thumb state,
                // so we must always set the Thumb bit.
                tf.gp_regs.xpsr = 0x01000000;

                // This is the actual return address of the exception handler. We set
                // it to the entry function of the new task.
                tf.gp_regs.pc = entry_trampoline as u32 | 1;

                // Let the trampoline function return to task destroy SVC.
                tf.gp_regs.lr = svc::svc_destroy_current_task as u32 | 1;

                // Make set the trampoline function argument as the closure pointer.
                tf.gp_regs.r0 = entry_closure_arg_ptr as u32;
            }
        }

        // Set relevant infomation in the task context.
        let ctxt = self.ctxt.get_mut();
        ctxt.sp = sp as u32;
        ctxt.stklet_bound = segmented_stack::stklet_ptr_to_bound(stklet_begin) as u32;

        // Set other task information.
        self.id.store(id, Ordering::SeqCst);
        self.state.store(TaskState::Ready);
        self.priority.store(TaskPriority::new_intrinsic(priority));

        Ok(())
    }

    /// Initialize the task struct for a non-restartable task.
    ///
    /// - `id`: The ID of the new task. Cannot be 0 unless it is the idle task.
    /// - `entry_closure`: The entry closure for the new task, i.e., the code
    ///   where the new task starts to execute.
    /// - `entry_arg`: The arguments to the closure.
    /// - `init_stklet_size`: The size in bytes of the initial stacklet of the
    ///   new task. When it is set to 0, the entry closure will always request
    ///   for a new stacklet before execution.
    /// - `priority`: The priority of the task. Smaller numerical values
    ///   represent higher priority.
    fn initialize<F, A>(
        &mut self,
        id: u8,
        entry_closure: F,
        entry_arg: A,
        init_stklet_size: usize,
        priority: u8,
    ) -> Result<(), ()>
    where
        F: FnOnce(A) + Send + 'static,
        A: Send + 'static,
    {
        // Bundle the entry closure and arguments, and put them onto the heap.
        let boxed_closure_arg = Box::new(EntryClosureArg::new(entry_closure, entry_arg));

        // Get the raw pointer to the bundle.
        let closure_arg_ptr = Box::into_raw(boxed_closure_arg) as usize;

        // Get the function address of the entry trampoline.
        let entry_trampoline = trampoline::task_entry::<F, A> as usize;

        // Perform other common initialization.
        self.initialize_common(
            id,
            closure_arg_ptr,
            entry_trampoline,
            init_stklet_size,
            priority,
        )
    }

    /// Initialize the task struct for a restartable task.
    ///
    /// - `id`: The ID of the new task. Cannot be 0 unless it is the idle task.
    /// - `entry_closure`: The entry closure for the new task, i.e., the code
    ///   where the new task starts to execute.
    /// - `entry_arg`: The arguments to the closure.
    /// - `init_stklet_size`: The size in bytes of the initial stacklet of the
    ///   new task. When it is set to 0, the entry closure will always request
    ///   for a new stacklet before execution.
    /// - `priority`: The priority of the task. Smaller numerical values
    ///   represent higher priority.
    #[cfg(feature = "unwind")]
    fn initialize_restartable<F, A>(
        &mut self,
        id: u8,
        entry_closure: F,
        entry_arg: A,
        init_stklet_size: usize,
        priority: u8,
    ) -> Result<(), ()>
    where
        F: FnOnce(A) + Send + Sync + Clone + 'static,
        A: Send + Sync + Clone + 'static,
    {
        use super::trampoline::RestartableEntryFuncArg;

        // Bundle the entry closure and arguments, and put them onto the heap.
        let arc_closure_arg = Arc::new(RestartableEntryFuncArg::new(entry_closure, entry_arg));

        // Store the bundle to the task struct, so that we can use it again
        // during task restart.
        self.entry_closure_arg = Some(arc_closure_arg);

        // Use downcast function to get the raw pointer to the bundle.
        let downcast_func = trampoline::downcast_to_ptr::<F, A>;
        let closure_arg_ptr =
            downcast_func(self.entry_closure_arg.as_ref().unwrap_or_die().as_ref()) as usize;

        // Get the function address of the entry trampoline.
        let entry_trampoline = trampoline::restartable_task_entry::<F, A>;

        // Store the downcast function to the task struct, so that we can call
        // it again during task restart.
        self.downcast_func = Some(downcast_func);

        // Store the trampoline to the task struct, so that we can call it again
        // during task restart.
        self.restart_entry_trampoline = Some(entry_trampoline);

        // Perform other common initialization.
        self.initialize_common(
            id,
            closure_arg_ptr,
            entry_trampoline as usize,
            init_stklet_size,
            priority,
        )
    }

    /// Initialize the task struct to create a restarted instance of a panicked
    /// task.
    ///
    /// - `prev_task`: The panicked task.
    #[cfg(feature = "unwind")]
    fn restart_from(&mut self, prev_task: Arc<Task>) -> Result<(), ()> {
        // The task ID is kept the same as the panicked task.
        let id = prev_task.id.load(Ordering::SeqCst);

        // Clone restart relevant fields from the panicked task struct.
        self.downcast_func = prev_task.downcast_func.clone();
        self.entry_closure_arg = prev_task.entry_closure_arg.clone();
        self.restart_entry_trampoline = prev_task.restart_entry_trampoline.clone();

        // Unwrap the downcast function and the bundled entry closure and
        // arguments. Get the raw pointer to the bundle using the downcast
        // function.
        let downcast_func = self.downcast_func.unwrap_or_die();
        let entry_closure_arg = self.entry_closure_arg.as_ref().unwrap_or_die();
        let closure_arg_ptr = downcast_func(entry_closure_arg.as_ref()) as usize;

        // Unwrap the entry trampoline function. Get its address.
        let entry_trampoline = self.restart_entry_trampoline.unwrap_or_die() as usize;

        // Get the intrinsic priority of the panicked task. The new task will
        // have the same priority.
        let priority = prev_task.priority.load().intrinsic_priority();

        // Record that this new task is a restarted instance from the panicked
        // task.
        self.restarted_from = Some(Arc::downgrade(&prev_task));

        // FIXME: what if this newly built task struct fail to be inserted into
        // the ready queue?
        // Record that the panicked task has been restarted. This will prevent
        // other restart attempt.
        prev_task.has_restarted.store(true, Ordering::SeqCst);

        // Perform other common initialization.
        self.initialize_common(
            id,
            closure_arg_ptr,
            entry_trampoline,
            prev_task.init_stklet_size,
            priority,
        )
    }
}

/// Field getters and Setters.
impl Task {
    pub(crate) fn get_sp(&mut self) -> u32 {
        self.ctxt.get_mut().sp
    }

    pub(crate) fn get_stk_bound(&mut self) -> u32 {
        self.ctxt.get_mut().stklet_bound
    }

    pub(crate) fn get_state(&self) -> TaskState {
        self.state.load()
    }

    pub(crate) fn set_state(&self, state: TaskState) {
        self.state.store(state);
    }

    pub(crate) fn get_id(&self) -> u8 {
        self.id.load(Ordering::SeqCst)
    }

    #[cfg(feature = "unwind")]
    pub(crate) fn set_unwind_flag(&self, val: bool) {
        self.is_unwinding.store(val, Ordering::SeqCst);
    }

    #[cfg(feature = "unwind")]
    pub(crate) fn is_unwinding(&self) -> bool {
        self.is_unwinding.load(Ordering::SeqCst)
    }

    pub(crate) fn get_wake_tick(&self) -> u32 {
        self.wake_at_tick.load(Ordering::SeqCst)
    }

    #[cfg(feature = "unwind")]
    pub(crate) fn get_restart_origin_task(&self) -> Option<&Weak<Task>> {
        self.restarted_from.as_ref()
    }

    pub(crate) fn set_wake_tick(&self, tick: u32) {
        self.wake_at_tick.store(tick, Ordering::SeqCst);
    }

    #[cfg(feature = "unwind")]
    pub(crate) fn has_restarted(&self) -> bool {
        self.has_restarted.load(Ordering::SeqCst)
    }

    /// Lock the task context and return the mutable raw pointer to the
    /// context. This is used when the scheduler picks a task to run.
    /// See [`Task`] for the invariants of the context.
    pub(crate) fn lock_ctxt(&self) -> *mut TaskCtxt {
        let mut locked_ctxt = self.ctxt.lock_now_or_die();
        let ptr = &mut *locked_ctxt as *mut _;
        core::mem::forget(locked_ctxt);
        ptr
    }

    /// Force unlock the task context. This is used when the previously
    /// running task yields or is blocked. See [`Task`] for the invariants
    /// of the context.
    pub(crate) unsafe fn force_unlock_ctxt(&self) {
        self.ctxt.force_unlock()
    }

    /// Return the lock guard for accessing the hot-split alleviation block.
    pub(crate) fn lock_hsab(&self) -> SpinGuard<HotSplitAlleviationBlock> {
        self.hsab.lock_now_or_die()
    }
}

/// Priority related.
impl Task {
    /// Get the priority of this task.
    pub(crate) fn get_priority(&self) -> TaskPriority {
        self.priority.load()
    }

    /// If the other given task has higher priority, inherit it. Otherwise,
    /// keep the current priority.
    ///
    /// Note: even if the task inherits priority from the given task, its
    /// intrinsic priority will still be kept and can be restored at any time.
    pub(crate) fn ceil_priority_from(&self, other: &Self) {
        let self_prio = self.priority.load();
        let other_prio = other.priority.load();
        if let Ok(inherited_prio) = TaskPriority::try_inherit_from(&self_prio, &other_prio) {
            self.priority.store(inherited_prio);
        }
    }

    /// Set the priority of the task to its intrinsic value, i.e. the one given
    /// at task creation time.
    pub(crate) fn restore_intrinsic_priority(&self) {
        let intrinsic_prio = TaskPriority::restore_intrinsic(&self.priority.load());
        self.priority.store(intrinsic_prio);
    }

    /// Reduce the task's priority during unwinding, so that the unwinder will
    /// use the CPU idle time, unless any priority inversion occurs.
    #[cfg(feature = "unwind")]
    pub(crate) fn reduce_priority_for_unwind(&self) {
        self.priority
            .store(TaskPriority::new_intrinsic(config::UNWIND_PRIORITY))
    }

    /// Return true if and only if this task has higher priority than the other
    /// task.
    pub(crate) fn should_preempt(&self, other: &Self) -> bool {
        if config::ALLOW_TASK_PREEMPTION {
            self.priority.load() < other.priority.load()
        } else {
            false
        }
    }
}

// Create the adapter for the intrusive linked list of task structs.
intrusive_adapter!(
    pub(crate) TaskListAdapter
        = Arc<Task>: Task { linked_list_link: LinkedListAtomicLink }
);

impl Drop for Task {
    /// When dropping a task struct, we should free the initial stacklet.
    fn drop(&mut self) {
        let stklet_ptr = self.initial_stklet.load(Ordering::SeqCst);

        if !stklet_ptr.is_null() {
            // Safety: Semantically, `initial_stklet` owns the memory it points
            // to. The memory was dynamically allocated. We must free it to
            // avoid memory leaks.
            unsafe {
                // Layout is not used in the current dealloc implementation.
                alloc::alloc::dealloc(stklet_ptr, Layout::new::<u8>());
            }
        }
    }
}

impl PartialEq for Task {
    fn eq(&self, other: &Self) -> bool {
        self as *const _ as usize == other as *const _ as usize
    }
}
