use super::super::{schedule, task::TaskTrait, unwind};
use super::{
    HeldInterrupt, Holdable, Lockable, Scheduler, SchedulerSuspendGuard, SpinGeneric,
    SpinGenericGuard, SpinSchedSafe, UnlockableGuard, WaitQueue,
};
use alloc::sync::Arc;
use core::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};
use owning_ref::StableAddress;

/// A lock that will block the current task if it tries to acquire the lock
/// when the lock is being held by other tasks, similar to `std::sync::Mutex`.
pub type Mutex<T> = MutexGeneric<T, (), ()>;

/// Similar to `std::sync::MutexGuard`.
pub type MutexGuard<'a, T> = MutexGenericGuard<'a, T, (), ()>;

/// A lock that will block the current task if it tries to acquire the lock
/// when the lock is being held by other tasks, similar to `std::sync::Mutex`.
/// When the lock is acquired, the corresponding interrupt will also be
/// atomically masked.
pub type MutexIrqSafe<T, I> = MutexGeneric<T, I, HeldInterrupt<I>>;

/// Similar to `std::sync::MutexGuard`, but with an additional feature that
/// while the guard is not dropped, the corresponding interrupt will continue
/// to be masked.
pub type MutexIrqSafeGuard<'a, T, I> = MutexGenericGuard<'a, T, I, HeldInterrupt<I>>;

/// Generic type of a mutex. When the mutex is being held, some extra
/// condition can also be held. For example, the extra condition may be
/// some interrupt being masked. Failure to acquire a mutex will cause
/// the calling task to block until the mutex can be acquired.
/// Generic parameter:
/// - `T``: The type that the lock is protecting.
/// - `H``: A holdable condition.
/// - `G``: The guard type representing the additional held condition.
pub struct MutexGeneric<T, H, G>
where
    T: ?Sized,
    H: Holdable<GuardType = G>,
{
    /// The wait queue for tasks being blocked to wait for their turn.
    queue: WaitQueue,
    /// The task that is currently holding this mutex.
    owner: SpinSchedSafe<Option<Arc<dyn TaskTrait>>>,
    /// If the mutex is released in an unwinding path, this variable will
    /// be set to `true`. This is just additional information to application
    /// code.
    poisoned: AtomicBool,
    /// The spin lock actually protecting the data. When the mutex is being
    /// acquired, the scheduler will first be suspended and then the additional
    /// condition be held. After the mutex is fully acquired, the guard on the
    /// spin lock will be downgraded to a weaker spin lock guard that no longer
    /// suspend the scheduler, but which keeps holding only the additional
    /// condition `H`.
    spin_lock: SpinGeneric<T, (Scheduler, H), (G, SchedulerSuspendGuard)>,
}

/// Generic type of a mutex guard that can dereference into contained type.
/// When the guard is dropped, the mutex will be released and the additionally
/// held condition will be released atomically.
pub struct MutexGenericGuard<'a, T, H, G>
where
    T: ?Sized,
    H: Holdable<GuardType = G>,
{
    guard: Option<SpinGenericGuard<'a, T, G>>,
    mutex: &'a MutexGeneric<T, H, G>,
}

impl<T, H, G> MutexGeneric<T, H, G>
where
    H: Holdable<GuardType = G>,
{
    /// Create a new mutex instance.
    pub const fn new(data: T) -> Self {
        MutexGeneric {
            queue: WaitQueue::new(),
            owner: SpinSchedSafe::new(None),
            poisoned: AtomicBool::new(false),
            spin_lock: SpinGeneric::new(data),
        }
    }

    #[allow(unused)]
    /// Discard the mutex and get back the contained data.
    pub fn into_inner(self) -> T {
        self.spin_lock.into_inner()
    }

    #[allow(unused)]
    /// Return if the mutex is ever being held during an unwinding.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::SeqCst)
    }

    #[allow(unused)]
    pub unsafe fn force_unlock(&self) {
        self.owner.lock().take();
        self.spin_lock.force_unlock();
        self.queue.notify_one_allow_isr();
    }
}

