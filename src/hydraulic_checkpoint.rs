//! Restartable, bounded-memory hydraulic simulation snapshots.
//!
//! Saves a complete committed `DiskTransport` state to a new directory,
//! never overwriting an existing checkpoint. All named field files are staged
//! beside the destination, checked row by row, then published with one
//! same-filesystem directory rename. Loading validates sizes, numeric values,
//! schema and checksums before constructing a new independent temporary state.
//!
//! Checksums are FNV-1a for accidental corruption, NOT cryptographic security.
//! Directory publish is not a cross-platform power-loss durability guarantee.
//! This is an experimental solver API, not a GUI/CLI project format.
use crate::hydraulic_stream::{DiskTransport, read_row, write_row};
use crate::region::MAX_REGION_RESOLUTION;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

const VERSION: u32 = 1;
const NAMES: [&str; 6] = [
    "height.f32", "water.f32", "sediment.f32",
    "flow.f32", "wear.f32", "deposits.f32",
];
const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointManifest {
    version: u32,
    resolution: usize,
    /// First three are mandatory. Flow is optional until the first tick;
    /// wear and deposits are both optional until the first eroding tick.
    hashes: [Option<String>; 6],
}

fn copy_checked(
    source: &mut File, target: &mut File, n: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<String, String> {
    let bytes = (n as u64) * (n as u64) * 4;
    if source.metadata().map_err(|e| e.to_string())?.len() != bytes {
        return Err("Hydraulic checkpoint field length does not match resolution".into());
    }
    let mut hash = FNV_OFFSET;
    for y in 0..n {
        if cancelled() { return Err("Build cancelled".into()); }
        let row = read_row(source, n, y)?;
        if row.iter().any(|v| !v.is_finite() || *v < 0.0) {
            return Err("Hydraulic checkpoint contains invalid/nonfinite field values".into());
        }
        for value in &row {
            for byte in value.to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        }
        write_row(target, &row)?;
    }
    target.sync_all().map_err(|e| e.to_string())?;
    Ok(format!("{hash:016x}"))
}

fn save_field(
    source: &mut File, staging: &Path, name: &str, n: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<String, String> {
    let mut output = File::create(staging.join(name)).map_err(|e| e.to_string())?;
    copy_checked(source, &mut output, n, cancelled)
}

fn load_field(
    directory: &Path, name: &str, expected_hash: &str, n: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<File, String> {
    let mut source = File::open(directory.join(name)).map_err(|e| e.to_string())?;
    let mut output = tempfile::tempfile().map_err(|e| e.to_string())?;
    let hash = copy_checked(&mut source, &mut output, n, cancelled)?;
    if hash != expected_hash {
        return Err(format!("Hydraulic checkpoint {name} checksum mismatch"));
    }
    Ok(output)
}

impl DiskTransport {
    /// Save a newly named, complete state snapshot. The destination must not
    /// exist and its parent directory must already exist on this filesystem.
    /// Old state is unchanged even when the write fails or is cancelled.
    ///
    /// Large world snapshots require substantial disk free space: six full
    /// 8192² fields occupy about 1.5 GiB before staging/copy overhead.
    pub fn save_checkpoint(
        &mut self, directory: &Path, cancelled: impl Fn() -> bool,
    ) -> Result<(), String> {
        if directory.exists() {
            return Err("Hydraulic checkpoint destination already exists".into());
        }
        let parent = directory.parent()
            .filter(|parent| parent.is_dir())
            .ok_or("Hydraulic checkpoint parent directory does not exist")?;
        if cancelled() { return Err("Build cancelled".into()); }
        let stage = tempfile::Builder::new().prefix(".gaia-hydro-")
            .tempdir_in(parent).map_err(|e| e.to_string())?;
        let n = self.size;
        let mut hashes: [Option<String>; 6] = std::array::from_fn(|_| None);
        hashes[0] = Some(save_field(&mut self.height, stage.path(), NAMES[0], n, &cancelled)?);
        hashes[1] = Some(save_field(&mut self.water, stage.path(), NAMES[1], n, &cancelled)?);
        hashes[2] = Some(save_field(&mut self.sediment, stage.path(), NAMES[2], n, &cancelled)?);
        if let Some(file) = self.last_flow.as_mut() {
            hashes[3] = Some(save_field(file, stage.path(), NAMES[3], n, &cancelled)?);
        }
        if let Some(file) = self.wear.as_mut() {
            hashes[4] = Some(save_field(file, stage.path(), NAMES[4], n, &cancelled)?);
        }
        if let Some(file) = self.deposits.as_mut() {
            hashes[5] = Some(save_field(file, stage.path(), NAMES[5], n, &cancelled)?);
        }
        if hashes[4].is_some() != hashes[5].is_some() {
            return Err("Hydraulic checkpoint wear/deposit states must be paired".into());
        }
        if cancelled() { return Err("Build cancelled".into()); }
        let manifest = CheckpointManifest {
            version: VERSION, resolution: n, hashes,
        };
        let body = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
        let mut output = File::create(stage.path().join("manifest.json"))
            .map_err(|e| e.to_string())?;
        output.write_all(&body).map_err(|e| e.to_string())?;
        output.sync_all().map_err(|e| e.to_string())?;
        drop(output);
        if cancelled() { return Err("Build cancelled".into()); }
        // Keep the staging directory only when it is ready to publish.
        let staged_path = stage.keep();
        match fs::rename(&staged_path, directory) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = fs::remove_dir_all(&staged_path);
                Err(format!("Hydraulic checkpoint publish failed: {err}"))
            }
        }
    }

    /// Restore a saved simulation snapshot into independent scratch files.
    /// The caller can resume stripe or 2D-tile ticks from exactly this state.
    /// A corrupt or incomplete snapshot is rejected before returning state.
    pub fn open_checkpoint(
        directory: &Path, cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() { return Err("Build cancelled".into()); }
        let path = directory.join("manifest.json");
        if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 8192 {
            return Err("Hydraulic checkpoint manifest is too large".into());
        }
        let body = fs::read(&path).map_err(|e| e.to_string())?;
        let manifest: CheckpointManifest = serde_json::from_slice(&body)
            .map_err(|e| format!("Invalid hydraulic checkpoint manifest: {e}"))?;
        let n = manifest.resolution;
        if manifest.version != VERSION || !(32..=MAX_REGION_RESOLUTION).contains(&n) {
            return Err("Unsupported hydraulic checkpoint version or resolution".into());
        }
        if manifest.hashes[..3].iter().any(Option::is_none) ||
            manifest.hashes[4].is_some() != manifest.hashes[5].is_some()
        {
            return Err("Hydraulic checkpoint is missing required paired fields".into());
        }
        let mut fields: [Option<File>; 6] = std::array::from_fn(|_| None);
        for (i, name) in NAMES.iter().enumerate() {
            if let Some(hash) = &manifest.hashes[i] {
                fields[i] = Some(load_field(directory, name, hash, n, &cancelled)?);
            } else if directory.join(name).exists() {
                return Err(format!("Unexpected hydraulic checkpoint field: {name}"));
            }
        }
        if cancelled() { return Err("Build cancelled".into()); }
        let mut iter = fields.into_iter();
        Ok(Self {
            size: n,
            height: iter.next().flatten().ok_or("Missing height checkpoint")?,
            water: iter.next().flatten().ok_or("Missing water checkpoint")?,
            sediment: iter.next().flatten().ok_or("Missing sediment checkpoint")?,
            last_flow: iter.next().flatten(),
            wear: iter.next().flatten(),
            deposits: iter.next().flatten(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hydraulic_exchange::WorldEdge;
    use crate::hydrology::HydraulicParameters;
    use std::cell::Cell;
    use std::io::{Read, Seek, SeekFrom};

    fn state(n: usize) -> DiskTransport {
        DiskTransport::from_rows(n, |y, h, w, s| {
            for x in 0..n {
                h[x] = (n - x) as f32 / n as f32;
                w[x] = if (x + y) % 5 == 0 { 0.4 } else { 0.01 };
                s[x] = if (x * 2 + y) % 7 == 0 { 0.08 } else { 0.01 };
            }
            Ok(())
        }).unwrap()
    }

    fn same(a: &mut DiskTransport, b: &mut DiskTransport) {
        assert_eq!(a.size(), b.size());
        for y in 0..a.size() {
            assert_eq!(a.solid_row(y).unwrap(), b.solid_row(y).unwrap());
            assert_eq!(a.transport_row(y).unwrap(), b.transport_row(y).unwrap());
        }
    }

    #[test]
    fn restores_all_six_fields_and_resumes_bitwise_equal_eroding_ticks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoint");
        let mut original = state(32);
        let params = HydraulicParameters { iterations: 1, ..Default::default() };
        for _ in 0..4 {
            original.step_eroding(3, WorldEdge::Open, params, || false).unwrap();
        }
        original.save_checkpoint(&path, || false).unwrap();
        let mut restored = DiskTransport::open_checkpoint(&path, || false).unwrap();
        same(&mut original, &mut restored);
        assert_eq!(original.mass().unwrap(), restored.mass().unwrap());
        assert_eq!(original.solid_mass().unwrap(), restored.solid_mass().unwrap());
        for tick in 0..15 {
            let edge = if tick % 2 == 0 { WorldEdge::Closed } else { WorldEdge::Open };
            let a = original.step_eroding(7, edge, params, || false).unwrap();
            let b = restored.step_eroding(1, edge, params, || false).unwrap();
            assert_eq!(a.water_outflow, b.water_outflow);
            assert_eq!(a.sediment_outflow, b.sediment_outflow);
        }
        same(&mut original, &mut restored);
    }

    #[test]
    fn optional_diagnostics_survive_before_and_after_transport_tick() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("initial");
        let mut original = state(32);
        original.save_checkpoint(&path, || false).unwrap();
        let mut restored = DiskTransport::open_checkpoint(&path, || false).unwrap();
        same(&mut original, &mut restored);
        let path2 = dir.path().join("transport");
        original.step_weather(3, WorldEdge::Closed, 0.004, 0.02, || false).unwrap();
        original.save_checkpoint(&path2, || false).unwrap();
        let mut restored = DiskTransport::open_checkpoint(&path2, || false).unwrap();
        same(&mut original, &mut restored);
        assert!(restored.transport_row(0).unwrap().2.is_some());
        assert!(restored.solid_row(0).unwrap().1.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn rejects_corrupted_and_truncated_checkpoint_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved");
        let mut original = state(32);
        original.save_checkpoint(&path, || false).unwrap();
        let target = path.join("water.f32");
        let mut file = fs::OpenOptions::new().read(true).write(true).open(&target).unwrap();
        let mut first = [0u8; 1];
        file.read_exact(&mut first).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&[first[0] ^ 1]).unwrap();
        drop(file);
        assert!(DiskTransport::open_checkpoint(&path, || false)
            .unwrap_err().contains("checksum"));
        let file = fs::OpenOptions::new().write(true).open(&target).unwrap();
        file.set_len(12).unwrap();
        assert!(DiskTransport::open_checkpoint(&path, || false)
            .unwrap_err().contains("length"));
    }

    #[test]
    fn cancellation_never_publishes_a_partial_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("interrupted");
        let mut original = state(32);
        let prior = original.mass().unwrap();
        let calls = Cell::new(0usize);
        assert_eq!(
            original.save_checkpoint(&path, || {
                calls.set(calls.get() + 1);
                calls.get() >= 10
            }).unwrap_err(),
            "Build cancelled"
        );
        assert!(!path.exists());
        assert_eq!(prior, original.mass().unwrap());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn existing_destination_is_never_overwritten_and_bad_manifest_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("first");
        let mut original = state(32);
        original.save_checkpoint(&path, || false).unwrap();
        assert!(original.save_checkpoint(&path, || false)
            .unwrap_err().contains("already exists"));
        let manifest_path = path.join("manifest.json");
        let mut json: serde_json::Value = serde_json::from_slice(
            &fs::read(&manifest_path).unwrap(),
        ).unwrap();
        json["version"] = serde_json::json!(99);
        fs::write(&manifest_path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(DiskTransport::open_checkpoint(&path, || false)
            .unwrap_err().contains("Unsupported"));
    }
}
