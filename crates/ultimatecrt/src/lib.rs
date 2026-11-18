//! ultimatecrt library: capture, GPU pipeline, output window and application logic.

pub mod app;
pub mod capture;
pub mod cli;
pub mod diag;
pub mod gpu;
pub mod output;
pub mod scaling;
pub mod sources;

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;
