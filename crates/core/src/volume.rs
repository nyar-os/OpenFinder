//! Raw volume access and platform dispatch.
//!
//! Indexing reads the volume device, not the mounted file tree. On Windows that
//! is `\\.\C:` through `CreateFileW`. Other targets report the stable
//! `unsupported_platform` error instead of substituting a directory walk.

use crate::{error::Result, ntfs::BootSector};

/// Sequential byte reader over a volume.
pub trait VolumeReader {
    /// Read exactly `buffer.len()` bytes at `offset`.
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()>;
}

/// A raw NTFS volume handle.
pub struct Volume {
    #[cfg(windows)]
    inner: win::RawVolume,
    #[cfg(not(windows))]
    _private: (),
}

impl Volume {
    /// Open a raw volume such as `C:` or `\\.\C:`.
    pub fn open(volume: &str) -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self { inner: win::RawVolume::open(volume)? })
        }
        #[cfg(not(windows))]
        {
            let _ = volume;
            Err(crate::error::Error::capability(
                "index-build",
                crate::error::CapabilityState::UnsupportedPlatform,
                "raw NTFS volume access is only implemented on Windows",
            ))
        }
    }

    /// Read and parse the NTFS boot sector.
    pub fn boot_sector(&mut self) -> Result<BootSector> {
        let mut sector = vec![0u8; 512];
        self.read_at(0, &mut sector)?;
        BootSector::parse(&sector)
    }

    /// Issue a control code against the volume.
    pub fn ioctl(&mut self, code: u32, input: &[u8], output_len: u32) -> Result<Vec<u8>> {
        #[cfg(windows)]
        {
            self.inner.ioctl(code, input, output_len)
        }
        #[cfg(not(windows))]
        {
            let _ = (code, input, output_len);
            Err(crate::error::Error::capability(
                "index-update",
                crate::error::CapabilityState::UnsupportedPlatform,
                "volume control codes are only implemented on Windows",
            ))
        }
    }

    /// Issue a control code that signals end-of-data with `ERROR_HANDLE_EOF`.
    pub fn ioctl_optional(&mut self, code: u32, input: &[u8], output_len: u32) -> Result<Option<Vec<u8>>> {
        #[cfg(windows)]
        {
            self.inner.ioctl_optional(code, input, output_len)
        }
        #[cfg(not(windows))]
        {
            let _ = (code, input, output_len);
            Err(crate::error::Error::capability(
                "index-update",
                crate::error::CapabilityState::UnsupportedPlatform,
                "volume control codes are only implemented on Windows",
            ))
        }
    }
}

