//! Fuzz the parse→emit-PROJJSON→parse cycle: every definition the parser
//! accepts must serialize to PROJJSON that parses back successfully.
//! Serializer errors are allowed (some definitions are not emittable);
//! panics and reparse failures are not.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(crs) = proj_wkt::parse_crs(text) else {
        return;
    };
    let Ok(json) = proj_wkt::to_projjson(&crs) else {
        return;
    };
    proj_wkt::parse_crs(&json).unwrap_or_else(|error| {
        panic!("emitted PROJJSON failed to reparse: {error}\njson: {json}");
    });
});
