//! Services: the singleton instances under `game` that scripts reach with
//! `game:GetService(name)`. Each area keeps its services in its own module.

pub mod core;
pub mod data;
pub mod extra;
