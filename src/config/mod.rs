use static_assertions::const_assert;

#[macro_use]
mod helper;

/* ############################ */
/* ### Stack Configurations ### */
/* ############################ */

/// The boundary of the contiguous stack that its top should not grow beyond.
pub const CONTIGUOUS_STACK_BOUNDARY: u32 = 0x2000_0010;
assert_value_type!(CONTIGUOUS_STACK_BOUNDARY, u32);

/// The bottom of the congituous stack.
pub const CONTIGUOUS_STACK_BOTTOM: u32 = 0x2000_1000;
assert_value_type!(CONTIGUOUS_STACK_BOTTOM, u32);

// Stacks are assumed to grow downwards.
const_assert!(CONTIGUOUS_STACK_BOUNDARY < CONTIGUOUS_STACK_BOTTOM);
// Stacks should be 8-byte aligned.
const_assert!(CONTIGUOUS_STACK_BOUNDARY % 8 == 0);
const_assert!(CONTIGUOUS_STACK_BOTTOM % 8 == 0);

/// The address in memory where the active stacklet boundary is stored.
pub const STACKLET_BOUNDARY_MEM_ADDR: u32 = 0x2000_0000;
assert_value_type!(STACKLET_BOUNDARY_MEM_ADDR, u32);

// The boundary value is 4-byte so should be 4-byte aligned.
const_assert!(STACKLET_BOUNDARY_MEM_ADDR % 4 == 0);
// The memory address should be encodable as a thumb2 instruction constant,
// so that we can use a `mov.w` instruction to load the address into a register.
const_assert!(helper::is_thumb2_allowed_constant(
    STACKLET_BOUNDARY_MEM_ADDR
));

/// The extra size added to a stacklet allocation request in addition to the
/// allocation size requested by the function prologue.
pub const STKLET_ADDITION_ALLOC_SIZE: usize = 64;
assert_value_type!(STKLET_ADDITION_ALLOC_SIZE, usize);

// The additional allocation size should be a multiple of 8
const_assert!(STKLET_ADDITION_ALLOC_SIZE % 8 == 0);

/* ########################### */
/* ### Heap Configurations ### */
/* ########################### */

/// The ending address of the heap region.
pub const HEAP_END: u32 = 0x2002_0000;
assert_value_type!(HEAP_END, u32);

/// Free memory chunks use 16-bit links to form linked lists. Since memory
/// chunks are 4-byte aligned, the linkes can represent a range of 2^18 bytes.
/// The represented range is
/// `[MEM_CHUNK_LINK_OFFSET, MEM_CHUNK_LINK_OFFSET + 2^18)`.
pub const MEM_CHUNK_LINK_OFFSET: u32 = 0x2000_0000;
assert_value_type!(MEM_CHUNK_LINK_OFFSET, u32);

/* ################################ */
/* ### Interrupt Configurations ### */
/* ################################ */

/// The numerical stepping between two adjacent IRQ priority levels.
/// The interrupt priority registers on Cortex-M reserves 8-bits for each
/// interrupt, but for most MCU implementations only a few significant bits
/// are used. For example, if only the top 5 significant bits are used, then
/// the numerical granularity will be 8. If the top 4 significant bits are used,
/// then the numerical granularity will be 16. See Nested Vectored Interrupt
/// Controller (NVIC) in Cortex-M for details.
pub const IRQ_PRIORITY_GRANULARITY: u8 = 16;
assert_value_type!(IRQ_PRIORITY_GRANULARITY, u8);

/// The maximum priority of an interrupt. It has the smallest numerical value.
pub const IRQ_MAX_PRIORITY: u8 = 7 * IRQ_PRIORITY_GRANULARITY;
assert_value_type!(IRQ_MAX_PRIORITY, u8);

/// The higher priority of an interrupt. Defined for convenience.
pub const IRQ_HIGH_PRIORITY: u8 = 8 * IRQ_PRIORITY_GRANULARITY;
assert_value_type!(IRQ_HIGH_PRIORITY, u8);

/// The normal priority of an interrupt. Defined for convenience.
pub const IRQ_NORMAL_PRIORITY: u8 = 9 * IRQ_PRIORITY_GRANULARITY;
assert_value_type!(IRQ_NORMAL_PRIORITY, u8);

/// The lower priority of an interrupt. Defined for convenience.
pub const IRQ_LOW_PRIORITY: u8 = 10 * IRQ_PRIORITY_GRANULARITY;
assert_value_type!(IRQ_LOW_PRIORITY, u8);

/// The minimum priority of an interrupt. It has the largest numerical value.
pub const IRQ_MIN_PRIORITY: u8 = 11 * IRQ_PRIORITY_GRANULARITY;
assert_value_type!(IRQ_MIN_PRIORITY, u8);

// Smaller numerical value represents higher priority.
const_assert!(IRQ_MIN_PRIORITY > IRQ_MAX_PRIORITY);

/// The priority of SysTick interrupt.
pub const SYSTICK_PRIORITY: u8 = IRQ_LOW_PRIORITY;
assert_value_type!(SYSTICK_PRIORITY, u8);

// SysTick's priority should fall between allowed IRQ priority levels.
const_assert!(SYSTICK_PRIORITY >= IRQ_MAX_PRIORITY);
const_assert!(SYSTICK_PRIORITY <= IRQ_MIN_PRIORITY);

/// Hopter globally enables or disables interrupts by configuring the BASEPRI
/// register. IRQs with higher priority (smaller numerical value) than BASEPRI
/// are enabled. This value should be set to be lower than all IRQ priority
/// levels.
pub const IRQ_ENABLE_BASEPRI_PRIORITY: u8 = 14 * IRQ_PRIORITY_GRANULARITY;
assert_value_type!(IRQ_ENABLE_BASEPRI_PRIORITY, u8);

