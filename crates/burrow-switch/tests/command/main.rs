//! HOST-switch and HOST-confirm as the machine runs them: burrow-switch
//! CONFIG [WORDS] and burrow-confirm CONFIG, on a machine made of files
//! (machine.rs). Every run is as someone other than root (as nobody, when
//! the tests run as root), so it goes through sudo wherever they run.
//!
//! What the switch's units do on a real machine, the activation killed at
//! its deadline and the revert landing inside the window, is the VM test's
//! (tests/switch.nix).

mod confirm;
mod machine;
mod queue;
mod switch;
