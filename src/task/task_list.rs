use super::task_struct::TaskListAdapter;
use super::Task;
use alloc::sync::Arc;
use intrusive_collections::LinkedList;

/// Additional interfaces for task list.
pub(crate) trait TaskListInterfaces {
    fn remove_task(&mut self, task: &Task) -> bool;
    fn push_back_tick_sorted(&mut self, new_task: Arc<Task>);
    fn pop_highest_priority(&mut self) -> Option<Arc<Task>>;
}

impl TaskListInterfaces for LinkedList<TaskListAdapter> {
    fn remove_task(&mut self, task: &Task) -> bool {
        let mut cursor_mut = self.front_mut();

        while let Some(candidate_task) = cursor_mut.get() {
            if candidate_task == task {
                cursor_mut.remove();
                return true;
            }
            cursor_mut.move_next();
        }

        false
    }

    /// Push a new task into the linked list. Maintain ascending order w.r.t.
    /// the tick number to wake up.
    fn push_back_tick_sorted(&mut self, new_task: Arc<Task>) {
        let mut cursor_mut = self.front_mut();

        // Move the cursor until we encounter a task with a higher wake up tick
        // number or we are at the end of the list.
        while let Some(task) = cursor_mut.get() {
            if new_task.get_wake_tick() < task.get_wake_tick() {
                break;
            }
            cursor_mut.move_next();
        }

        // Panics if the task is already in a list, i.e., the linked field in
        // the task struct is already in use.
        cursor_mut.insert_before(new_task);
    }

    /// Pop out the task with the highest priority in the linked list. If
    /// there are multiple tasks having the highest priority, the one in the
    /// front will be popped out. Return `None` if the list is empty.
    fn pop_highest_priority(&mut self) -> Option<Arc<Task>> {
        let mut cursor = self.front();

        // Get the priority of the first task in the list.
        let mut max_prio = match cursor.get() {
            Some(task) => task.get_priority(),
            None => return None,
        };
        let mut max_pos = 0usize;

        // Move the cursor to the next element.
        cursor.move_next();

        // Record the position of the cursor.
        let mut cur_pos = 1usize;

        // Scan through the linked list. Whenever we see a task with higher
        // priority than all scanned tasks, update the position.
        while let Some(task) = cursor.get() {
            let cur_prio = task.get_priority();

            // Update the position if we see higher priority.
            // Smaller numerical value represents higher priority.
            if cur_prio < max_prio {
                max_pos = cur_pos;
                max_prio = cur_prio;
            }

            cursor.move_next();
            cur_pos += 1;
        }

        // Move the cursor to the task with the highest priority.
        let mut cursor = self.front_mut();
        for _ in 0..max_pos {
            cursor.move_next();
        }

        // Pop out the chosen task.
        cursor.remove()
    }
}
