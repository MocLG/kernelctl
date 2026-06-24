//! The layer between kernelctl and the running system.
//!
//! Everything that touches the machine itself - uname, privileges, mounts,
//! external helpers, and the atomic write primitive - lives here so the
//! bootloader adapters above can stay pure config parsing.

pub mod atomic;
pub mod host;
pub mod privilege;

pub use host::Host;
pub use privilege::Privileges;
