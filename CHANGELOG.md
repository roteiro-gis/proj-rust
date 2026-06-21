# Changelog

## Unreleased

- breaking: bump the workspace crates to `0.8.0`; `SelectionOptions` now includes the public `area_bounds_densify_points` field for configurable AOI bounds sampling, so downstream struct-literal construction must set it or use `SelectionOptions::new()` / `Default`

## 0.7.0 - 2026-06-12

- add optional `geotiff` feature decoding PROJ-format GeoTIFF/COG grids (horizontal NTv2-equivalent offset grids with nested subgrids, and vertical geoid grids) into the existing NTv2/GTX sampling paths via the pure-Rust `geotiff-reader` crate
- add RDNAPTRANS2018 support: register EPSG:5709 (NAP height) and EPSG:7415 (Amersfoort / RD New + NAP height), and select the grid-backed ETRS89/WGS 84 3D ↔ RD New + NAP operation (nested `nl_nsgi_rdtrans2018.tif` horizontal shift + `nl_nsgi_nlgeo2018.tif` geoid), matching PROJ 9.8 to sub-millimetre when grids are supplied through a `GridProvider`

## 0.6.0 - 2026-05-30

- add registry-backed GTX vertical grid operation metadata and automatic ellipsoidal-to-gravity height selection while keeping geoid grid assets caller-supplied
- add `geo-types` geometry-level transform support for points, line strings, polygons with holes, multi-geometries, rectangles, geometry collections, and `Geometry` enum values
- add antimeridian-aware geographic AOI and bounds support without weakening projected bounds validation
- add fluent `SelectionOptions` builders and option-aware `proj-wkt::Proj` facade constructors for area-of-interest, grid policy, approximate fallback, and explicit operation selection
- add EPSG:32662 Plate Carree registry lookup, operation selection, and reference-corpus coverage
- make approximate Helmert fallback an explicit opt-in policy: `BestAvailable` no longer synthesizes approximate Helmert fallbacks, and selection errors explain how to enable them
- fix `Transform::inverse()` so inverse transforms preserve compiled fallback pipelines and diagnostics, including grid coverage miss reporting when an inverse falls back to another operation
- fix 2D diagnostic conversion so XY-only callers do not sample vertical grids
- reject invalid source coordinates on identity/same-datum transform paths instead of allowing non-finite or out-of-range values to bypass projection validation
- normalize projection longitude deltas across implemented projection families so wrapped longitudes remain stable near projection centers and seams
- optimize transform execution by compiling source/target XY unit conversion modes into the pipeline and reducing Rayon parallel batch temporary allocations
- optimize grid loading with a single-flight parse/checksum cache and filesystem grid path caching so repeated lookups avoid duplicated parsing and filesystem traversal
- bound grid loading and parsing with filesystem read limits, NTv2 and GTX resource size limits, NTv2 cell-count and non-finite shift validation, GTX cell-count validation, and safer GTX edge/longitude handling
- fix PROJ-string projected unit handling so `+x_0` and `+y_0` remain PROJ-compatible meter parameters while `+units` controls output coordinates
- reject malformed WKT and PROJJSON projection parameter values and non-finite unit factors instead of silently ignoring invalid fields
- fix WKT2 axis-unit handling so projected axis `LENGTHUNIT` declarations drive native linear units when no top-level unit exists, geographic axis angular units are validated, and inconsistent axis units are rejected
- validate CRS URN structure and authority so only `urn:ogc:def:crs:EPSG:...` URNs resolve through the EPSG registry
- validate CRS definition ellipsoid dimensions and Helmert/datum parameters before transform construction
- deduplicate WKT and PROJJSON semantic parsing helpers for datum candidates, unit normalization, parameter mapping, and vertical unit authority validation without intended behavior changes

## 0.5.0 - 2026-04-29

