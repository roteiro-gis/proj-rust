//! Fuzz the PROJ GeoTIFF/COG grid decoder with attacker-controlled bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use proj_core::operation::{GridId, GridInterpolation};
use proj_core::{GridDefinition, GridFormat, GridHandle};

fuzz_target!(|data: &[u8]| {
    let definition = GridDefinition {
        id: GridId(1),
        name: "fuzz.tif".into(),
        format: GridFormat::GeoTiff,
        interpolation: GridInterpolation::Bilinear,
        area_of_use: None,
        resource_names: smallvec::SmallVec::from_vec(vec!["fuzz.tif".to_string()]),
    };
    if let Ok(handle) = GridHandle::from_bytes(definition, data) {
        let _ = handle.sample(0.001, 0.001);
        let _ = handle.sample_vertical_offset_meters(0.001, 0.001);
    }
});
