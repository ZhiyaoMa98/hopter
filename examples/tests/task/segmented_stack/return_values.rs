#![no_std]
#![no_main]
#![feature(naked_functions)]

extern crate alloc;
use core::arch::asm;
use hopter::{boot::main, debug::semihosting, hprintln, schedule};



#[naked]
extern "C" fn callee() {
    unsafe {
        asm!(
            "mov.w  r12, #0x20000000",  /* take stacklet boundary address */
            "ldr.w  r12, [r12]",   /* read stacklet boundary */
            "subs.w r12, sp, r12",     /* calculate remaining free size */
            "cmp.w  r12, #100",         /* compare with requested size */
            "bge.n  1f",        /* if enough free space, goto func_body */
            "svc    #255",             /* otherwise, invoke SVC */
            ".short 25", /* function_stack_size (right shifted by 2)  <- preserved PC */
            ".short 4", /* function_arg_size (right shifted by 2)  ; <- PC + 2 */

            "1:", 
            // set r0-r3 to known values and return to caller
            "mov r0, #25", 
            "mov r1, #26", 
            "mov r2, #27", 
            "mov r3, #28", 
            "bx lr",

            options(noreturn)
        )
    }
}


#[naked]
extern "C" fn caller() {
    unsafe {
        asm!(
            "mov.w  r12, #0x20000000",  /* take stacklet boundary address */
            "ldr.w  r12, [r12]",   /* read stacklet boundary */
            "subs.w r12, sp, r12",     /* calculate remaining free size */
            "cmp.w  r12, #100",         /* compare with requested size */
            "bge.n  1f",        /* if enough free space, goto func_body */
            "svc    #255",             /* otherwise, invoke SVC */
            ".short 25", /* function_stack_size (right shifted by 2)  <- preserved PC */
            ".short 4",   /* function_arg_size (right shifted by 2)" ; <- PC + 2 */
            
            "1:",   
            "push {{lr}}",
            "bl {callee_function}",

            // verify return values in registers r0-r3
            "cmp r0, #25",
            "bne {error}",
            "cmp r1, #26",
            "bne {error}",
            "cmp r2, #27",
            "bne {error}",
            "cmp r3, #28",
            "bne {error}",

            "bl {print_success}",
            "pop {{lr}}",
            "bx lr",

            callee_function = sym callee,
            error = sym error,
            print_success = sym success,
            options(noreturn)

        )

    }
}


extern "C" fn success() {
    hprintln!("Test Passed");
}

extern "C" fn error() {
    hprintln!("Test Failed");
}



#[main]
fn main(_: cortex_m::Peripherals) {
    schedule::start_task(2, |_| caller(), (), 0, 4).unwrap();
    schedule::change_current_task_priority(10).unwrap();
    semihosting::terminate(true);
}