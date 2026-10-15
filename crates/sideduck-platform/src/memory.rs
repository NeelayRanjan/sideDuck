use std::io;

/// Operating-system process residency, including allocations outside the tile cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessMemory {
    pub resident_bytes: u64,
    /// The kernel's lifetime high-water mark, not the largest sampled value.
    pub peak_resident_bytes: u64,
}

pub fn process_memory() -> io::Result<ProcessMemory> {
    #[cfg(target_os = "macos")]
    {
        macos_memory()
    }
    #[cfg(target_os = "linux")]
    {
        parse_linux_status(&std::fs::read_to_string("/proc/self/status")?)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Process residency measurement requires macOS or Linux",
        ))
    }
}

#[cfg(target_os = "macos")]
fn macos_memory() -> io::Result<ProcessMemory> {
    let mut task = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    // SAFETY: The kernel writes exactly the checked struct size into this buffer.
    let written = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            task.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: proc_pidinfo returned the full initialized size above.
    let task = unsafe { task.assume_init() };
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: A successful getrusage initializes the supplied rusage struct.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Return status was checked above. macOS reports ru_maxrss in bytes.
    let peak = unsafe { usage.assume_init() }.ru_maxrss;
    let peak =
        u64::try_from(peak).map_err(|_| io::Error::other("Negative resident high-water mark"))?;
    Ok(ProcessMemory {
        resident_bytes: task.pti_resident_size,
        peak_resident_bytes: peak.max(task.pti_resident_size),
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_status(status: &str) -> io::Result<ProcessMemory> {
    fn field(status: &str, key: &str) -> io::Result<u64> {
        let value = status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|line| {
                let mut words = line.split_whitespace();
                let value = words.next()?.parse::<u64>().ok()?;
                (words.next()? == "kB").then_some(value)
            })
            .and_then(|value| value.checked_mul(1024))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Missing or invalid process residency field",
                )
            })?;
        Ok(value)
    }
    Ok(ProcessMemory {
        resident_bytes: field(status, "VmRSS:")?,
        peak_resident_bytes: field(status, "VmHWM:")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_residency_is_measured_in_bytes_including_kernel_peak() {
        let memory = process_memory().unwrap();
        assert!(memory.resident_bytes > 0);
        assert!(memory.peak_resident_bytes >= memory.resident_bytes);
    }

    #[test]
    fn linux_residency_ceiling_measurement_converts_kib_to_bytes() {
        let memory = parse_linux_status("Name:\ttest\nVmRSS:\t1024 kB\nVmHWM:\t2048 kB\n").unwrap();
        assert_eq!(memory.resident_bytes, 1024 * 1024);
        assert_eq!(memory.peak_resident_bytes, 2048 * 1024);
        assert!(parse_linux_status("VmRSS: 5 kB").is_err());
        assert!(parse_linux_status("VmRSS: 1024 MB\nVmHWM: 4 kB").is_err());
    }
}
