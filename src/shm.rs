//! Shared memory frame buffer using /dev/shm.
//! PipeWire writes frames here; the renderer reads via SharedArrayBuffer.
//! Double-buffered: write to one while renderer reads the other.

use std::sync::atomic::{AtomicU64, AtomicU32, Ordering};

const SHM_NAME: &str = "/pipecap-frames";

/// Header stored at the start of the shared memory region.
/// Renderer polls `seq` to detect new frames.
#[repr(C)]
pub struct ShmHeader {
    /// Incremented on each new frame. Renderer compares to detect updates.
    pub seq: AtomicU64,
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub stride: AtomicU32,
    /// Offset into the shm region where the current frame data starts.
    pub data_offset: AtomicU32,
    pub data_size: AtomicU32,
}

pub struct ShmBuffer {
    ptr: *mut u8,
    size: usize,
    fd: i32,
}

// SAFETY: the shared memory region is accessed via atomic operations
unsafe impl Send for ShmBuffer {}
unsafe impl Sync for ShmBuffer {}

impl ShmBuffer {
    /// Create a new shared memory buffer.
    /// `max_frame_size` is the max size of a single frame in bytes.
    /// Total allocation = header + 2 * max_frame_size (double buffer).
    pub fn new(max_frame_size: usize) -> anyhow::Result<Self> {
        let header_size = std::mem::size_of::<ShmHeader>();
        let total_size = header_size + max_frame_size * 2;

        // Remove any stale shm
        let name = std::ffi::CString::new(SHM_NAME)?;
        unsafe { libc::shm_unlink(name.as_ptr()); }
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_RDWR,
                0o600,
            )
        };
        if fd < 0 {
            anyhow::bail!("shm_open failed: {}", std::io::Error::last_os_error());
        }

        if unsafe { libc::ftruncate(fd, total_size as libc::off_t) } < 0 {
            unsafe { libc::close(fd); }
            anyhow::bail!("ftruncate failed: {}", std::io::Error::last_os_error());
        }

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                total_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            unsafe { libc::close(fd); }
            anyhow::bail!("mmap failed: {}", std::io::Error::last_os_error());
        }

        // Zero out the header
        unsafe {
            std::ptr::write_bytes(ptr as *mut u8, 0, header_size);
        }

        Ok(ShmBuffer {
            ptr: ptr as *mut u8,
            size: total_size,
            fd,
        })
    }

    pub fn size(&self) -> usize {
        self.size
    }

    fn header(&self) -> &ShmHeader {
        unsafe { &*(self.ptr as *const ShmHeader) }
    }

    fn header_size(&self) -> usize {
        std::mem::size_of::<ShmHeader>()
    }

    fn max_frame_size(&self) -> usize {
        (self.size - self.header_size()) / 2
    }

    /// Write a frame into the shared memory region.
    /// Alternates between two slots (double buffer).
    pub fn write_frame(&self, width: u32, height: u32, stride: u32, data: &[u8]) {
        let max = self.max_frame_size();
        if data.len() > max {
            return; // Frame too large
        }

        let header = self.header();
        let seq = header.seq.load(Ordering::Relaxed);

        // Alternate between slot 0 and slot 1
        let slot = (seq as usize + 1) % 2;
        let offset = self.header_size() + slot * max;

        // Copy frame data
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.ptr.add(offset),
                data.len(),
            );
        }

        // Update header atomically (write data first, then seq last as release fence)
        header.width.store(width, Ordering::Relaxed);
        header.height.store(height, Ordering::Relaxed);
        header.stride.store(stride, Ordering::Relaxed);
        header.data_offset.store(offset as u32, Ordering::Relaxed);
        header.data_size.store(data.len() as u32, Ordering::Relaxed);
        header.seq.store(seq + 1, Ordering::Release);
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.size);
            libc::close(self.fd);
            let name = std::ffi::CString::new(SHM_NAME).unwrap();
            libc::shm_unlink(name.as_ptr());
        }
    }
}
