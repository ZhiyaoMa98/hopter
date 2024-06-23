use crate::schedule;

pub trait Lethal {
    type T;
    fn unwrap_or_die(self) -> Self::T;
}

impl<T, E> Lethal for Result<T, E> {
    type T = T;

    fn unwrap_or_die(self) -> Self::T {
        self.unwrap_or_else(|arg| die_with_arg(arg))
    }
}

impl<T> Lethal for Option<T> {
    type T = T;
    fn unwrap_or_die(self) -> Self::T {
        self.unwrap_or_else(|| die())
    }
}

pub fn die() -> ! {
    loop {}
}

pub fn die_with_arg<T>(_arg: T) -> ! {
    loop {}
}

pub fn die_if<F>(cond: F)
where
    F: Fn() -> bool,
{
    if cond() {
        die()
    }
}

pub fn die_if_in_isr() {
    if schedule::is_running_in_isr() {
        die();
    }
}
