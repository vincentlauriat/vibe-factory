//! Security layer for shell execution: command parsing, policy and output
//! hygiene.

pub mod command_parser;
pub mod output;
pub mod policy;

pub use command_parser::{CommandSegment, ParseError, parse_command};
pub use output::{is_probably_binary, truncate_output};
pub use policy::SecurityPolicy;
