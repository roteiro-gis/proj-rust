//! Binary format definition for proj-rust's embedded EPSG registry
//! (`proj-core/data/epsg.bin`).
//!
//! This crate is the single source of truth for the container layout shared
//! by the `proj-core` reader (`epsg_db`) and the `gen-registry` writer in
//! `gen-reference`. Any change here is a format change: bump [`VERSION`],
//! regenerate the registry, and let the CI reproducibility check
//! (`scripts/check-registry-generation.sh`) prove writer and reader agree.
//!
//! The container is little-endian throughout: a fixed header (magic, version,
//! ten section counts) followed by packed variable-length records per
//! section. Strings are `u16` length-prefixed UTF-8.

#![forbid(unsafe_code)]

/// `"EPSG"` in big-endian byte order.
pub const MAGIC: u32 = 0x4550_5347;
/// Container format version; bump on any layout or semantic change.
pub const VERSION: u16 = 9;
/// Fixed header: magic (4) + version (2) + reserved (2) + eleven `u32`
/// section counts.
pub const HEADER_SIZE: usize = 52;

// Fixed-size record prefixes. "Base" sizes exclude the trailing
// length-prefixed strings.
pub const ELLIPSOID_RECORD_SIZE: usize = 20;
pub const DATUM_RECORD_SIZE: usize = 12;
pub const GEO_CRS_RECORD_BASE_SIZE: usize = 8;
pub const PROJ_CRS_RECORD_BASE_SIZE: usize = 80;
pub const VERTICAL_CRS_RECORD_BASE_SIZE: usize = 16;
pub const COMPOUND_CRS_RECORD_BASE_SIZE: usize = 28;

// Datum→WGS84 relationship tags (definition metadata only; operation
// selection never synthesizes transforms from these). Since version 9,
// records carry only the WGS84-compatibility tag; per-datum Helmert values
// are a generation-time input for deriving it and are not stored.
pub const DATUM_SHIFT_UNKNOWN: u8 = 0;
pub const DATUM_SHIFT_IDENTITY: u8 = 1;

// Projection method tags.
pub const METHOD_WEB_MERCATOR: u8 = 1;
pub const METHOD_TRANSVERSE_MERCATOR: u8 = 2;
pub const METHOD_MERCATOR: u8 = 3;
pub const METHOD_LCC: u8 = 4;
pub const METHOD_ALBERS: u8 = 5;
pub const METHOD_POLAR_STEREO: u8 = 6;
pub const METHOD_EQUIDISTANT_CYL: u8 = 7;
pub const METHOD_LAEA: u8 = 8;
pub const METHOD_OBLIQUE_STEREO: u8 = 9;
pub const METHOD_HOTINE_OBLIQUE_MERCATOR_A: u8 = 10;
pub const METHOD_HOTINE_OBLIQUE_MERCATOR_B: u8 = 11;
pub const METHOD_CASSINI_SOLDNER: u8 = 12;
pub const METHOD_LAEA_SPHERICAL: u8 = 13;
pub const METHOD_COLOMBIA_URBAN: u8 = 14;
pub const METHOD_LCC_MICHIGAN: u8 = 15;
pub const METHOD_LCC_1SP_VARIANT_B: u8 = 16;
pub const METHOD_KROVAK_NORTH_ORIENTATED: u8 = 17;
pub const METHOD_KROVAK_MODIFIED_NORTH_ORIENTATED: u8 = 18;
pub const METHOD_EQUAL_EARTH: u8 = 19;

// Coordinate operation step tags.
pub const OP_IDENTITY: u8 = 0;
pub const OP_HELMERT: u8 = 1;
pub const OP_GRID_SHIFT: u8 = 2;
pub const OP_CONCATENATED: u8 = 3;

// Operation flag bits.
pub const FLAG_DEPRECATED: u8 = 1 << 0;
pub const FLAG_PREFERRED: u8 = 1 << 1;
pub const FLAG_APPROXIMATE: u8 = 1 << 2;
/// EPSG records a same-CRS-pair replacement for this operation
/// (`supersession` table); ranking prefers the replacement.
pub const FLAG_SUPERSEDED: u8 = 1 << 3;

// Grid resource tags.
pub const GRID_FORMAT_NTV2: u8 = 1;
pub const GRID_FORMAT_GTX: u8 = 2;
pub const GRID_FORMAT_GEOTIFF: u8 = 3;
pub const GRID_INTERPOLATION_BILINEAR: u8 = 1;
pub const VERTICAL_OFFSET_GEOID_HEIGHT_METERS: u8 = 1;

// Compound CRS component tags.
pub const HORIZONTAL_CRS_GEOGRAPHIC: u8 = 1;
pub const HORIZONTAL_CRS_PROJECTED: u8 = 2;
pub const VERTICAL_COMPONENT_ELLIPSOIDAL: u8 = 1;
pub const VERTICAL_COMPONENT_REGISTRY_CRS: u8 = 2;

/// Little-endian read primitives for the trusted, CI-reproducibility-gated
/// embedded blob. Offsets are validated by construction (record walking), so
/// these index directly and panic on a malformed blob rather than returning
/// errors — a corrupt embedded registry is a build defect, not user input.
pub mod read {
    pub fn u16(data: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([data[offset], data[offset + 1]])
    }

    pub fn u32(data: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ])
    }

    pub fn f64(data: &[u8], offset: usize) -> f64 {
        f64::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ])
    }
}

/// Little-endian write primitives. Float canonicalization (NaN bit patterns,
/// decimal normalization) is generator policy and lives with the generator.
pub mod write {
    pub fn u16(buf: &mut Vec<u8>, value: u16) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u32(buf: &mut Vec<u8>, value: u32) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    /// `u16` length-prefixed UTF-8 string.
    pub fn string_u16(buf: &mut Vec<u8>, value: &str) {
        let bytes = value.as_bytes();
        let len = u16::try_from(bytes.len()).expect("string too long for embedded registry");
        u16(buf, len);
        buf.extend_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn read_write_roundtrip() {
        let mut buf = Vec::new();
        super::write::u16(&mut buf, 0xBEEF);
        super::write::u32(&mut buf, 0xDEAD_BEEF);
        super::write::string_u16(&mut buf, "EPSG");

        assert_eq!(super::read::u16(&buf, 0), 0xBEEF);
        assert_eq!(super::read::u32(&buf, 2), 0xDEAD_BEEF);
        assert_eq!(super::read::u16(&buf, 6), 4);
        assert_eq!(&buf[8..12], b"EPSG");
    }
}
