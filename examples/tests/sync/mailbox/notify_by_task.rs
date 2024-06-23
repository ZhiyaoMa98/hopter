//! Test notifying a task from another task. The waiting task starts its wait
//! before the notifying task notifies.

#![no_std]
#![no_main]

extern crate alloc;
use hopter::{boot::main, debug::semihosting, hprintln, schedule, sync::Mailbox, time};

static MAILBOX: Mailbox = Mailbox::new();

#[main]
fn main(_: cortex_m::Peripherals) {
    schedule::start_task(2, |_| notifier(), (), 0, 4).unwrap();
    schedule::start_task(2, |_| listener(), (), 0, 4).unwrap();
}

fn notifier() {
    for _ in 0..5 {
        time::sleep_ms(250);
        MAILBOX.notify_allow_isr();
    }
}

fn listener() {
    for _ in 0..5 {
        MAILBOX.wait();
        hprintln!("received");
    }

    semihosting::terminate(true);
}
