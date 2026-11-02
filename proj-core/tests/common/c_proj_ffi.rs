#![allow(unsafe_code)]
// Shared by the 3D parity test and the compare bench, which use different
// constructor subsets.
#![allow(dead_code)]

use proj_sys::{
    proj_area_create, proj_area_destroy, proj_context_create, proj_context_destroy,
    proj_context_errno, proj_create, proj_create_crs_to_crs, proj_destroy, proj_errno,
    proj_errno_reset, proj_errno_string, proj_normalize_for_visualization, proj_trans, PJ_CONTEXT,
    PJ_COORD, PJ_DIRECTION_PJ_FWD, PJ_XYZT,
};
use std::ffi::{CStr, CString};

pub fn assert_reference_version() {
    let info = unsafe { proj_sys::proj_info() };
    assert!(
        (info.major, info.minor, info.patch) >= (9, 8, 0),
        "coordinate references require PROJ 9.8+ (found {}.{}.{}); set PKG_CONFIG_PATH to a PROJ 9.8+ installation",
        info.major,
        info.minor,
        info.patch
    );
}

pub struct CProjTransform {
    ctx: *mut PJ_CONTEXT,
    pj: *mut proj_sys::PJ,
}

impl CProjTransform {
    pub fn new_known_crs(from: &str, to: &str) -> Result<Self, String> {
        let from = CString::new(from).map_err(|e| format!("invalid source CRS {from}: {e}"))?;
        let to = CString::new(to).map_err(|e| format!("invalid target CRS {to}: {e}"))?;

        unsafe {
            let ctx = proj_context_create();
            if ctx.is_null() {
                return Err("failed to create PROJ context".into());
            }

            let area = proj_area_create();
            let raw = proj_create_crs_to_crs(ctx, from.as_ptr(), to.as_ptr(), area);
            proj_area_destroy(area);

            if raw.is_null() {
                let err = proj_context_errno(ctx);
                let message = error_message(err);
                proj_context_destroy(ctx);
                return Err(format!("failed to create C PROJ transform: {message}"));
            }

            let normalized = proj_normalize_for_visualization(ctx, raw);
            proj_destroy(raw);

            if normalized.is_null() {
                let err = proj_context_errno(ctx);
                let message = error_message(err);
                proj_context_destroy(ctx);
                return Err(format!("failed to normalize C PROJ transform: {message}"));
            }

            Ok(Self {
                ctx,
                pj: normalized,
            })
        }
    }

    /// Create a transform in each CRS's authority axis order, without the
    /// longitude-first normalization of [`Self::new_known_crs`].
    pub fn new_known_crs_authority_axes(from: &str, to: &str) -> Result<Self, String> {
        let from = CString::new(from).map_err(|e| format!("invalid source CRS {from}: {e}"))?;
        let to = CString::new(to).map_err(|e| format!("invalid target CRS {to}: {e}"))?;

        unsafe {
            let ctx = proj_context_create();
            if ctx.is_null() {
                return Err("failed to create PROJ context".into());
            }

            let area = proj_area_create();
            let pj = proj_create_crs_to_crs(ctx, from.as_ptr(), to.as_ptr(), area);
            proj_area_destroy(area);

            if pj.is_null() {
                let message = error_message(proj_context_errno(ctx));
                proj_context_destroy(ctx);
                return Err(format!("failed to create C PROJ transform: {message}"));
            }

            Ok(Self { ctx, pj })
        }
    }

    /// Create a transform from one explicit EPSG coordinate operation.
    ///
    /// Using the operation URN avoids late-binding selection differences and
    /// lets parity tests exercise the same registry record in both engines.
    pub fn new_coordinate_operation(operation_epsg: u32) -> Result<Self, String> {
        let definition = CString::new(format!(
            "urn:ogc:def:coordinateOperation:EPSG::{operation_epsg}"
        ))
        .expect("EPSG operation URNs have no NUL");

        unsafe {
            let ctx = proj_context_create();
            if ctx.is_null() {
                return Err("failed to create PROJ context".into());
            }

            let raw = proj_create(ctx, definition.as_ptr());
            if raw.is_null() {
                let message = error_message(proj_context_errno(ctx));
                proj_context_destroy(ctx);
                return Err(format!(
                    "failed to create C PROJ operation EPSG:{operation_epsg}: {message}"
                ));
            }

            let normalized = proj_normalize_for_visualization(ctx, raw);
            proj_destroy(raw);
            if normalized.is_null() {
                let message = error_message(proj_context_errno(ctx));
                proj_context_destroy(ctx);
                return Err(format!(
                    "failed to normalize C PROJ operation EPSG:{operation_epsg}: {message}"
                ));
            }

            Ok(Self {
                ctx,
                pj: normalized,
            })
        }
    }

    pub fn convert_3d(&self, coord: (f64, f64, f64)) -> Result<(f64, f64, f64), String> {
        unsafe {
            proj_errno_reset(self.pj);
            let trans = proj_trans(
                self.pj,
                PJ_DIRECTION_PJ_FWD,
                PJ_COORD {
                    xyzt: PJ_XYZT {
                        x: coord.0,
                        y: coord.1,
                        z: coord.2,
                        t: f64::INFINITY,
                    },
                },
            );

            let err = proj_errno(self.pj);
            if err != 0 {
                return Err(format!("C PROJ convert failed: {}", error_message(err)));
            }

            Ok((trans.xyzt.x, trans.xyzt.y, trans.xyzt.z))
        }
    }
}

impl Drop for CProjTransform {
    fn drop(&mut self) {
        unsafe {
            if !self.pj.is_null() {
                proj_destroy(self.pj);
            }
            if !self.ctx.is_null() {
                proj_context_destroy(self.ctx);
            }
        }
    }
}

fn error_message(err: i32) -> String {
    unsafe {
        let ptr = proj_errno_string(err);
        if ptr.is_null() {
            return format!("PROJ error code {err}");
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}