impl VolumeReader for Volume {
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.read_at(offset, buffer)
        }
        #[cfg(not(windows))]
        {
            let _ = (offset, buffer);
            Err(crate::error::Error::capability(
                "index-build",
                crate::error::CapabilityState::UnsupportedPlatform,
                "raw NTFS volume access is only implemented on Windows",
            ))
        }
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use crate::error::{CapabilityState, Error, Result};

    use super::VolumeReader;

    type Handle = isize;

    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
    const FILE_BEGIN: u32 = 0;
    const INVALID_HANDLE_VALUE: Handle = -1;
    /// `ERROR_HANDLE_EOF`: the file-system control ran out of data.
    const ERROR_HANDLE_EOF: u32 = 38;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            file_name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: Handle,
        ) -> Handle;
        fn ReadFile(handle: Handle, buffer: *mut c_void, bytes_to_read: u32, bytes_read: *mut u32, overlapped: *mut c_void) -> i32;
        fn SetFilePointerEx(handle: Handle, distance: i64, new_position: *mut i64, method: u32) -> i32;
        fn DeviceIoControl(
            handle: Handle,
            code: u32,
            in_buffer: *const c_void,
            in_size: u32,
            out_buffer: *mut c_void,
            out_size: u32,
            bytes_returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetLastError() -> u32;
    }

    /// An owned raw volume handle.
    pub struct RawVolume {
        handle: Handle,
    }

    impl RawVolume {
        /// Open the raw device for a volume label such as `C:`.
        pub fn open(volume: &str) -> Result<Self> {
            let device =
                if volume.starts_with("\\\\") { volume.to_string() } else { format!("\\\\.\\{}", volume.trim_end_matches(['\\', '/'])) };
            let wide = encode_utf16(&device);
            // SAFETY: the wide string outlives the call and the remaining arguments are null or plain integers.
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    0,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                let code = unsafe { GetLastError() };
                let detail = format!("cannot open raw volume {device} (Win32 error {code}); indexing $MFT requires an elevated process");
                return Err(crate::error::Error::capability("index-build", CapabilityState::DependencyBlocked, detail));
            }
            Ok(Self { handle })
        }

        /// Read exactly `buffer.len()` bytes.
        pub fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
            let distance = i64::try_from(offset).map_err(|_| Error::message("volume offset exceeds the signed 64-bit range"))?;
            // SAFETY: the handle is valid for the lifetime of `self` and the position pointer is a valid stack slot.
            let moved = unsafe { SetFilePointerEx(self.handle, distance, std::ptr::null_mut(), FILE_BEGIN) };
            if moved == 0 {
                // SAFETY: GetLastError has no preconditions.
                return Err(self.error_for("seek", unsafe { GetLastError() }));
            }
            let mut written = 0usize;
            while written < buffer.len() {
                let remaining = u32::try_from(buffer.len() - written).map_err(|_| Error::message("single read exceeds 4 GiB"))?;
                let mut read = 0u32;
                // SAFETY: the destination slice covers `remaining` bytes and `read` is a valid out parameter.
                let ok = unsafe { ReadFile(self.handle, buffer[written..].as_mut_ptr().cast(), remaining, &mut read, std::ptr::null_mut()) };
                if ok == 0 {
                    // SAFETY: GetLastError has no preconditions.
                    return Err(self.error_for("read", unsafe { GetLastError() }));
                }
                if read == 0 {
                    return Err(Error::message("volume read returned no bytes"));
                }
                written += read as usize;
            }
            Ok(())
        }

        /// Issue a control code against the volume handle.
        pub fn ioctl(&mut self, code: u32, input: &[u8], output_len: u32) -> Result<Vec<u8>> {
            self.ioctl_code(code, input, output_len).map_err(|error_code| self.error_for("control", error_code))
        }

        /// Issue a control code that reports `ERROR_HANDLE_EOF` when it runs out of data.
        pub fn ioctl_optional(&mut self, code: u32, input: &[u8], output_len: u32) -> Result<Option<Vec<u8>>> {
            match self.ioctl_code(code, input, output_len) {
                Ok(output) => Ok(Some(output)),
                Err(ERROR_HANDLE_EOF) => Ok(None),
                Err(code) => Err(self.error_for("control", code)),
            }
        }

        fn ioctl_code(&mut self, code: u32, input: &[u8], output_len: u32) -> std::result::Result<Vec<u8>, u32> {
            let mut output = vec![0u8; output_len as usize];
            let mut returned = 0u32;
            // SAFETY: input and output buffers are valid for their stated lengths for the duration of the call.
            let ok = unsafe {
                DeviceIoControl(
                    self.handle,
                    code,
                    input.as_ptr().cast(),
                    input.len() as u32,
                    output.as_mut_ptr().cast(),
                    output_len,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                // SAFETY: GetLastError has no preconditions.
                return Err(unsafe { GetLastError() });
            }
            output.truncate(returned as usize);
            Ok(output)
        }

        fn error_for(&self, action: &str, code: u32) -> Error {
            let mut error = Error::message(format!("volume {action} failed with Win32 error {code}"));
            if code == 5 {
                error = Error::capability(
                    "index-build",
                    CapabilityState::DependencyBlocked,
                    format!("volume {action} was denied (Win32 error 5); run from an elevated process"),
                );
            }
            error
        }
    }

    impl VolumeReader for RawVolume {
        fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
            RawVolume::read_at(self, offset, buffer)
        }
    }

    impl Drop for RawVolume {
        fn drop(&mut self) {
            // SAFETY: the handle is owned by this value and closed exactly once.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    fn encode_utf16(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl VolumeReader for Vec<u8> {
        fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
            let start = offset as usize;
            let end = start + buffer.len();
            if end > self.len() {
                return Err(crate::error::Error::message("memory volume read past the end"));
            }
            buffer.copy_from_slice(&self[start..end]);
            Ok(())
        }
    }

    #[test]
    fn memory_volume_reads_ranges() {
        let mut volume: Vec<u8> = (0..16).collect();
        let mut buffer = [0u8; 4];
        volume.read_at(4, &mut buffer).unwrap();
        assert_eq!(buffer, [4, 5, 6, 7]);
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_platform_is_reported_not_faked() {
        let error = Volume::open("C:").err().unwrap();
        assert_eq!(error.code(), "unsupported_platform");
    }
}
