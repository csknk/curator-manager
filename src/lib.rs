//! A small batch reconstruction pipeline: feed -> canonical events -> state -> report.
pub mod error;
pub mod event;
pub mod feed;
pub mod plan;
pub mod policy;
pub mod report;
pub mod state;
pub use feed::reconstruct;
