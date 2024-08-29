#![no_main]
#![no_std]

extern crate alloc;
use hopter::{
    debug::semihosting::{self, dbg_println},
    sync::Semaphore,
    task,
    task::main,
};

static SEMAPHORE: Semaphore = Semaphore::new(1, 0);

#[main]
fn main(_: cortex_m::Peripherals) {
    task::build().set_entry(acquire).spawn().unwrap();
    task::build().set_entry(release).spawn().unwrap();
}

fn acquire() {
    dbg_println!("attempting to acquire semaphore..");
    SEMAPHORE.down();
    dbg_println!("semaphore acquired");
    // semihosting::terminate(true);
    dbg_println!("test complete!");
    loop {}
}

fn release() {
    SEMAPHORE.up();
    dbg_println!("semaphore released");
}
