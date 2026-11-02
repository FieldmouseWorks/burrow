//! HOST-boot as the machine runs it: burrow-boot CONFIG WORD, on a machine
//! made of files (machine.rs). Every run is as someone other than root (as
//! nobody, when the tests run as root), so it goes through sudo wherever
//! the tests run.

mod check;
mod default;
mod machine;
mod trial;
