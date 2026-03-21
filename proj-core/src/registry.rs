use crate::crs::CrsDef;
use crate::error::{Error, Result};
use crate::registry_data;

/// Look up a CRS definition by EPSG code.
///
/// Returns `None` if the EPSG code is not in the built-in registry.
pub fn lookup_epsg(code: u32) -> Option<CrsDef> {
    // Check geographic CRS
    for &(epsg, ref def) in registry_data::GEOGRAPHIC_CRS {
        if epsg == code {
            return Some(CrsDef::Geographic(*def));
        }
    }

    // Check manually curated projected CRS
    for &(epsg, ref def) in registry_data::PROJECTED_CRS {
        if epsg == code {
            return Some(CrsDef::Projected(*def));
        }
    }

    // Check UTM zones
    if let Some(def) = registry_data::lookup_utm(code) {
        return Some(CrsDef::Projected(def));
    }

    None
}

/// Parse an authority:code string (e.g., "EPSG:4326") and look up the CRS definition.
///
/// Currently only supports the "EPSG" authority.
pub fn lookup_authority_code(code: &str) -> Result<CrsDef> {
    let parts: Vec<&str> = code.split(':').collect();
    if parts.len() != 2 {
        return Err(Error::UnknownCrs(format!(
            "invalid authority:code format: {code}"
        )));
    }

    let authority = parts[0].trim();
    let code_str = parts[1].trim();

    if !authority.eq_ignore_ascii_case("EPSG") {
        return Err(Error::UnknownCrs(format!(
            "unsupported authority: {authority} (only EPSG is supported)"
        )));
    }

    let epsg: u32 = code_str
        .parse()
        .map_err(|_| Error::UnknownCrs(format!("invalid EPSG code: {code_str}")))?;

    lookup_epsg(epsg).ok_or_else(|| Error::UnknownCrs(format!("unknown EPSG code: {epsg}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_wgs84() {
        let crs = lookup_epsg(4326).expect("should find 4326");
        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 4326);
    }

    #[test]
    fn lookup_web_mercator() {
        let crs = lookup_epsg(3857).expect("should find 3857");
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 3857);
    }

    #[test]
    fn lookup_polar_stereo_north() {
        let crs = lookup_epsg(3413).expect("should find 3413");
        assert!(crs.is_projected());
    }

    #[test]
    fn lookup_utm_zone_18n() {
        let crs = lookup_epsg(32618).expect("should find UTM 18N");
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 32618);
    }

    #[test]
    fn lookup_utm_zone_1s() {
        let crs = lookup_epsg(32701).expect("should find UTM 1S");
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 32701);
    }

    #[test]
    fn lookup_utm_zone_60n() {
        let crs = lookup_epsg(32660).expect("should find UTM 60N");
        assert!(crs.is_projected());
    }

    #[test]
    fn lookup_unknown_epsg() {
        assert!(lookup_epsg(99999).is_none());
    }

    #[test]
    fn authority_code_parse() {
        let crs = lookup_authority_code("EPSG:4326").expect("should parse");
        assert_eq!(crs.epsg(), 4326);
    }

    #[test]
    fn authority_code_case_insensitive() {
        let crs = lookup_authority_code("epsg:3857").expect("should parse");
        assert_eq!(crs.epsg(), 3857);
    }

    #[test]
    fn authority_code_invalid_format() {
        assert!(lookup_authority_code("NONSENSE").is_err());
    }

    #[test]
    fn authority_code_unknown() {
        assert!(lookup_authority_code("EPSG:99999").is_err());
    }

    #[test]
    fn authority_code_non_epsg() {
        assert!(lookup_authority_code("OGC:CRS84").is_err());
    }

    #[test]
    fn nad27_lookup() {
        let crs = lookup_epsg(4267).expect("should find NAD27");
        assert!(crs.is_geographic());
        assert!(!crs.datum().is_wgs84_compatible());
    }
}
