//! Fuzz the NTv2 (.gsb) grid parser with attacker-controlled bytes: grid
//! files are user-supplied resources, so parsing must never panic,
//! over-allocate past its documented caps, or hang.
#![no_main]

use libfuzzer_sys::fuzz_target;
use proj_core::operation::{GridId, GridInterpolation};
use proj_core::{GridDefinition, GridFormat, GridHandle};

fuzz_target!(|data: &[u8]| {
    let definition = GridDefinition {
        id: GridId(1),
        name: "fuzz.gsb".into(),
        format: GridFormat::Ntv2,
        interpolation: GridInterpolation::Bilinear,
        area_of_use: None,
        resource_names: smallvec::SmallVec::from_vec(vec!["fuzz.gsb".to_string()]),
    };
    if let Ok(handle) = GridHandle::from_bytes(definition, data) {
        // Exercise sampling on successfully parsed grids too.
        let _ = handle.sample(0.001, 0.001);
    }
});