/// Hopter globally enables or disables interrupts by configuring the BASEPRI
/// register. IRQs with lower or equal priority (greater or qeual numerical
/// value) than BASEPRI are disabled. This value should be set to be higher or
/// equal than all IRQ priority levels.
pub const IRQ_DISABLE_BASEPRI_PRIORITY: u8 = 6 * IRQ_PRIORITY_GRANULARITY;
assert_value_type!(IRQ_DISABLE_BASEPRI_PRIORITY, u8);

// Setting smaller numerical values to BASEPRI will disable more interrupts.
const_assert!(IRQ_DISABLE_BASEPRI_PRIORITY < IRQ_ENABLE_BASEPRI_PRIORITY);
// The highest priority interrupt must be disabled when the interrupt is
// globally masked.
const_assert!(IRQ_DISABLE_BASEPRI_PRIORITY <= IRQ_MAX_PRIORITY);
// The lowest priority interrupt must be enabled when the interrupt is not
// globally masked.
const_assert!(IRQ_ENABLE_BASEPRI_PRIORITY > IRQ_MIN_PRIORITY);

/// When the interrupt is not globally masked, i.e. the normal case, the SVC
/// is set to a priority lower than all IRQs, so that IRQs can nest above an
/// active SVC and get served promptly.
pub const SVC_NORMAL_PRIORITY: u8 = 12 * IRQ_PRIORITY_GRANULARITY;
assert_value_type!(SVC_NORMAL_PRIORITY, u8);

// SVC should have a lower priority than all IRQs.
const_assert!(SVC_NORMAL_PRIORITY > IRQ_MIN_PRIORITY);
// When the interrupt is not globally masked, SVC should be allowed to execute.
const_assert!(SVC_NORMAL_PRIORITY < IRQ_ENABLE_BASEPRI_PRIORITY);

/// When the interrupt is globally masked, SVC still need to be allowed because
/// growing segmented stacks depend on it. During the period that the interrupt
/// is globally masked, the priority of SVC is raised to keep it higher than
/// BASEPRI.
pub const SVC_RAISED_PRIORITY: u8 = 0 * IRQ_PRIORITY_GRANULARITY;
assert_value_type!(SVC_RAISED_PRIORITY, u8);

// Raised priority should be higher than normal priority.
const_assert!(SVC_RAISED_PRIORITY < SVC_NORMAL_PRIORITY);
// When the interrupt is globally masked, SVC still need to be allowed.
const_assert!(SVC_RAISED_PRIORITY < IRQ_DISABLE_BASEPRI_PRIORITY);

/// PendSV is used to implement context switch. Since an SVC may tail chain a
/// PendSV to perform context switch, PendSV's priority must be lower than SVC.
pub const PENDSV_PRIORITY: u8 = 13 * IRQ_PRIORITY_GRANULARITY;
assert_value_type!(PENDSV_PRIORITY, u8);

// PendSV's priority must be lower than SVC.
const_assert!(PENDSV_PRIORITY > SVC_NORMAL_PRIORITY);
// When the interrupt is not globally masked, PendSV should be allowed to
// execute.
const_assert!(PENDSV_PRIORITY < IRQ_ENABLE_BASEPRI_PRIORITY);

/* ########################### */
/* ### Task Configurations ### */
/* ########################### */

/// The maximum number of tasks. Must be a power of 2.
pub const MAX_TASK_NUMBER: usize = 16;
assert_value_type!(MAX_TASK_NUMBER, usize);

// Maximum task number must be a power of 2 due to internal implementation
// constraints.
const_assert!(helper::is_power_of_2(MAX_TASK_NUMBER as u32));

/// Whether a ready higher priority task should cause a lower priority running
/// task to yield.
pub const ALLOW_TASK_PREEMPTION: bool = true;
assert_value_type!(ALLOW_TASK_PREEMPTION, bool);

/// Maximum priority number. Lower numerical numbers represent higher priorities.
/// Allowed priority range: 0 to (TASK_PRIORITY_LEVELS - 1).
pub const TASK_PRIORITY_LEVELS: u8 = 16;
assert_value_type!(TASK_PRIORITY_LEVELS, u8);

// Should have at least four allowed task priority levels.
const_assert!(TASK_PRIORITY_LEVELS >= 4);

/// The priority of the idle task. Typically this is set to the lowest allowed
/// priority.
pub const IDLE_TASK_PRIORITY: u8 = TASK_PRIORITY_LEVELS - 1;
assert_value_type!(IDLE_TASK_PRIORITY, u8);

// The idle task's priority should be one of the allowed priority levels.
const_assert!(IDLE_TASK_PRIORITY < TASK_PRIORITY_LEVELS);

/// The task priority of the main task.
pub const MAIN_TASK_PRIORITY: u8 = 0;
assert_value_type!(MAIN_TASK_PRIORITY, u8);

// The idle task's priority should be one of the allowed priority levels.
const_assert!(MAIN_TASK_PRIORITY < TASK_PRIORITY_LEVELS);

/// A panicked task will get its priority reduced to the unwind priority,
/// which is very low but still higher than idle priority.
pub const UNWIND_PRIORITY: u8 = TASK_PRIORITY_LEVELS - 3;
assert_value_type!(UNWIND_PRIORITY, u8);

// Unwind priority should be higher than idle priority.
const_assert!(UNWIND_PRIORITY < IDLE_TASK_PRIORITY);
