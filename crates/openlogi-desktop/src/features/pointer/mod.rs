//! Pointer sensitivity and wheel controls.

pub mod dpi;
mod glide_shape;
pub mod momentum;
mod os_pointer_speed;
pub use os_pointer_speed::os_pointer_speed_section;
mod pointer_speed;
mod report_rate;
pub mod smartshift;
