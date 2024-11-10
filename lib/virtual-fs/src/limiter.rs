use std::sync::Arc;

use crate::FsError;

pub use self::tracked_vec::TrackedVec;

/// Allows tracking and limiting the memory usage of a memfs [`FileSystem`].
pub trait FsMemoryLimiter: Send + Sync + std::fmt::Debug {
    fn on_grow(&self, grown_bytes: usize) -> std::result::Result<(), FsError>;
    fn on_shrink(&self, shrunk_bytes: usize);
}

pub type DynFsMemoryLimiter = Arc<dyn FsMemoryLimiter + Send + Sync>;

mod tracked_vec {
    use crate::FsError;

    use super::DynFsMemoryLimiter;

    #[derive(Debug)]
    pub struct TrackedVec {
        data: Vec<u8>,
    }

    impl TrackedVec {
        pub fn new(_limiter: Option<DynFsMemoryLimiter>) -> Self {
            Self { data: Vec::new() }
        }

        pub fn limiter(&self) -> Option<&DynFsMemoryLimiter> {
            None
        }

        pub fn with_capacity(
            capacity: usize,
            _limiter: Option<DynFsMemoryLimiter>,
        ) -> Result<Self, FsError> {
            Ok(Self {
                data: Vec::with_capacity(capacity),
            })
        }

        pub fn clear(&mut self) {
            self.data.clear();
        }

        pub fn append(&mut self, other: &mut Self) -> Result<(), FsError> {
            self.data.append(&mut other.data);
            Ok(())
        }

        pub fn split_off(&mut self, at: usize) -> Result<Self, FsError> {
            let other = self.data.split_off(at);
            Ok(Self { data: other })
        }

        pub fn resize(&mut self, new_len: usize, value: u8) -> Result<(), FsError> {
            self.data.resize(new_len, value);
            Ok(())
        }

        pub fn extend_from_slice(&mut self, other: &[u8]) -> Result<(), FsError> {
            self.data.extend_from_slice(other);
            Ok(())
        }

        pub fn reserve_exact(&mut self, additional: usize) -> Result<(), FsError> {
            self.data.reserve_exact(additional);
            Ok(())
        }
    }

    impl std::ops::Deref for TrackedVec {
        type Target = Vec<u8>;

        fn deref(&self) -> &Self::Target {
            &self.data
        }
    }

    impl std::ops::DerefMut for TrackedVec {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.data
        }
    }
}
