use super::super::task::{Task, TaskTrait};
use super::scheduler;
use alloc::sync::Arc;

/// Create a new task with the given task ID and the closure as the entry
/// function. The task ID should be non-zero otherwise the function will
/// return `Err(())`. `reserve_stack_size` is the extra bytes allocated
/// to the initial stacklet.
pub fn start_task<F, A>(
    id: u8,
    entry_closure: F,
    entry_argument: A,
    reserve_stack_size: usize,
    priority: u8,
) -> Result<(), ()>
where
    F: FnOnce(A) + Send + 'static,
    A: Send + 'static,
{
    let new_task = Task::build(
        id,
        entry_closure,
        entry_argument,
        reserve_stack_size,
        priority,
    )?;
    scheduler::make_new_task_ready(id, Arc::new(new_task))
}

/// Create a new task with the given task ID and the closure as the entry
/// function. The task ID should be non-zero otherwise the function will
/// return `Err(())`. `reserve_stack_size` is the extra bytes allocated
/// to the initial stacklet.
///
/// When created task panics, it will automatically restart using the same
/// entry function and argument, and thus both of them must implement `Clone`.
pub fn start_restartable_task<F, A>(
    id: u8,
    entry_closure: F,
    entry_argument: A,
    reserve_stack_size: usize,
    priority: u8,
) -> Result<(), ()>
where
    F: FnOnce(A) + Send + Sync + Clone + 'static,
    A: Send + Sync + Clone + 'static,
{
    let new_task = Task::build_restartable(
        id,
        entry_closure,
        entry_argument,
        reserve_stack_size,
        priority,
    )?;
    scheduler::make_new_task_ready(id, Arc::new(new_task))
}

/// Start a new task from a previously failed task.
pub(in super::super) fn restart_from_task(prev_task: Arc<dyn TaskTrait>) -> Result<(), ()> {
    let id = prev_task.get_id();
    let new_task = Task::build_restarted(prev_task)?;

    // FIXME: should check for available task slot in advance but not here.
    scheduler::make_new_task_ready(id, Arc::new(new_task))
}
