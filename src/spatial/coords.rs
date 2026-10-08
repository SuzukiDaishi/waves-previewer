//! Positions: ADM polar <-> ADM Cartesian, and where each speaker stands in
//! the Cartesian room.
//!
//! The polar/Cartesian mapping is the one ITU-R BS.2127 section 10 describes
//! for converting object positions, written from its text: azimuth sectors
//! bounded by the nominal 0 / 30 / 110 / 180 degree speaker directions are
//! mapped onto the edges of the unit square (so M+030 lands on the room's
//! front-left corner, not at 30 degrees off the front wall), with a tangent
//! law inside each sector, and elevation above 30 degrees is stretched so the
//! ceiling is reached at 90. It is its own inverse, which is what lets a
//! polar element be edited in a Cartesian view and written back in polar.
//!
//! Azimuths here are ADM's: positive to the LEFT. The app's own
//! `SpeakerPos::direction` is positive to the right; convert with
//! [`adm_azimuth_to_app`] / [`app_azimuth_to_adm`].

use crate::audio_channels::SpeakerPos;

/// Elevation of the upper layer of nominal speaker directions, in degrees.
pub const EL_TOP_DEG: f32 = 30.0;
/// Where that elevation lands in the Cartesian room: 45 degrees up a cube
/// face, the corner of the upper layer.
pub const EL_TOP_TILDE_DEG: f32 = 45.0;

/// Below this, a coordinate is treated as zero (the room's centre).
const EPS: f32 = 1e-6;

/// The azimuths (ADM, degrees, counter-clockwise from the front) bounding the
/// sectors, and the corner or edge midpoint of the unit square each lands on.
const SECTORS: [(f32, [f32; 2]); 6] = [
    (0.0, [0.0, 1.0]),
    (30.0, [-1.0, 1.0]),
    (110.0, [-1.0, -1.0]),
    (180.0, [0.0, -1.0]),
    (250.0, [1.0, -1.0]),
    (330.0, [1.0, 1.0]),
];

pub fn adm_azimuth_to_app(azimuth: f32) -> f32 {
    -azimuth
}

pub fn app_azimuth_to_adm(azimuth: f32) -> f32 {
    -azimuth
}

/// An azimuth in (-180, 180].
pub fn wrap_azimuth(deg: f32) -> f32 {
    let wrapped = (deg + 180.0).rem_euclid(360.0) - 180.0;
    if wrapped <= -180.0 {
        180.0
    } else {
        wrapped
    }
}

/// The sector (index into [`SECTORS`]) holding `az`, given in [0, 360).
fn sector_of(az: f32) -> usize {
    let mut index = SECTORS.len() - 1;
    for (i, (start, _)) in SECTORS.iter().enumerate() {
        if az >= *start {
            index = i;
        }
    }
    index
}

fn sector_bounds(index: usize) -> (f32, f32, [f32; 2], [f32; 2]) {
    let (a0, p0) = SECTORS[index];
    let (a1, p1) = SECTORS[(index + 1) % SECTORS.len()];
    let a1 = if a1 <= a0 { a1 + 360.0 } else { a1 };
    (a0, a1, p0, p1)
}

/// Where a direction meets the unit square, as a point on its edge.
fn azimuth_to_square(azimuth: f32) -> [f32; 2] {
    let az = azimuth.rem_euclid(360.0);
    let index = sector_of(az);
    let (a0, a1, p0, p1) = sector_bounds(index);
    let mid = (a0 + a1) * 0.5;
    let half = (a1 - a0) * 0.5;
    let t = 0.5 * (1.0 + (az - mid).to_radians().tan() / half.to_radians().tan());
    let p = t.atan2(1.0 - t) * std::f32::consts::FRAC_2_PI;
    [p0[0] + (p1[0] - p0[0]) * p, p0[1] + (p1[1] - p0[1]) * p]
}

