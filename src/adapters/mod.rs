#[cfg(target_os = "linux")]
mod process_sysinfo;
mod system_clock;

#[cfg(target_os = "linux")]
pub use process_sysinfo::SysinfoProcessSource;
pub use system_clock::SystemClock;
