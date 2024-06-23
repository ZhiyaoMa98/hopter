//! Test timeout waiting on a mailbox. The timeout is deliberate.

#![no_std]
#![no_main]

extern crate alloc;
use hopter::{boot::main, debug::semihosting, hprintln, schedule, sync::Mailbox};

static MAILBOX: Mailbox = Mailbox::new();

#[main]
fn main(_: cortex_m::Peripherals) {
    schedule::start_task(2, |_| listener(), (), 0, 4).unwrap();
}

fn listener() {
    let notified = MAILBOX.wait_until_timeout(1000);
    if notified {
        hprintln!("Unexpected notification.");
        semihosting::terminate(false);
    }
    semihosting::terminate(true);
}
