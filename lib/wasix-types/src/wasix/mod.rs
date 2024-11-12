#[cfg(feature = "enable-serde")]
use serde::*;

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "enable-serde", serde(rename_all = "snake_case"))]
pub enum ThreadStartType {
    MainThread,
    ThreadSpawn { start_ptr: u64 },
}
