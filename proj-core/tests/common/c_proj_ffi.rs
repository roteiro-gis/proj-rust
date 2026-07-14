#![allow(unsafe_code)]
// Shared by the 3D parity test and the compare bench, which use different
// constructor subsets.
#![allow(dead_code)]

use proj_sys::{
    proj_area_create, proj_area_destroy, proj_context_create, proj_context_destroy,
    proj_context_errno, proj_create, proj_create_crs_to_crs, proj_create_crs_to_crs_from_pj,
    proj_crs_promote_to_3D, proj_destroy, proj_errno, proj_errno_reset, proj_errno_string,
    proj_normalize_for_visualization, proj_trans, PJ_CONTEXT, PJ_COORD, PJ_DIRECTION_PJ_FWD,
    PJ_XYZT,
};
use std::ffi::{CStr, CString};
use std::ptr;

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

    /// Transform through the 3D promotions of both CRSs
    /// (`proj_crs_promote_to_3D`), so datum-shift-induced ellipsoidal height
    /// changes appear instead of the 2D `push/pop v_3` height passthrough.
    pub fn new_promoted_3d(from_epsg: u32, to_epsg: u32) -> Result<Self, String> {
        unsafe {
            let ctx = proj_context_create();
            if ctx.is_null() {
                return Err("failed to create PROJ context".into());
            }

            let from_crs = match create_promoted_crs(ctx, from_epsg) {
                Ok(crs) => crs,
                Err(err) => {
                    proj_context_destroy(ctx);
                    return Err(err);
                }
            };
            let to_crs = match create_promoted_crs(ctx, to_epsg) {
                Ok(crs) => crs,
                Err(err) => {
                    proj_destroy(from_crs);
                    proj_context_destroy(ctx);
                    return Err(err);
                }
            };

            let area = proj_area_create();
            let raw = proj_create_crs_to_crs_from_pj(ctx, from_crs, to_crs, area, ptr::null());
            proj_area_destroy(area);
            proj_destroy(from_crs);
            proj_destroy(to_crs);

            if raw.is_null() {
                let message = error_message(proj_context_errno(ctx));
                proj_context_destroy(ctx);
                return Err(format!("failed to create promoted 3D transform: {message}"));
            }

            let normalized = proj_normalize_for_visualization(ctx, raw);
            proj_destroy(raw);

            if normalized.is_null() {
                let message = error_message(proj_context_errno(ctx));
                proj_context_destroy(ctx);
                return Err(format!(
                    "failed to normalize promoted 3D transform: {message}"
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

unsafe fn create_promoted_crs(
    ctx: *mut PJ_CONTEXT,
    code: u32,
) -> Result<*mut proj_sys::PJ, String> {
    let def = CString::new(format!("EPSG:{code}")).expect("EPSG code strings have no NUL");
    let crs = proj_create(ctx, def.as_ptr());
    if crs.is_null() {
        return Err(format!(
            "failed to create CRS EPSG:{code}: {}",
            error_message(proj_context_errno(ctx))
        ));
    }
    let crs_3d = proj_crs_promote_to_3D(ctx, ptr::null(), crs);
    proj_destroy(crs);
    if crs_3d.is_null() {
        return Err(format!(
            "failed to promote CRS EPSG:{code} to 3D: {}",
            error_message(proj_context_errno(ctx))
        ));
    }
    Ok(crs_3d)
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
