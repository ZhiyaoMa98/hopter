//! Test mutex poisoning upon unwinding.

#![no_main]
#![no_std]

extern crate alloc;
use hopter::{boot::main, config, debug::semihosting, hprintln, sync::Mutex, task};

static MTX: Mutex<()> = Mutex::new(());

#[main]
fn main(_: cortex_m::Peripherals) {
    task::build().set_entry(will_panic).spawn().unwrap();
    task::change_current_priority(config::DEFAULT_TASK_PRIORITY + 1).unwrap();
    if MTX.is_poisoned() {
        hprintln!("Test Passed");
        semihosting::terminate(true);
    } else {
        hprintln!("Test Failed");
        semihosting::terminate(false);
    }
}

fn will_panic() {
    let _gurad = MTX.lock();
    panic!();
}
