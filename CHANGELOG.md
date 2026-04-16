# Changelog

## 0.3.0

- add embedded coordinate-operation metadata and selection APIs, including explicit operation lookup, `Transform::from_operation`, `Transform::with_selection_options`, selected-operation introspection, and detailed selection diagnostics
- add NTv2 grid runtime support with embedded and application-provided grid providers, recursive concatenated operation handling, and bundled registry-backed grid resources
- expand the embedded EPSG registry with explicit datum-shift states, coordinate operations, areas of use, grid definitions, and CRS names
- make transform construction operation-aware with typed area-of-interest selection, inverse-aware metadata, and indexed operation lookup instead of registry-wide scans
- switch projections onto precomputed enum-backed hot paths and compiled transform pipelines, improving construction and execution costs
- tighten CRS parsing semantics across WKT, PROJJSON, and authority wrappers, rejecting contradictory or unsupported inputs instead of silently normalizing them
- refresh live bundled C PROJ parity coverage, benchmark coverage, and the published benchmark report for the new operation-selection and 3D paths
- continue to scope the release as a supported `0.x` line rather than a claim of full PROJ feature parity

## 0.2.0

- redesign the public CRS API around constructors/getters and a typed `LinearUnit`, replacing raw projected-unit scalars and making the native-unit model explicit
- add projected native-unit support across `proj-core`, `proj-wkt`, and the embedded EPSG registry so foot-based and other non-meter projected CRS definitions transform correctly
- add native 3D coordinate APIs in `proj-core` and `proj-wkt`, with `z` preserved by the current horizontal-only transform pipeline
- add live bundled C PROJ parity coverage and benchmark coverage for the new 3D transform path
- optimize exact same-definition custom CRS transforms to use the identity pipeline
- add `Transform::inverse()` plus source/target CRS introspection for reusable forward/reverse transform pairs
- add `Bounds` and sampled `transform_bounds()` APIs in `proj-core`, with matching `Proj` facade support in `proj-wkt`
- fix WKT EPSG detection to respect top-level CRS identifiers instead of nested base CRS metadata, and add WKT2 `ID["EPSG", ...]` support
- add legacy `+init=epsg:XXXX` parsing support for downstream `Proj`-style compatibility flows
- reject custom WKT, PROJJSON, and PROJ definitions that require unsupported geographic axis-order, prime-meridian, or angular-unit semantics instead of silently degrading them
- make Rayon-backed batch transforms adaptive by chunking large inputs and falling back to the sequential path when parallel overhead would dominate
- add staged release-packaging verification for the `proj-core` then `proj-wkt` publish order, and run it in CI

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
