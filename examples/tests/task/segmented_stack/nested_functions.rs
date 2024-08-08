#![no_std]
#![no_main]
#![feature(naked_functions)]

extern crate alloc;
use core::arch::asm;
use hopter::{boot::main, debug::semihosting, hprintln, schedule};


#[naked]
extern "C" fn nested_function_calls() {
    unsafe {
        asm!(
            "mov.w  r12, #0x20000000",  /* take stacklet boundary address */
            "ldr.w  r12, [r12]",   /* read stacklet boundary */
            "subs.w r12, sp, r12",     /* calculate remaining free size */
            "cmp.w  r12, #1000",         /* compare with requested size */
            "bge.n  1f",        /* if enough free space, goto func_body */
            "svc    #255",             /* otherwise, invoke SVC */
            ".short 250", /* function_stack_size (right shifted by 2)  <- preserved PC */
            ".short 4", /* function_arg_size (right shifted by 2)  ; <- PC + 2 */

            "1:",
            "push {{lr}}", 
            "mov r0, #0", 
            "push {{r0}}", 
            "bl {inner_function1}",
            "pop {{r0, lr}}",
            "bx lr",
            inner_function1 = sym inner_function1,
            options(noreturn)
        )
    }
}

#[naked]
extern "C" fn inner_function1() {
    unsafe {
        asm!(
            "mov.w  r12, #0x20000000",  /* take stacklet boundary address */
            "ldr.w  r12, [r12]",   /* read stacklet boundary */
            "subs.w r12, sp, r12",     /* calculate remaining free size */
            "cmp.w  r12, #1000",         /* compare with requested size */
            "bge.n  1f",        /* if enough free space, goto func_body */
            "svc    #255",             /* otherwise, invoke SVC */
            ".short 250", /* function_stack_size (right shifted by 2)  <- preserved PC */
            ".short 4", /* function_arg_size (right shifted by 2)  ; <- PC + 2 */

            "1:",
            "push {{lr}}",
            "mov r1, #1",
            "push {{r1}}",
            "bl {inner_function2}",
            "pop {{r1, lr}}",
            "bx lr",
            inner_function2 = sym inner_function2,
            options(noreturn)
        )
    }
}

#[naked]
extern "C" fn inner_function2() {
    unsafe {
        asm!(
            "mov.w  r12, #0x20000000",  /* take stacklet boundary address */
            "ldr.w  r12, [r12]",   /* read stacklet boundary */
            "subs.w r12, sp, r12",     /* calculate remaining free size */
            "cmp.w  r12, #1000",         /* compare with requested size */
            "bge.n  1f",        /* if enough free space, goto func_body */
            "svc    #255",             /* otherwise, invoke SVC */
            ".short 250", /* function_stack_size (right shifted by 2)  <- preserved PC */
            ".short 4", /* function_arg_size (right shifted by 2)  ; <- PC + 2 */

            "1:",

            "push {{lr}}",

            "mov r2, #2",
            "push {{r2}}",
            "bl {verify_arguments}",
            "pop {{r2, lr}}",
            "bx lr",
            verify_arguments = sym verify_arguments,
            options(noreturn)
        )
    }
}

#[naked]
extern "C" fn verify_arguments() {
    unsafe {
        asm!(
            "mov.w  r12, #0x20000000",  /* take stacklet boundary address */
            "ldr.w  r12, [r12]",   /* read stacklet boundary */
            "subs.w r12, sp, r12",     /* calculate remaining free size */
            "cmp.w  r12, #1000",         /* compare with requested size */
            "bge.n  1f",        /* if enough free space, goto func_body */
            "svc    #255",             /* otherwise, invoke SVC */
            ".short 250", /* (right shifted by 2)  <- preserved PC */
            ".short 4",   /* (right shifted by 2)" ; <- PC + 2 */
            
            "1:",   
            "push {{r4-r5, lr}}",

            /* verify arguments on stack */
            "ldr r3, [sp, #12]", /* Check last pushed value (r3). Load the value into r3 */
            "cmp r3, #2",             
            "bne {error}",

            "ldr r4, [sp, #20]", /* Check second last pushed value (r2). Load the value into r4 */
            "cmp r4, #1",             
            "bne {error}",

            "ldr r5, [sp, #28]", /*Check third last pushed value (r1). Load the value into r5 */
            "cmp r5, #0",             
            "bne {error}",


            "bl {print_success}",
            "pop {{r4-r5,lr}}",
            "bx lr",

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
    schedule::start_task(2, |_| nested_function_calls(), (), 0, 4).unwrap();
    schedule::change_current_task_priority(10).unwrap();
    semihosting::terminate(true);
}
