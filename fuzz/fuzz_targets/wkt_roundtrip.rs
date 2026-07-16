//! Fuzz the parse→emit→parse cycle: every definition the parser accepts must
//! serialize to WKT that parses back successfully. Serializer errors are
//! allowed (some definitions are not emittable); panics and reparse failures
//! are not.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(crs) = proj_wkt::parse_crs(text) else {
        return;
    };
    let Ok(wkt) = proj_wkt::to_wkt(&crs) else {
        return;
    };
    proj_wkt::parse_crs(&wkt).unwrap_or_else(|error| {
        panic!("emitted WKT failed to reparse: {error}\nwkt: {wkt}");
    });
});
