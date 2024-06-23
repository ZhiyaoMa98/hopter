//! Test waiting with timeout but getting notified before timeout.

#![no_std]
#![no_main]

extern crate alloc;
use hopter::{boot::main, debug::semihosting, hprintln, schedule, sync::Mailbox, time};

static MAILBOX: Mailbox = Mailbox::new();

#[main]
fn main(_: cortex_m::Peripherals) {
    MAILBOX.notify_allow_isr();
    schedule::start_task(2, |_| listener(), (), 0, 4).unwrap();
    schedule::start_task(2, |_| notifier(), (), 0, 8).unwrap();
}

fn listener() {
    let notified = MAILBOX.wait_until_timeout(1000);
    if !notified {
        hprintln!("Unexpected timeout.");
        semihosting::terminate(false);
    }

    let notified = MAILBOX.wait_until_timeout(1000);
    if !notified {
        hprintln!("Unexpected timeout.");
        semihosting::terminate(false);
    }

    semihosting::terminate(true);
}

fn notifier() {
    time::sleep_ms(500);
    MAILBOX.notify_allow_isr();
}
