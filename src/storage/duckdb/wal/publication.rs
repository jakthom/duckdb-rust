use super::*;
use crate::storage::recovery::RecoveryPublication;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn prepare(
    input: RecoveryInput,
    format: &dyn SnapshotFormat,
    context: &QueryContext,
) -> Result<PreparedRecovery> {
    let Inspection {
        identity,
        header,
        scan,
    } = inspect(&input, format, context)?;
    let snapshot = format.decode(input.checkpoint.clone(), context.type_registry())?;
    let mut snapshot = replay(snapshot, &input.log, &scan, identity, context)?;
    let layout;
    let publication = if scan.checkpoint == Some(identity.root) || scan.frames.is_empty() {
        layout = crate::storage::layout::CheckpointLayout::identity(&snapshot)?;
        RecoveryPublication::RetireLog
    } else {
        let image = format.encode_successor(&snapshot, &input.checkpoint)?;
        let checkpoint = image.bytes;
        layout = image.layout;
        context.check()?;
        let successor = super::super::CheckpointIdentity::read(&checkpoint)?;
        if successor.identifier != identity.identifier
            || identity.iteration.checked_add(1) != Some(successor.iteration)
            || successor.root == identity.root
            || successor.root == u64::MAX
        {
            return Err(Error::Internal(
                "successor checkpoint violates native recovery identity".into(),
            ));
        }
        // Publication compacts rows. Return the published physical identities
        // so a subsequent transaction logger can address this checkpoint.
        let decoded = format.decode(checkpoint.clone(), context.type_registry())?;
        snapshot.validate_checkpoint_layout(&decoded, &layout, context)?;
        snapshot = decoded;
        // Reconstruct only committed frames, excluding a previous checkpoint
        // marker and incomplete tail. A flush after that old marker still
        // commits any preceding data records, which must be preserved.
        let mut bridge_log = input.log[..header.map_or(0, |h| h.end)].to_vec();
        for frame in &scan.frames {
            context.check()?;
            bridge_log.extend_from_slice(&input.log[frame.start - 16..frame.end]);
        }
        let mut marker = super::super::binary::Encoder::default();
        marker.property(100, 99);
        marker.field(101);
        marker.property(100, successor.root);
        marker.property(101, 8);
        marker.end();
        marker.end();
        append_frame(&marker.0, &mut bridge_log)?;
        append_frame(&[100, 0, 100, 255, 255], &mut bridge_log)?;
        if checkpoint.len() > MAX_BYTES || bridge_log.len() > MAX_BYTES {
            return Err(Error::Resource(
                "recovery publication limits each file to 512 MiB".into(),
            ));
        }
        RecoveryPublication::Replace {
            checkpoint,
            bridge_log,
        }
    };
    Ok(PreparedRecovery {
        snapshot,
        layout,
        basis: input,
        publication,
    })
}
