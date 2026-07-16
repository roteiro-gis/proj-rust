//! Fuzz the full CRS parsing surface: WKT1/WKT2, PROJJSON, PROJ strings, and
//! authority codes all dispatch through `parse_crs`. The contract under fuzz
//! is "never panic, never hang": any outcome must be `Ok` or a typed error.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = proj_wkt::parse_crs(text);
    }
});
