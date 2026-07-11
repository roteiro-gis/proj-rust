//! Fuzz the parse→emit→parse cycle for both WKT dialects: every definition
//! the parser accepts must serialize to WKT1 and WKT2 that parse back
//! successfully. Serializer errors are allowed (some definitions are not
//! emittable); panics and reparse failures are not.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(crs) = proj_wkt::parse_crs(text) else {
        return;
    };
    if let Ok(wkt) = proj_wkt::to_wkt(&crs) {
        proj_wkt::parse_crs(&wkt).unwrap_or_else(|error| {
            panic!("emitted WKT1 failed to reparse: {error}\nwkt: {wkt}");
        });
    }
    if let Ok(wkt2) = proj_wkt::to_wkt2(&crs) {
        proj_wkt::parse_crs(&wkt2).unwrap_or_else(|error| {
            panic!("emitted WKT2 failed to reparse: {error}\nwkt2: {wkt2}");
        });
    }
});
