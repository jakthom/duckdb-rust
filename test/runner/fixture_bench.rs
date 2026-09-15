//! Deterministic accounting for the fixture/directive timing workload.
//!
//! The campaign runner owns timing and reference invocation. This helper only
//! provides a stable operation count and checksum, so a fast run cannot be
//! accepted when it staged a different fixture set or took different actions.

use crc32fast::Hasher;
use std::time::{Duration, Instant};

use super::directives::DirectiveAction;
use super::fixtures::FixtureFingerprint;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureBenchResult {
    pub elapsed: Duration,
    pub operations: u64,
    pub checksum: u32,
}

pub(crate) struct FixtureBench {
    started: Instant,
    operations: u64,
    checksum: Hasher,
}

impl FixtureBench {
    pub(crate) fn start() -> Self {
        Self {
            started: Instant::now(),
            operations: 0,
            checksum: Hasher::new(),
        }
    }
    pub(crate) fn directive(&mut self, action: &DirectiveAction) {
        self.operations += 1;
        // Debug has no randomized fields for these enums and keeps all action
        // payloads (not merely their variant) in the comparison checksum.
        self.checksum.update(format!("{action:?}\n").as_bytes());
    }
    pub(crate) fn fixture(&mut self, fingerprint: FixtureFingerprint) {
        self.operations += 1;
        self.checksum.update(&fingerprint.bytes.to_le_bytes());
        self.checksum.update(&fingerprint.crc32.to_le_bytes());
    }
    pub(crate) fn finish(self) -> FixtureBenchResult {
        FixtureBenchResult {
            elapsed: self.started.elapsed(),
            operations: self.operations,
            checksum: self.checksum.finalize(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksum_distinguishes_wrong_disposition() {
        let mut pass = FixtureBench::start();
        pass.directive(&DirectiveAction::SkipFile {
            reason: "require json".into(),
        });
        pass.fixture(FixtureFingerprint { bytes: 3, crc32: 4 });
        let mut wrong = FixtureBench::start();
        wrong.directive(&DirectiveAction::None);
        wrong.fixture(FixtureFingerprint { bytes: 3, crc32: 4 });
        let pass = pass.finish();
        let wrong = wrong.finish();
        assert_eq!(pass.operations, wrong.operations);
        assert_ne!(pass.checksum, wrong.checksum);
    }
}