- add explicit `VerticalCrsDef`, `VerticalCrsKind`, `HorizontalCrsDef`, and `CompoundCrsDef` CRS model types for ellipsoidal-height and gravity-related vertical components
- add EPSG:4979 and WKT/PROJJSON 3D geographic and compound CRS parsing paths while preserving `z` only when vertical CRS components are identical
- reject explicit vertical CRS mismatches, standalone vertical CRS transform requests, and vertical/geoid transformation semantics instead of silently preserving ambiguous heights
- add horizontal-only transform constructors for compound CRS inputs, covering XY AOI/preview workflows without weakening default vertical fail-closed behavior
- add vertical transform diagnostics and same-reference vertical unit conversion for compound CRS definitions
- add caller-supplied `VerticalGridOperation` support for ellipsoidal-to-gravity height transforms backed by NOAA/VDatum binary GTX grids
- select vertical grids with declared sampling CRS handling, area-of-use ordering, runtime fallback across coverage misses, resolved SHA-256 grid checksums, and safe filesystem resource paths
- add supported vertical CRS lookup for EPSG:3855, 5702, 5703, 5773, and 6360 and canonicalize WKT/PROJJSON vertical components from vertical CRS EPSG identifiers when datum identifiers are absent
- make embedded EPSG registry generation deterministic with PROJ database provenance from `registry::embedded_registry_provenance_json()`, `epsg.bin` reproducibility checks, and CI coverage for the generator crate

## 0.4.0 - 2026-04-28

- add Lambert Azimuthal Equal Area, EPSG Oblique Stereographic, Hotine Oblique Mercator variants A/B, and Cassini-Soldner projection families, extending embedded projected CRS coverage for common European, Swiss, North American, Malaysian, and Caribbean grids
- add PROJ string `+nadgrids` support for NTv2-backed custom datum shifts through the existing `GridProvider` pipeline, including optional grid entries and explicit rejection of conflicting or unsupported grid parameters
- expose grid-backed datum shifts in the public CRS model and add CRS-definition transform constructors with selection options for caller-supplied grid providers
- make public CRS and datum definition values owned rather than `Copy` so grid-backed metadata can be represented without static-only constraints
- normalize non-metre EPSG ellipsoid axes to metres in the generated registry so historical CRS definitions with foot-based ellipsoids use correct projection radii
- tighten Swiss Hotine Oblique Mercator variant B parity with C PROJ, including the Swiss right-angle kernel and inverse convergence behavior
- refresh live bundled C PROJ parity coverage, reference values, and benchmark coverage for the new projection and grid cases
- continue to scope the release as a supported `0.x` line rather than a claim of full PROJ feature parity

## 0.3.0 - 2026-04-16

- add embedded coordinate-operation metadata and selection APIs, including explicit operation lookup, `Transform::from_operation`, `Transform::with_selection_options`, selected-operation introspection, and detailed selection diagnostics
- add NTv2 grid runtime support with embedded and application-provided grid providers, recursive concatenated operation handling, and bundled registry-backed grid resources
- expand the embedded EPSG registry with explicit datum-shift states, coordinate operations, areas of use, grid definitions, and CRS names
- make transform construction operation-aware with typed area-of-interest selection, inverse-aware metadata, and indexed operation lookup instead of registry-wide scans
- switch projections onto precomputed enum-backed hot paths and compiled transform pipelines, improving construction and execution costs
- tighten CRS parsing semantics across WKT, PROJJSON, and authority wrappers, rejecting contradictory or unsupported inputs instead of silently normalizing them
- refresh live bundled C PROJ parity coverage, benchmark coverage, and the published benchmark report for the new operation-selection and 3D paths
- continue to scope the release as a supported `0.x` line rather than a claim of full PROJ feature parity

## 0.2.0 - 2026-04-01

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

## 0.1.0 - 2026-03-21

- initial public release
- add a pure-Rust CRS transform engine with no C dependencies, no build scripts, and no unsafe code
- add a built-in EPSG registry for supported geographic and projected CRS definitions
- add projection support for Web Mercator, Transverse Mercator / UTM, Polar Stereographic, Lambert Conformal Conic, Albers Equal Area, Mercator, and Equidistant Cylindrical
- add datum-shift support for the built-in Helmert-backed datums in the registry
- add `proj-wkt` parser support for EPSG codes, common WKT, PROJ strings, and basic PROJJSON inputs for the implemented projection families
- add a lightweight compatibility facade for common downstream `Proj`-style construction and coordinate conversion flows
- scope the release as production-ready for the supported projection families and CRS formats in this repository
- avoid claiming full PROJ feature parity