impl<T, H, G> MutexGeneric<T, H, G>
where
    T: ?Sized,
    H: Holdable<GuardType = G>,
{
    /// If the mutex has not been locked, lock it and return it within `Some`.
    /// Otherwise, return `None`.
    pub fn try_lock(&self) -> Option<MutexGenericGuard<T, H, G>> {
        self.spin_lock
            // Try to acquire the spin lock that wraps around the protected data. If it
            // failes, i.e. returns `None`, the following will be skipped and this
            // method returns with `None`.
            .try_lock()
            // If the spin lock is acquired, the scheduler should have been suspended.
            // Update the current task to be the owner of the lock. If we acquired the
            // lock inside an ISR, it should always succeed. However, we will not update
            // the owner field for two reasons. First, logically, the ISR cannot context
            // switch, so the ISR will always release the lock before any other code can
            // later acquire it. Second, mechanically, reading the current task pointer
            // from an ISR can cause deadlock when the scheduler is setting the current
            // task pointer.
            .and_then(|guard| {
                if !schedule::is_running_in_isr() {
                    schedule::with_current_task_arc(|cur_task| self.owner.lock().replace(cur_task));
                }
                Some(guard)
            })
            // After we finish updating the owner, we can resume the scheduler. We now
            // downgrade the guard that both suspends the scheduler and holds the
            // condition `H` to a weaker one that no longer suspends the scheduler
            // but still holds `H`.
            .map(|guard| SpinGenericGuard {
                raw_spin_guard: guard.raw_spin_guard,
                // Discard the `.1` component which is the scheduler suspend guard.
                _held: guard._held.0,
            })
            // Finally, we convert it to `MutexGenericGuard`.
            .map(|guard| MutexGenericGuard {
                guard: Some(guard),
                mutex: &self,
            })
    }

    /// Lock the mutex. If the mutex has already been locked, block until the
    /// current task can lock it.
    pub fn lock(&self) -> MutexGenericGuard<T, H, G> {
        // Fast path for no contention.
        if let Some(guard) = self.try_lock() {
            return guard;
        }

        // Priority inheritance.
        schedule::with_current_task(|cur_task| {
            let locked_owner = self.owner.lock();
            if let Some(owner) = locked_owner.as_ref() {
                owner.ceil_priority_from(cur_task);
            }
        });

        // Otherwise, wait on the wait queue until the current task can lock it.
        // The called method will return the mutex guard upon return.
        self.queue.wait_until(|| self.try_lock())
    }
}

impl<'a, T, H, G> Deref for MutexGenericGuard<'a, T, H, G>
where
    T: ?Sized,
    H: Holdable<GuardType = G>,
{
    type Target = T;

    fn deref(&self) -> &T {
        self.guard.as_ref().unwrap()
    }
}

impl<'a, T, H, G> DerefMut for MutexGenericGuard<'a, T, H, G>
where
    T: ?Sized,
    H: Holdable<GuardType = G>,
{
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_mut().unwrap()
    }
}

impl<'a, T, H, G> Drop for MutexGenericGuard<'a, T, H, G>
where
    T: ?Sized,
    H: Holdable<GuardType = G>,
{
    fn drop(&mut self) {
        if let Some(cur_task) = self.mutex.owner.lock().take() {
            cur_task.restore_intrinsic_priority();
        }

        if unwind::unwind::is_unwinding() {
            self.mutex.poisoned.store(true, Ordering::SeqCst);
        }

        // Drop the spin lock before notifying other tasks. Otherwise, it a task being
        // notified has higher priority than the current task, without having the spin
        // lock guard dropped first, the woken up high priority task will immediately
        // go back to block state again, causing a deadlock.
        self.guard.take();

        // Notify a task on the waitqueue that the lock is released, which occurs
        // automatically when the inner `guard` is dropped after this method executes.
        self.mutex.queue.notify_one_allow_isr();
    }
}

impl<T, H, G> Lockable for MutexGeneric<T, H, G>
where
    T: ?Sized,
    H: Holdable<GuardType = G>,
{
    type GuardType<'a> = MutexGenericGuard<'a, T, H, G> where T: 'a, H: 'a, G:'a;

    fn lock_and_get_guard(&self) -> Self::GuardType<'_> {
        self.lock()
    }
}

impl<'a, T, H, G> UnlockableGuard<'a> for MutexGenericGuard<'a, T, H, G>
where
    T: ?Sized,
    H: Holdable<GuardType = G>,
{
    type LockType = MutexGeneric<T, H, G>;

    fn unlock_and_into_lock_ref(self) -> &'a Self::LockType {
        self.mutex
    }
}

unsafe impl<'a, T, H, G> StableAddress for MutexGenericGuard<'a, T, H, G>
where
    T: ?Sized,
    H: Holdable<GuardType = G>,
{
}
