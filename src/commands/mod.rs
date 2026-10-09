//! Commands: the `/` palette, the dispatcher and the commands themselves.

mod admin;
mod dispatch;
mod help;
mod ops;
mod palette;
mod work;

pub(crate) use dispatch::check_model;
pub(crate) use help::{COMMANDS, HELP_END, shown};
pub use palette::*;
