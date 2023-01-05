#[cfg(target_os = "linux")]
pub use linux::create_tun;
#[cfg(target_os = "macos")]
pub use mac::create_tun;
#[cfg(any(unix))]
pub use unix::{TunReader, TunWriter};
#[cfg(target_os = "windows")]
pub use windows::{TunReader, TunWriter};
#[cfg(target_os = "windows")]
pub use windows::create_tun;

#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(any(unix))]
pub mod unix;
#[cfg(target_os = "windows")]
pub mod windows;
