#![no_main]
#![no_std]

extern crate alloc;
use hopter::{boot::main, debug::semihosting, schedule, sync::Semaphore};
use core::sync::atomic::{AtomicUsize, Ordering};
use cortex_m::asm::delay;

static SEMAPHORE: Semaphore = Semaphore::new(10, 5);
static TASK_COMPLETION_COUNTER: AtomicUsize = AtomicUsize::new(0);
const TOTAL_TASKS: usize = 10;

#[main]
fn main(_: cortex_m::Peripherals) {
    // Start 10 tasks
    for _ in 0..TOTAL_TASKS {
        schedule::start_task(2, move |_| task(), (), 0, 4).unwrap();
    }

    // Loop to check for task completion
    loop {
        let completed_tasks = TASK_COMPLETION_COUNTER.load(Ordering::SeqCst);
        semihosting::hprintln!("Completed tasks: {}", completed_tasks).unwrap();

        if completed_tasks == TOTAL_TASKS {
            // All tasks completed, check semaphore count
            let final_count = SEMAPHORE.count();
            // Check if the count matches the initial value
            if final_count == 5 {
                semihosting::terminate(true);
            } else {
                semihosting::terminate(false);
            }
        }

        // Introduce a small delay
        delay(100_000);
    }
}

// Task function that will run independently
fn task() {
    for _ in 0..10 {
        SEMAPHORE.up();
        SEMAPHORE.down();
    }
    // Increment the task completion counter
    TASK_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
}


// #![no_main]
// #![no_std]

// extern crate alloc;
// use hopter::{boot::main, debug::semihosting, schedule, sync::Semaphore};
// use core::sync::atomic::{AtomicUsize, Ordering};

// static SEMAPHORE: Semaphore = Semaphore::new(10, 5);
// static TASK_COMPLETION_COUNTER: AtomicUsize = AtomicUsize::new(0);
// const TOTAL_TASKS: usize = 10;

// #[main]
// fn main(_: cortex_m::Peripherals) {
//     // Start 100 tasks
//     for _ in 0..TOTAL_TASKS {
//         schedule::start_task(2, move |_| task(), (), 0, 4).unwrap();
//     }

//     // loop to check for task completion
//     loop {
//         if TASK_COMPLETION_COUNTER.load(Ordering::SeqCst) == TOTAL_TASKS {
//             // All tasks completed, check semaphore count
//             let final_count = SEMAPHORE.count();
//              // Check if the count matches the initial value
//             if final_count == 5 {
//                 semihosting::terminate(true);
//             }
//             else {
//                 semihosting::terminate(false);
//             }
//         }
//         break;
//     }
// }

// // Task function that will run independently
// fn task() {
//     for _ in 0..10 {
//         SEMAPHORE.up();
//         SEMAPHORE.down();
//     }
//     // Increment the task completion counter
//     TASK_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
// }
