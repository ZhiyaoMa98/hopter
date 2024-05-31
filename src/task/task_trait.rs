use super::{
    priority::TaskPriority, FatLinked, HotSplitAlleviationBlock, TaskCtxt, TaskFatLink, TaskState,
};
use crate::sync::SpinGuard;
use alloc::sync::Weak;
use core::any::Any;
use core::ptr::NonNull;

pub(crate) trait TaskTrait: FatLinked + Any + Send + Sync {
    fn get_link(self: *const Self) -> NonNull<TaskFatLink>;

    fn as_any(&self) -> &dyn Any;

    fn get_sp(&mut self) -> u32;

    fn get_stk_bound(&mut self) -> u32;

    fn get_state(&self) -> TaskState;

    fn set_state(&self, state: TaskState);

    fn get_id(&self) -> u8;

    fn set_unwind_flag(&self, val: bool);

    fn is_unwinding(&self) -> bool;

    fn get_wake_tick(&self) -> u32;

    fn get_restart_origin_task(&self) -> Option<&Weak<dyn TaskTrait>>;

    fn set_wake_tick(&self, tick: u32);

    fn has_restarted(&self) -> bool;

    /// Lock the task context and return the mutable raw pointer to the
    /// context. This is used when the scheduler picks a task to run.
    /// See [`Task`] for the invariants of the context.
    fn lock_ctxt(&self) -> *mut TaskCtxt;

    /// Force unlock the task context. This is used when the previously
    /// running task yields or is blocked. See [`Task`] for the invariants
    /// of the context.
    unsafe fn force_unlock_ctxt(&self);

    /// Return the lock guard for accessing the hot-split alleviation block.
    fn lock_hsab(&self) -> SpinGuard<HotSplitAlleviationBlock>;

    /// Get the priority of this task.
    fn get_priority(&self) -> TaskPriority;

    /// If the other given task has higher priority, inherit it. Otherwise,
    /// keep the current priority.
    ///
    /// Note: even if the task inherits priority from the given task, its
    /// intrinsic priority will still be kept and can be restored at any time.
    fn ceil_priority_from(&self, other: &dyn TaskTrait);

    /// Set the priority of the task to its intrinsic value, i.e. the one given
    /// at task creation time.
    fn restore_intrinsic_priority(&self);

    /// Reduce the task's priority during unwinding, so that the unwinder will
    /// use the CPU idle time, unless any priority inversion occurs.
    fn reduce_priority_for_unwind(&self);

    /// Return true if and only if this task has higher priority than the other
    /// task.
    fn should_preempt(&self, other: &dyn TaskTrait) -> bool;
}
