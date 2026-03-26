# Changelog

## Unreleased

- add native 3D coordinate APIs in `proj-core` and `proj-wkt`, with `z` preserved by the current horizontal-only transform pipeline
- add live bundled C PROJ parity coverage and benchmark coverage for the new 3D transform path
- optimize exact same-definition custom CRS transforms to use the identity pipeline
- add `Transform::inverse()` plus source/target CRS introspection for reusable forward/reverse transform pairs
- add `Bounds` and sampled `transform_bounds()` APIs in `proj-core`, with matching `Proj` facade support in `proj-wkt`
- fix WKT EPSG detection to respect top-level CRS identifiers instead of nested base CRS metadata, and add WKT2 `ID["EPSG", ...]` support
- add legacy `+init=epsg:XXXX` parsing support for downstream `Proj`-style compatibility flows

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
