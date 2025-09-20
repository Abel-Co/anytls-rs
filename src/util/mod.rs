pub mod deadline;
pub mod mkcert;
pub mod routine;
pub mod string_map;
pub mod r#type;
pub mod version;

// 高性能模块
pub mod memory_pool;
pub mod ebpf;
pub mod high_perf_io;

pub use version::PROGRAM_VERSION_NAME;
pub use memory_pool::{MemoryPool, Buffer, ZeroCopyForwarder};
pub use ebpf::{EbpfMonitor, KernelNetworkOptimizer, NetworkStats, PerformanceCounter};
pub use high_perf_io::{HighPerfIoManager, HighPerfTcpConnection, HighPerfTcpListener, AsyncIoOptimizer, MmapIo};
