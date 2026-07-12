# Changelog

## Unreleased

- accept EPSG-tagged WKT1 `COMPD_CS` definitions whose horizontal component uses the authority-native axis permutation, including SWEREF99 TM + RH2000 height (EPSG:5845), while continuing to reject unsupported custom axis semantics
- add `Transform::new_horizontal`, `Transform::new_horizontal_with_selection_options`, and `Transform::from_epsg_horizontal` for explicit XY-only transforms from compound authority-code CRSs without silently weakening the vertical-safe constructors
- fix grad-unit conversion parameters in the registry generator: grads were multiplied by the arc-second factor (and arc-seconds by the arc-minute factor), so all 21 included CRSs whose EPSG parameters are stated in grads — the NTF (Paris) Lambert zones and Nord de Guerre, the Carthage, Voirol, Nord Sahara and Merchich Lamberts, and Deir ez Zor / Levant Stereographic — produced garbage coordinates; angular factors are now exact per-unit constants cross-checked against proj.db at generation time, and new corpus points pin NTF (Paris) / Lambert zone II and the Levant Stereographic against C PROJ
- add Polar Stereographic variant C (EPSG method 9830, the Petrels 1972 and Perroud 1950 Terre Adelie grids), growing projected CRS coverage to 5,233; C PROJ has no implementation of this method, so the implementation is pinned directly to the EPSG Guidance Note worked example (the variant reduces to the standard-parallel form with the false northing offset by the standard parallel's radius)
- add the Azimuthal Equidistant projection family: EPSG methods 1125 (e.g. the WGS 84 / Equi7 continental grids) and 9832 Modified Azimuthal Equidistant (Guam 1963 / Yap Islands) through geodesic azimuth/distance as in C PROJ (Karney's algorithms via the pure-Rust `geographiclib-rs`), and 9831 Guam Projection (Guam 1963 / Guam SPCS) in its closed form; projected CRS coverage grows to 5,231, pinned by C PROJ test vectors and the EPSG Guidance Note worked examples for Yap and Guam
- replace the meridional-arc series with C PROJ 9's third-flattening expansion (Karney, arXiv:2212.05818), providing the inverse (arc→latitude) the polar Azimuthal Equidistant and Guam projections need; American Polyconic now shares the same engine
- add the American Polyconic projection (EPSG method 9818, e.g. SIRGAS 2000 / Brazil Polyconic and Panama-Colon 1911 / Panama Polyconic), growing projected CRS coverage to 5,222; verified against C PROJ's own test vectors and reference corpus points
- add the Equal Earth projection (EPSG method 1078, EPSG:8857/8858/8859), growing projected CRS coverage to 5,218; verified against C PROJ's own test vectors and reference corpus points
- add the Krovak north-orientated projections (EPSG methods 1041 and 1043, e.g. S-JTSK / Krovak East North and S-JTSK/05 / Modified Krovak East North), growing projected CRS coverage to 5,215; the cone geometry is taken from the EPSG parameters rather than hardcoded as in C PROJ, and the modified variant applies the S-JTSK/05 polynomial distortion correction (documented sub-centimetre divergence from C PROJ on EPSG:5516, whose stored cone-axis co-latitude is rounded to 30°17'17.303" while C PROJ hardcodes 30°17'17.30311")
- fix `CrsDef::semantically_equivalent` for projection methods added after the comparison ladder was written (Colombia Urban, LCC Michigan, LCC 1SP variant B): identical definitions compared as not equivalent; method equivalence is now an exhaustive canonical-parameters match that new methods must extend to compile
- fix Lambert Conformal Conic 1SP: the registry only read the 2SP false-origin parameter set, leaving all 235 non-deprecated LCC 1SP CRSs (e.g. Jamaica 1969 / National Grid) unusable at transform construction; the LCC model gains the natural-origin scale factor (`k0`), carried through all parsers and serializers
- add LCC 2SP Michigan (EPSG method 1051) and LCC 1SP variant B (1102), reusing the LCC implementation via a scaled ellipsoid and a false-origin constructor; projected CRS coverage grows to 5,210
- add the Colombia Urban projection (EPSG method 1052), growing projected CRS coverage from 5,171 to 5,203 (the MAGNA-SIRGAS urban grids); verified against C PROJ and the EPSG Guidance Note worked example
- fail closed when a geoid-grid vertical transform would compose with a Helmert/geocentric horizontal pipeline: the datum shift's ellipsoidal-height change cannot yet be applied through a geoid transformation, so construction reports a typed error instead of producing silently wrong heights
- breaking: registry format v9 — datum records drop their stored to-WGS84 Helmert values (identity is the datum's EPSG code; `Datum` gains `epsg()`/`with_epsg` and `same_datum` compares codes when both sides carry one), and a new datum alias index (1,664 EPSG names and aliases) lets WKT/PROJ-string parsing resolve any registry datum by name instead of only the 8 curated ones, fail-closed on ellipsoid mismatch
- breaking: `Error`, `GridError`, and `ParseError` are `#[non_exhaustive]`; non-convergent inverse iterations report a structured `Error::NonConvergence { context, iterations }`
- breaking: `Transformable`/`Transformable3D` conversion methods are borrow-based (`to_coord(&self)`/`to_coord3d(&self)`), and `convert_batch`/`convert_batch_3d` no longer require `Clone`
- breaking: `TransformOutcome::operation` and `GridCoverageMiss::operation` are `Arc<CoordinateOperationMetadata>`; diagnostics conversions share the compiled metadata instead of cloning strings per point
- add `proj_wkt::to_wkt2`: WKT2 (ISO 19162) serialization for geographic, projected, and compound definitions, sharing the WKT1 serializer's mapping; roundtrips are asserted for all supported methods and fuzzed alongside WKT1
- registry CRS names are served zero-copy from the embedded blob instead of leaking ~6.7k heap copies at first registry access
- add `proj_wkt::to_projjson`: full-body PROJJSON serialization for geographic, projected, and compound definitions that reparses to equivalent definitions, sharing the WKT serializer's method/parameter/datum mapping; fixes latent parser gaps the roundtrip surfaced (strict datum equality in PROJJSON canonicalization, projection-method equivalence covering only 5 of 13 methods, unit type tags ignored during axis unit extraction) and adds a `projjson_roundtrip` fuzz target
- `Transform` implements `Clone` (grid data is Arc-shared) and a summary-form `Debug`; a compile-time assertion pins `Send + Sync + Clone + Debug`
- add an optional `serde` feature deriving `Serialize`/`Deserialize` for coordinate and operation-metadata value types

- model EPSG supersession: operations with a same-CRS-pair replacement carry a new registry flag (`CoordinateOperation::superseded`) and rank below their replacements during selection, matching C PROJ (selection parity improves from 202 to 218 of 250 probes); the generator also re-derives its hand-curated list premises from proj.db at generation time
- add coverage-guided fuzzing (workspace-excluded `fuzz/` crate) for the CRS parsers, the WKT parse→emit→reparse cycle, and the NTv2/GTX/GeoTIFF grid parsers, with seed corpora and a nightly/PR CI workflow
- fix WKT serialization of compound ellipsoidal-height CRSs: the vertical datum authority is now resolved from the horizontal CRS instead of emitting `VERT_DATUM["Unknown datum"]`, and definition-identity cross-checks no longer reuse the fail-closed operation-selection datum equality (found by the new `wkt_roundtrip` fuzz target)
- add `proj-epsg-format`, a zero-dependency crate that is now the single source of truth for the embedded registry's binary layout, shared by the `proj-core` reader and the `gen-registry` writer (previously ~40 hand-synced constant definitions); registry bytes are unchanged and the source-audit test forbids redefinitions
- replace the hand-rolled SHA-256 grid checksum implementation with the `sha2` crate (unchanged output, FIPS 180-4 vectors added)
- gate supply-chain health with cargo-deny in CI (advisories/licenses/bans/sources); updated crossbeam-epoch past RUSTSEC-2026-0204
- add a CI coverage job (cargo-llvm-cov) and a linear-parse-time regression test for adversarial WKT parameter lists; the live C PROJ parity workflow gains a weekly schedule and path-filtered PR triggers

- breaking (behavior): 3D transforms between CRSs without vertical components now propagate datum-shift-induced ellipsoidal height changes instead of preserving the caller's `z`, matching C PROJ's promoted-3D CRS semantics; compound-CRS transforms with gravity-related vertical components are unchanged
- breaking (behavior): polar stereographic extends the conformal-latitude formula continuously across the equator, so opposite-hemisphere inputs map to their true large-radius coordinates (matching C PROJ) instead of silently mirroring into the projection's hemisphere
- breaking (behavior): iterative inverse computations (Mercator, Lambert Conformal Conic, Polar Stereographic, Hotine Oblique Mercator, Albers, Oblique Stereographic, geocentric-to-geodetic, NTv2 inverse shift) return a typed error on non-convergence instead of silently returning the last iterate; a shared convergence helper replaces four duplicated latitude iterations
- replace the Snyder transverse Mercator series with the exact Poder/Engsager formulation C PROJ uses by default: near-pole inverses no longer lose longitude to cancellation, and coordinates beyond the conformal-easting domain produce a typed error
- compute the exact closed-form Helmert inverse instead of the first-order parameter negation; forward/inverse roundtrips now hold to machine precision at any rotation magnitude
- wrap out-of-branch longitudes into the grid frame during NTv2 sampling (e.g. 358° resolves like -2°), matching existing GTX behavior
- rank equally accurate area-matched operations by area-of-use specificity (smaller extent first), as C PROJ does
- add a committed operation-selection parity corpus generated from C PROJ's late-binding choices (`gen-selection-parity`), asserted by a default-run test with documented known divergences (EPSG supersession-driven variant preferences are not modeled yet)
- expand the reference corpus (161 → 192 points) with precision points for LCC/Albers/Cassini/Mercator/Equidistant Cylindrical, near-pole transverse Mercator inverses, wrong-hemisphere polar stereographic inputs, and promoted-3D cross-datum height references; corpus schema gains optional `z` fields and a `pending_fix` divergence marker
- fix the property-test RNG seed for deterministic runs and verify the declared MSRV (1.85) in CI

## 0.9.0 - 2026-07-06

- breaking: remove opt-in synthetic Helmert datum-shift fallback selection and related legacy API surface; `SelectionPolicy::AllowApproximateHelmertFallback`, `SelectionOptions::allow_approximate_helmert_fallback`, `Datum::approximate_helmert_to`, and `proj-wkt` compatibility aliases for that option are no longer supported, and CRS pairs without a registry, identity, or supported grid/identity custom datum operation now fail during transform construction
- add `proj_wkt::to_wkt` for serializing `CrsDef` values to WKT1-style `GEOGCS`, `PROJCS`, and `COMPD_CS` definitions, including datum, spheroid, prime meridian, unit, authority, and vertical CRS metadata
- add WKT serializer coverage for every `ProjectionMethod` variant currently represented by `proj-core`, including fail-closed errors for invalid projection emission state
- preserve metre, US survey foot, and international foot linear units in emitted WKT, with round-trip coverage for foot-based State Plane CRS definitions and compound vertical CRS definitions
- expose datum and ellipsoid authority lookup helpers from `proj-core` so serializers can preserve generated EPSG registry metadata
- replace the RDNAP-specific registry generator aliases with a generic generated-operation graph pass that composes PROJ grid alternatives with known zero/identity bridge operations
- document and test the registry-only operation-selection model: selector candidates are limited to registry/generated-registry operations, explicit caller/parser-provided operations, and internal identity behavior
- reduce datum WGS84 relationship values to definition metadata during operation selection; registry or explicit custom operations now remain authoritative for CRS-to-CRS transforms
- move parsed PROJ `+nadgrids` horizontal datum shifts out of selector internals and into explicit custom coordinate operations supplied by `proj-wkt` transform construction paths
- add explicit custom horizontal coordinate operation candidates through `SelectionOptions::with_coordinate_operation` and `SelectionOptions::with_coordinate_operations`, with custom selection diagnostics and direct pipeline compilation

## 0.8.0 - 2026-06-24

- breaking: bump the workspace crates to `0.8.0`; `SelectionOptions` now includes the public `area_bounds_densify_points` field for configurable AOI bounds sampling, so downstream struct-literal construction must set it or use `SelectionOptions::new()` / `Default`
- add generated EPSG vertical CRS and compound CRS records, including generated registry metadata for RDNAPTRANS2018 compound CRS/grid operations instead of hand-coded RDNAP entries
- add configurable AOI bounds densification through `SelectionOptions::with_area_bounds_densify_points`, with `MAX_BOUNDS_DENSIFY_POINTS` bounding CPU work for AOI and bounds sampling APIs
- add `Transform::convert_rect` so `geo_types::Rect` callers can choose bounds sampling density; default `convert_geometry` rectangle conversion now densifies edges instead of using corners only
- fix 3D horizontal transforms so Helmert/geocentric coordinate math uses the source height while horizontal-only transforms continue to preserve the caller's `z`
- revalidate cached filesystem grid paths before reuse and open grid files without following symlinks on Unix to reject stale path and symlink swaps
- bound GeoTIFF grid parsing with IFD, dimension, total-cell, and band-count limits before decoding untrusted grid data
- add CodeQL analysis coverage

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
