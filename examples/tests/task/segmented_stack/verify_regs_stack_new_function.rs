
#![no_std]
#![no_main]
#![feature(naked_functions)]

extern crate alloc;
use core::arch::asm;
use hopter::{boot::main, debug::semihosting, hprintln, schedule};


#[naked]
extern "C" fn prepare_regs_stack() {
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
            "push {{r4-r7, lr}}",
            /* Prepare arguments in registers */
            "mov r0, #1",
            "mov r1, #2",
            "mov r2, #3",
            "mov r3, #4",


            /* Prepare arguments on stack */
            "mov r4, #5",   
            "mov  r5, #6",  
            "mov r6, #7",   
            "mov r7, #8",   
            "push {{r4, r5, r6, r7}}",/* Push values onto the stack in reverse order */

            /* call verify_arguments function */
            "bl {verify_arguments}",
            "pop {{r4, r5, r6, r7}}", 
            "pop {{r4-r7,lr}}",
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
            "bge.n  2f",        /* if enough free space, goto func_body */
            "svc    #255",             /* otherwise, invoke SVC */
            ".short 250", /* (right shifted by 2)  <- preserved PC */
            ".short 4",   /* (right shifted by 2)" ; <- PC + 2 */
            
            "2:",    
            "push {{r4-r7, lr}}",      

            /* verify arguments in registers */
            "cmp r0, #1",
            "bne {error}",
            "cmp r1, #2",
            "bne {error}",
            "cmp r2, #3",
            "bne {error}",
            "cmp r3, #4",
            "bne {error}",

            /* verify arguments on stack */
            "ldr r8, [sp, #12]", /* Check last pushed value (r7). Load the value into r8 */
            "cmp r8, #8",             
            "bne {error}",

            "ldr r9, [sp, #8]", /* Check second last pushed value (r6). Load the value into r9 */
            "cmp r9, #7",             
            "bne {error}",

            "ldr r10, [sp, #4]", /*Check third last pushed value (r5). Load the value into r10 */
            "cmp r10, #6",             
            "bne {error}",

            "ldr r11, [sp, #0]",  /* Check first pushed value (r4). Load the value into r11 */
            "cmp r11, #5",             
            "bne {error}",

            "bl {print_success}",
            "pop {{r4-r7,lr}}",
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

    schedule::start_task(2, |_| prepare_regs_stack(), (), 0, 4).unwrap();
    schedule::change_current_task_priority(10).unwrap();
  
    semihosting::terminate(true);

}





