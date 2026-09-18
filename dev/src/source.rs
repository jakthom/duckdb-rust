use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

fn collect(path: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs")
            || path.file_name().is_some_and(|name| name == "Cargo.toml")
        {
            files.push(path);
        }
    }
    Ok(())
}

/// Content identity for Rust sources and Cargo configuration, including dirty and new files.
pub fn fingerprint(root: &Path) -> io::Result<String> {
    let mut files = vec![
        root.join("Cargo.toml"),
        root.join("Cargo.lock"),
        root.join(".cargo/config.toml"),
    ];
    for directory in ["src", "dev", "tools", "test", "benchmark"] {
        collect(&root.join(directory), &mut files)?;
    }
    files.sort();
    let mut hash = Sha256::new();
    for path in files {
        hash.update(
            path.strip_prefix(root)
                .map_err(io::Error::other)?
                .as_os_str()
                .as_encoded_bytes(),
        );
        hash.update([0]);
        let bytes = fs::read(path)?;
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok(format!("{:x}", hash.finalize()))
}
