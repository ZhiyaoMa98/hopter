#![no_std]
#![feature(core_intrinsics)]
#![feature(naked_functions)]
#![feature(asm_const)]
#![feature(lang_items)]
#![feature(negative_impls)]
#![allow(internal_features)]
#![feature(new_uninit)]
#![feature(unchecked_math)]
#![feature(raw_ref_op)]
#![feature(alloc_error_handler)]
#![feature(offset_of)]
#![feature(arbitrary_self_types)]

extern crate alloc;

pub mod allocator;
mod assembly;
pub mod boot;
pub mod config;
pub mod debug;
pub mod interrupt;
pub mod schedule;
pub mod sync;
pub mod task;
pub mod time;
mod unrecoverable;
pub mod unwind;
