//! Generic external-command sidebar list section.
//!
//! Contains no SLURM (or any other provider's) knowledge. `protocol` parses and
//! validates the JSON a provider script prints to stdout.

pub mod protocol;
pub mod substitute;