/// ADM polar (azimuth left-positive, elevation, distance) to ADM Cartesian.
pub fn polar_to_cart(azimuth: f32, elevation: f32, distance: f32) -> [f32; 3] {
    let el = elevation.clamp(-90.0, 90.0);
    let (z, r_xy) = if el.abs() > EL_TOP_DEG {
        let el_tilde = EL_TOP_TILDE_DEG
            + (90.0 - EL_TOP_TILDE_DEG) * (el.abs() - EL_TOP_DEG) / (90.0 - EL_TOP_DEG);
        (
            distance * el.signum(),
            distance * (90.0 - el_tilde).to_radians().tan(),
        )
    } else {
        let el_tilde = EL_TOP_TILDE_DEG * el / EL_TOP_DEG;
        (distance * el_tilde.to_radians().tan(), distance)
    };
    let [x, y] = azimuth_to_square(azimuth);
    [r_xy * x, r_xy * y, z]
}

/// ADM Cartesian to ADM polar `[azimuth, elevation, distance]`, the inverse
/// of [`polar_to_cart`].
pub fn cart_to_polar(x: f32, y: f32, z: f32) -> [f32; 3] {
    if x.abs() < EPS && y.abs() < EPS {
        return if z.abs() < EPS {
            [0.0, 0.0, 0.0]
        } else {
            [0.0, 90.0 * z.signum(), z.abs()]
        };
    }
    // The sector whose square edge the ray through (x, y) crosses: solve
    // (x, y) = r * (p0 + p * (p1 - p0)) for r > 0 and p in [0, 1].
    let mut found = None;
    for index in 0..SECTORS.len() {
        let (a0, a1, p0, p1) = sector_bounds(index);
        let d = [p1[0] - p0[0], p1[1] - p0[1]];
        let det = p0[0] * d[1] - p0[1] * d[0];
        if det.abs() < EPS {
            continue;
        }
        let r = (x * d[1] - y * d[0]) / det;
        let rp = (p0[0] * y - p0[1] * x) / det;
        if r <= 0.0 {
            continue;
        }
        let p = rp / r;
        if (-EPS..=1.0 + EPS).contains(&p) {
            found = Some((a0, a1, p.clamp(0.0, 1.0), r));
            break;
        }
    }
    let Some((a0, a1, p, r_xy)) = found else {
        return [0.0, 0.0, 0.0];
    };
    let mid = (a0 + a1) * 0.5;
    let half = (a1 - a0) * 0.5;
    let k = (p * std::f32::consts::FRAC_PI_2).tan();
    let t = if k.is_finite() { k / (1.0 + k) } else { 1.0 };
    let azimuth = mid
        + ((2.0 * t - 1.0) * half.to_radians().tan())
            .atan()
            .to_degrees();
    let el_tilde = z.atan2(r_xy).to_degrees();
    let (elevation, distance) = if el_tilde.abs() > EL_TOP_TILDE_DEG {
        (
            el_tilde.signum()
                * (EL_TOP_DEG
                    + (90.0 - EL_TOP_DEG) * (el_tilde.abs() - EL_TOP_TILDE_DEG)
                        / (90.0 - EL_TOP_TILDE_DEG)),
            z.abs(),
        )
    } else {
        (EL_TOP_DEG * el_tilde / EL_TOP_TILDE_DEG, r_xy)
    };
    [wrap_azimuth(azimuth), elevation, distance]
}

