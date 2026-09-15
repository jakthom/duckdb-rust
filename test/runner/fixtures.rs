//! Safe source-rooted fixture operations for SQLLogicTest directives.

use crc32fast::Hasher;
use flate2::read::GzDecoder;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use super::directives::SourceLocation;

pub(crate) const DEFAULT_INCLUDE_DEPTH: usize = 64;
pub(crate) const DEFAULT_MAX_GZIP_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FixtureError {
    #[error("{location}: {message}")]
    Located {
        location: SourceLocation,
        message: String,
    },
    #[error("fixture path {path:?}: {message}")]
    Path { path: PathBuf, message: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FixtureFingerprint {
    pub bytes: u64,
    pub crc32: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct FixtureResolver {
    source_root: PathBuf,
    scratch_root: PathBuf,
    include_stack: Vec<PathBuf>,
    max_include_depth: usize,
    max_gzip_bytes: u64,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FixtureResolver {
    pub(crate) fn new(
        source_root: impl AsRef<Path>,
        scratch_root: impl AsRef<Path>,
    ) -> Result<Self, FixtureError> {
        let source_root = fs::canonicalize(source_root)?;
        let scratch_root = scratch_root.as_ref().to_path_buf();
        fs::create_dir_all(&scratch_root)?;
        let scratch_root = fs::canonicalize(scratch_root)?;
        Ok(Self {
            source_root,
            scratch_root,
            include_stack: Vec::new(),
            max_include_depth: DEFAULT_INCLUDE_DEPTH,
            max_gzip_bytes: DEFAULT_MAX_GZIP_BYTES,
        })
    }

    #[cfg(test)]
    fn limits(mut self, include_depth: usize, gzip_bytes: u64) -> Self {
        self.max_include_depth = include_depth;
        self.max_gzip_bytes = gzip_bytes;
        self
    }

    fn reject_parent(path: &Path) -> Result<(), FixtureError> {
        if path.is_absolute()
            || path.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(FixtureError::Path {
                path: path.to_path_buf(),
                message: "must be a relative path without traversal".into(),
            });
        }
        Ok(())
    }

    fn rooted_existing(&self, root: &Path, relative: &Path) -> Result<PathBuf, FixtureError> {
        Self::reject_parent(relative)?;
        let path = fs::canonicalize(root.join(relative)).map_err(|error| FixtureError::Path {
            path: relative.to_path_buf(),
            message: error.to_string(),
        })?;
        if !path.starts_with(root) {
            return Err(FixtureError::Path {
                path,
                message: "symlink escapes fixture root".into(),
            });
        }
        Ok(path)
    }

    fn rooted_output(&self, relative: &Path) -> Result<PathBuf, FixtureError> {
        if relative.components().any(|part| {
            matches!(part, Component::ParentDir)
                || (!relative.is_absolute()
                    && matches!(part, Component::RootDir | Component::Prefix(_)))
        }) {
            return Err(FixtureError::Path {
                path: relative.to_path_buf(),
                message: "must not contain traversal".into(),
            });
        }
        let output = if relative.is_absolute() {
            relative.to_path_buf()
        } else {
            self.scratch_root.join(relative)
        };
        // Check the lexical path before creating parents, then canonicalize the
        // parent to catch symlink escapes. `{TEST_DIR}/x` is absolute after the
        // runner performs source-compatible keyword replacement.
        if output == self.scratch_root || !output.starts_with(&self.scratch_root) {
            return Err(FixtureError::Path {
                path: output,
                message: "output must be inside the scratch root".into(),
            });
        }
        let parent = output.parent().ok_or_else(|| FixtureError::Path {
            path: output.clone(),
            message: "has no parent".into(),
        })?;
        fs::create_dir_all(parent)?;
        let parent = fs::canonicalize(parent)?;
        if !parent.starts_with(&self.scratch_root) {
            return Err(FixtureError::Path {
                path: output,
                message: "parent symlink escapes scratch root".into(),
            });
        }
        Ok(
            parent.join(relative.file_name().ok_or_else(|| FixtureError::Path {
                path: relative.to_path_buf(),
                message: "has no file name".into(),
            })?),
        )
    }

    /// Starts the include stack for one top-level script. The canonical source
    /// is retained so an `a -> b -> a` cycle is caught deterministically.
    pub(crate) fn begin_script(
        &mut self,
        script: impl AsRef<Path>,
    ) -> Result<PathBuf, FixtureError> {
        let script = fs::canonicalize(script)?;
        if !script.starts_with(&self.source_root) {
            return Err(FixtureError::Path {
                path: script,
                message: "script is outside source root".into(),
            });
        }
        if !self.include_stack.is_empty() {
            return Err(FixtureError::Path {
                path: script,
                message: "previous script include stack is still active".into(),
            });
        }
        self.include_stack.push(script.clone());
        Ok(script)
    }
    pub(crate) fn finish_script(&mut self) {
        self.include_stack.clear();
    }

    /// Resolves an include from the declared source root (the pinned runner's
    /// working-directory convention), then records it in the active stack.
    /// Call `leave_include` after the nested parser exhausts.
    pub(crate) fn enter_include(
        &mut self,
        including: &Path,
        requested: &str,
        location: SourceLocation,
    ) -> Result<PathBuf, FixtureError> {
        if self.include_stack.len() >= self.max_include_depth {
            return Err(FixtureError::Located {
                location,
                message: format!("include depth exceeds {}", self.max_include_depth),
            });
        }
        let including = fs::canonicalize(including).map_err(|e| FixtureError::Located {
            location: location.clone(),
            message: e.to_string(),
        })?;
        if !including.starts_with(&self.source_root) {
            return Err(FixtureError::Located {
                location,
                message: "including source is outside source root".into(),
            });
        }
        let requested = Path::new(requested);
        Self::reject_parent(requested).map_err(|e| FixtureError::Located {
            location: location.clone(),
            message: e.to_string(),
        })?;
        let candidate = fs::canonicalize(self.source_root.join(requested)).map_err(|e| {
            FixtureError::Located {
                location: location.clone(),
                message: format!("failed to open include {requested:?}: {e}"),
            }
        })?;
        if !candidate.starts_with(&self.source_root) {
            return Err(FixtureError::Located {
                location,
                message: "include escapes source root".into(),
            });
        }
        if self.include_stack.last() != Some(&including) {
            return Err(FixtureError::Located {
                location,
                message: "include stack does not match including source".into(),
            });
        }
        if self.include_stack.iter().any(|path| path == &candidate) {
            return Err(FixtureError::Located {
                location,
                message: format!("include cycle at {}", candidate.display()),
            });
        }
        self.include_stack.push(candidate.clone());
        Ok(candidate)
    }
    pub(crate) fn leave_include(&mut self, path: &Path) {
        debug_assert_eq!(self.include_stack.last().map(PathBuf::as_path), Some(path));
        self.include_stack.pop();
    }

    pub(crate) fn expected_file(
        &self,
        relative: impl AsRef<Path>,
    ) -> Result<(PathBuf, FixtureFingerprint), FixtureError> {
        let path = self.rooted_existing(&self.source_root, relative.as_ref())?;
        Ok((path.clone(), fingerprint(File::open(path)?)?))
    }

    /// Copies a declared read-only source fixture into the per-test scratch
    /// tree. Symlinks are canonicalized before copying and never created.
    pub(crate) fn stage(
        &self,
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
    ) -> Result<FixtureFingerprint, FixtureError> {
        let source = self.rooted_existing(&self.source_root, source.as_ref())?;
        let destination = self.rooted_output(destination.as_ref())?;
        copy_bounded(File::open(source)?, create_new(&destination)?, u64::MAX)
    }

    /// G01's hardened equivalent of C++ `unzip`: `.gz` source must be rooted;
    /// output is always scratch-confined and extraction has a declared bound.
    pub(crate) fn unzip(
        &self,
        source: impl AsRef<Path>,
        destination: Option<&Path>,
    ) -> Result<(PathBuf, FixtureFingerprint), FixtureError> {
        let source_relative = source.as_ref();
        if source_relative.extension().and_then(|x| x.to_str()) != Some("gz") {
            return Err(FixtureError::Path {
                path: source_relative.to_path_buf(),
                message: "unzip input has not a GZIP extension".into(),
            });
        }
        let source = self.rooted_existing(&self.source_root, source_relative)?;
        let default =
            PathBuf::from(
                source_relative
                    .file_stem()
                    .ok_or_else(|| FixtureError::Path {
                        path: source_relative.to_path_buf(),
                        message: "missing gzip file stem".into(),
                    })?,
            );
        let output = self.rooted_output(destination.unwrap_or(&default))?;
        let result = copy_bounded(
            GzDecoder::new(File::open(source)?),
            create_or_truncate(&output)?,
            self.max_gzip_bytes,
        );
        match result {
            Ok(result) => Ok((output, result)),
            Err(error) => {
                // Do not leave a partial extraction that a later directive can
                // mistake for a complete fixture.
                let _ = fs::remove_file(&output);
                Err(error)
            }
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn create_new(path: &Path) -> Result<File, FixtureError> {
    Ok(OpenOptions::new().write(true).create_new(true).open(path)?)
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn create_or_truncate(path: &Path) -> Result<File, FixtureError> {
    Ok(OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?)
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fingerprint(mut input: impl Read) -> Result<FixtureFingerprint, FixtureError> {
    let mut hasher = Hasher::new();
    let mut bytes = 0;
    let mut chunk = [0; 8192];
    loop {
        let count = input.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        hasher.update(&chunk[..count]);
    }
    Ok(FixtureFingerprint {
        bytes,
        crc32: hasher.finalize(),
    })
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn copy_bounded(
    mut input: impl Read,
    mut output: impl Write,
    limit: u64,
) -> Result<FixtureFingerprint, FixtureError> {
    let mut hasher = Hasher::new();
    let mut bytes = 0u64;
    let mut chunk = [0; 8192];
    loop {
        let count = input.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or_else(|| FixtureError::Path {
                path: PathBuf::new(),
                message: "fixture size overflow".into(),
            })?;
        if bytes > limit {
            return Err(FixtureError::Path {
                path: PathBuf::new(),
                message: format!("fixture exceeds {limit} byte extraction limit"),
            });
        }
        output.write_all(&chunk[..count])?;
        hasher.update(&chunk[..count]);
    }
    Ok(FixtureFingerprint {
        bytes,
        crc32: hasher.finalize(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use tempfile::TempDir;
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn roots() -> (TempDir, FixtureResolver) {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let scratch = temp.path().join("scratch");
        fs::create_dir(&source).unwrap();
        (temp, FixtureResolver::new(&source, &scratch).unwrap())
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn include_is_source_rooted_cycle_checked_and_located() {
        let (temp, mut r) = roots();
        let root = temp.path().join("source");
        fs::write(root.join("a.test"), "").unwrap();
        fs::create_dir(root.join("dir")).unwrap();
        fs::write(root.join("dir/b.test"), "").unwrap();
        let a = r.begin_script(root.join("a.test")).unwrap();
        let b = r
            .enter_include(
                &a,
                "dir/b.test",
                SourceLocation {
                    source: "a.test".into(),
                    line: 1,
                },
            )
            .unwrap();
        assert!(matches!(
            r.enter_include(
                &b,
                "a.test",
                SourceLocation {
                    source: "b.test".into(),
                    line: 2
                }
            ),
            Err(FixtureError::Located { .. })
        ));
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn traversal_and_missing_expected_are_rejected() {
        let (_temp, r) = roots();
        assert!(r.expected_file("../secret").is_err());
        assert!(r.expected_file("missing.csv").is_err());
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[cfg(unix)]
    #[test]
    fn symlink_fixture_escape_and_include_depth_are_rejected() {
        let (temp, mut r) = roots();
        let root = temp.path().join("source");
        let outside = temp.path().join("outside");
        fs::write(&outside, "no").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert!(r.expected_file("link").is_err());
        fs::write(root.join("a"), "").unwrap();
        fs::write(root.join("b"), "").unwrap();
        let a = r.begin_script(root.join("a")).unwrap();
        let mut shallow = r.clone().limits(1, DEFAULT_MAX_GZIP_BYTES);
        assert!(matches!(
            shallow.enter_include(
                &a,
                "b",
                SourceLocation {
                    source: "a".into(),
                    line: 1
                }
            ),
            Err(FixtureError::Located { .. })
        ));
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn gzip_supports_absolute_scratch_destinations_null_default_and_repeat_extraction() {
        let (temp, r) = roots();
        let root = temp.path().join("source");
        let scratch = r.scratch_root.clone();
        let file = File::create(root.join("x.db.gz")).unwrap();
        let mut gzip = GzEncoder::new(file, Compression::default());
        gzip.write_all(b"fixture").unwrap();
        gzip.finish().unwrap();

        let explicit = scratch.join("explicit.db");
        let (out, hash) = r.unzip("x.db.gz", Some(&explicit)).unwrap();
        assert_eq!(out, explicit);
        assert_eq!(fs::read(&out).unwrap(), b"fixture");
        assert_eq!(hash.bytes, 7);
        fs::write(&out, b"stale bytes that must be truncated").unwrap();
        r.unzip("x.db.gz", Some(&explicit)).unwrap();
        assert_eq!(fs::read(&out).unwrap(), b"fixture");

        let (default, _) = r.unzip("x.db.gz", None).unwrap();
        assert_eq!(default, scratch.join("x.db"));
        r.unzip("x.db.gz", None).unwrap();
        assert_eq!(fs::read(default).unwrap(), b"fixture");
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn gzip_extraction_bound_is_reached_and_partial_output_is_removed() {
        let (temp, r) = roots();
        let root = temp.path().join("source");
        let file = File::create(root.join("bounded.gz")).unwrap();
        let mut gzip = GzEncoder::new(file, Compression::default());
        gzip.write_all(b"four").unwrap();
        gzip.finish().unwrap();

        let bounded = r.clone().limits(4, 3);
        let error = bounded.unzip("bounded.gz", None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("exceeds 3 byte extraction limit")
        );
        assert!(!temp.path().join("scratch/bounded").exists());
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn absolute_output_outside_scratch_is_rejected_without_creation() {
        let (temp, r) = roots();
        let outside = temp.path().join("outside/result.db");
        assert!(r.rooted_output(&outside).is_err());
        assert!(!outside.parent().unwrap().exists());
    }
}
