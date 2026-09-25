use crate::{Error, Result};
use std::sync::Arc;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Pure Linux precedence resolver. Tests supply closures for environment and
/// filesystem reads, so no test mutates process-global environment state.
pub fn resolve_linux_base(
    physical: usize,
    threads: usize,
    env: impl Fn(&str) -> Option<String>,
    read: impl Fn(&str) -> Option<String>,
) -> usize {
    let parse = |value: Option<String>| value.and_then(|v| v.trim().parse::<usize>().ok());
    let slurm = |text: &str| {
        let (number, multiplier) = match text.as_bytes().last().map(u8::to_ascii_lowercase) {
            Some(b'k') => (&text[..text.len() - 1], 1_000f64),
            Some(b'm') => (&text[..text.len() - 1], 1_000_000f64),
            Some(b'g') => (&text[..text.len() - 1], 1_000_000_000f64),
            Some(b't') => (&text[..text.len() - 1], 1_000_000_000_000f64),
            _ => (text, 1_000_000f64),
        };
        let number = number.trim().parse::<f64>().ok()?;
        if !number.is_finite() {
            return None;
        }
        Some(if number < 0.0 {
            i64::MAX as usize
        } else {
            (number * multiplier) as usize
        })
    };
    if let Some(value) = env("SLURM_MEM_PER_NODE") {
        if let Some(bytes) = slurm(&value) {
            return bytes;
        }
    } else if let Some(value) = env("SLURM_MEM_PER_CPU")
        && let Some(bytes) = slurm(&value)
    {
        return bytes.saturating_mul(threads);
    }
    let cgroup = read("/proc/self/cgroup").unwrap_or_default();
    let v2 = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .unwrap_or("/");
    let v2_path = format!("/sys/fs/cgroup{v2}/memory.max");
    if let Some(limit) = parse(read(&v2_path)) {
        return physical.min(limit);
    }
    if let Some(limit) = parse(read("/sys/fs/cgroup/memory.max")) {
        return physical.min(limit);
    }
    if let Some(path) = cgroup.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        let _ = fields.next();
        let controllers = fields.next()?;
        let path = fields.next()?;
        controllers
            .split(',')
            .any(|controller| controller == "memory")
            .then_some(path)
    }) {
        let v1 = format!("/sys/fs/cgroup/memory{path}/memory.limit_in_bytes");
        if let Some(limit) = parse(read(&v1)) {
            return physical.min(limit);
        }
        if let Some(limit) = parse(read("/sys/fs/cgroup/memory/memory.limit_in_bytes")) {
            return physical.min(limit);
        }
    }
    physical
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Application-time memory denominator. Parsing settings never calls this.
pub trait MemoryLimitBase: Send + Sync + std::fmt::Debug {
    fn base_bytes(&self) -> Result<usize>;
    fn fallback(&self) -> bool {
        false
    }
    fn source(&self) -> &'static str;
}

#[derive(Debug)]
pub struct HostMemoryLimitBase;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl MemoryLimitBase for HostMemoryLimitBase {
    fn base_bytes(&self) -> Result<usize> {
        #[cfg(target_os = "linux")]
        {
            let text = std::fs::read_to_string("/proc/meminfo")?;
            let physical = text
                .lines()
                .find_map(|line| {
                    line.strip_prefix("MemTotal:")?
                        .split_whitespace()
                        .next()?
                        .parse::<usize>()
                        .ok()
                })
                .and_then(|kb| kb.checked_mul(1024))
                .ok_or_else(|| Error::Resource("cannot determine physical memory".into()))?;
            return Ok(resolve_linux_base(
                physical,
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1),
                |key| std::env::var(key).ok(),
                |path| std::fs::read_to_string(path).ok(),
            ));
        }
        #[cfg(target_os = "macos")]
        {
            static PHYSICAL_BYTES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
            if let Some(bytes) = PHYSICAL_BYTES.get() {
                return Ok(*bytes);
            }
            let output = std::process::Command::new("sysctl")
                .args(["-n", "hw.memsize"])
                .output()
                .map_err(|_| Error::Resource("cannot determine physical memory".into()))?;
            if !output.status.success() {
                return Err(Error::Resource("cannot determine physical memory".into()));
            }
            let bytes = std::str::from_utf8(&output.stdout)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .ok_or_else(|| Error::Resource("cannot determine physical memory".into()))?;
            let _ = PHYSICAL_BYTES.set(bytes);
            return Ok(bytes);
        }
        #[allow(unreachable_code)]
        Ok(usize::MAX)
    }
    fn source(&self) -> &'static str {
        "host-physical"
    }
}

#[derive(Debug)]
pub struct FixedMemoryLimitBase {
    pub bytes: usize,
    pub fallback: bool,
    pub label: &'static str,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl MemoryLimitBase for FixedMemoryLimitBase {
    fn base_bytes(&self) -> Result<usize> {
        Ok(self.bytes)
    }
    fn fallback(&self) -> bool {
        self.fallback
    }
    fn source(&self) -> &'static str {
        self.label
    }
}
pub type MemoryLimitBaseRef = Arc<dyn MemoryLimitBase>;