/// Where a speaker stands in the Cartesian room the panner and the views
/// use: the ear layer at Z = 0, the upper layer at Z = 1, the front wall at
/// Y = 1. `None` for the LFE, which has no position.
pub fn speaker_cart(pos: SpeakerPos) -> Option<[f32; 3]> {
    use SpeakerPos::*;
    Some(match pos {
        Fl => [-1.0, 1.0, 0.0],
        Fr => [1.0, 1.0, 0.0],
        Fc => [0.0, 1.0, 0.0],
        Lfe => return None,
        Flc => [-0.5, 1.0, 0.0],
        Frc => [0.5, 1.0, 0.0],
        Wl => [-1.0, 0.5, 0.0],
        Wr => [1.0, 0.5, 0.0],
        Sl => [-1.0, 0.0, 0.0],
        Sr => [1.0, 0.0, 0.0],
        Bl => [-1.0, -1.0, 0.0],
        Br => [1.0, -1.0, 0.0],
        Bc => [0.0, -1.0, 0.0],
        Tfl => [-1.0, 1.0, 1.0],
        Tfr => [1.0, 1.0, 1.0],
        Tfc => [0.0, 1.0, 1.0],
        Tsl => [-1.0, 0.0, 1.0],
        Tsr => [1.0, 0.0, 1.0],
        Tbl => [-1.0, -1.0, 1.0],
        Tbr => [1.0, -1.0, 1.0],
        Tbc => [0.0, -1.0, 1.0],
        Tc => [0.0, 0.0, 1.0],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < tol)
    }

    #[test]
    fn nominal_directions_land_on_the_square() {
        assert!(close(polar_to_cart(0.0, 0.0, 1.0), [0.0, 1.0, 0.0], 1e-5));
        // ADM azimuth is positive to the left: M+030 is the front-left corner.
        assert!(close(polar_to_cart(30.0, 0.0, 1.0), [-1.0, 1.0, 0.0], 1e-5));
        assert!(close(polar_to_cart(-30.0, 0.0, 1.0), [1.0, 1.0, 0.0], 1e-5));
        assert!(close(
            polar_to_cart(110.0, 0.0, 1.0),
            [-1.0, -1.0, 0.0],
            1e-5
        ));
        assert!(close(
            polar_to_cart(180.0, 0.0, 1.0),
            [0.0, -1.0, 0.0],
            1e-5
        ));
        // The middle of a sector is the middle of its edge.
        assert!(close(polar_to_cart(70.0, 0.0, 1.0), [-1.0, 0.0, 0.0], 1e-5));
        // U+030 is the upper front-left corner; straight up is the ceiling.
        assert!(close(
            polar_to_cart(30.0, 30.0, 1.0),
            [-1.0, 1.0, 1.0],
            1e-5
        ));
        assert!(close(polar_to_cart(0.0, 90.0, 1.0), [0.0, 0.0, 1.0], 1e-5));
        // Distance scales the point towards the listener.
        assert!(close(polar_to_cart(0.0, 0.0, 0.5), [0.0, 0.5, 0.0], 1e-5));
    }

    #[test]
    fn cart_and_polar_are_each_others_inverse() {
        for az in [
            -170.0f32, -110.0, -45.0, -30.0, 0.0, 12.5, 30.0, 90.0, 135.0, 179.0,
        ] {
            for el in [-60.0f32, -10.0, 0.0, 20.0, 30.0, 50.0, 85.0] {
                for dist in [0.3f32, 1.0] {
                    let [x, y, z] = polar_to_cart(az, el, dist);
                    let [az2, el2, d2] = cart_to_polar(x, y, z);
                    assert!(
                        (wrap_azimuth(az2 - az)).abs() < 0.05,
                        "az {az} el {el}: {az2}"
                    );
                    assert!((el2 - el).abs() < 0.05, "az {az} el {el}: el {el2}");
                    assert!((d2 - dist).abs() < 1e-3, "az {az} el {el}: d {d2}");
                }
            }
        }
        assert_eq!(cart_to_polar(0.0, 0.0, 0.0), [0.0, 0.0, 0.0]);
        assert_eq!(cart_to_polar(0.0, 0.0, 1.0), [0.0, 90.0, 1.0]);
    }

    #[test]
    fn the_azimuth_signs_are_opposite() {
        // ADM M+030 is the app's FL, which `SpeakerPos::direction` calls -30.
        assert_eq!(adm_azimuth_to_app(30.0), -30.0);
        assert_eq!(app_azimuth_to_adm(-30.0), 30.0);
        let fl = SpeakerPos::Fl.direction(&[Some(SpeakerPos::Fl), Some(SpeakerPos::Fr)]);
        assert!(close(
            polar_to_cart(app_azimuth_to_adm(fl.0), fl.1, 1.0),
            speaker_cart(SpeakerPos::Fl).unwrap(),
            1e-5
        ));
    }
}
