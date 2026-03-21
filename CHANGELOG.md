# Changelog

## 0.1.0

Initial public release.

Highlights:

- pure-Rust CRS transform engine with no C dependencies, no build scripts, and no unsafe code
- built-in EPSG registry for supported geographic and projected CRS definitions
- implemented projection support for Web Mercator, Transverse Mercator / UTM, Polar Stereographic, Lambert Conformal Conic, Albers Equal Area, Mercator, and Equidistant Cylindrical
- datum-shift support for the built-in Helmert-backed datums in the registry
- `proj-wkt` parser support for EPSG codes, common WKT, PROJ strings, and basic PROJJSON inputs for the implemented projection families
- lightweight compatibility facade for common downstream `Proj`-style construction and coordinate conversion flows

Release scope:

- production-ready for the supported projection families and CRS formats in this repository
- not a claim of full PROJ feature parity
