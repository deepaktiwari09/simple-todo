//! Is anything underneath the window? With nothing behind it but the desktop
//! there is no point in a see-through background, so the UI goes opaque.

use tauri::WebviewWindow;

/// Other apps' windows must cover more than this fraction of ours to count as
/// "underneath"; a little overlap at the edges is ignored.
pub const COVERED_THRESHOLD: f64 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Fraction (0…1) of `target` covered by the union of `others`.
pub fn covered_fraction(target: Rect, others: &[Rect]) -> f64 {
    let area = target.w * target.h;
    if area <= 0.0 {
        return 0.0;
    }
    // Clip to the target as (x0, y0, x1, y1), dropping anything outside it.
    let clipped: Vec<(f64, f64, f64, f64)> = others
        .iter()
        .filter_map(|r| {
            let x0 = r.x.max(target.x);
            let x1 = (r.x + r.w).min(target.x + target.w);
            let y0 = r.y.max(target.y);
            let y1 = (r.y + r.h).min(target.y + target.h);
            (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
        })
        .collect();
    // Sweep the vertical strips between rectangle edges, merging the
    // y-intervals that span each strip, so overlaps are not counted twice.
    let mut xs: Vec<f64> = clipped.iter().flat_map(|r| [r.0, r.2]).collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    let mut covered = 0.0;
    for strip in xs.windows(2) {
        let (a, b) = (strip[0], strip[1]);
        let mut spans: Vec<(f64, f64)> = clipped
            .iter()
            .filter(|r| r.0 <= a && r.2 >= b)
            .map(|r| (r.1, r.3))
            .collect();
        spans.sort_by(|p, q| p.0.total_cmp(&q.0));
        let mut len = 0.0;
        let mut cur: Option<(f64, f64)> = None;
        for (s, e) in spans {
            cur = match cur {
                Some((cs, ce)) if s <= ce => Some((cs, ce.max(e))),
                Some((cs, ce)) => {
                    len += ce - cs;
                    Some((s, e))
                }
                None => Some((s, e)),
            };
        }
        if let Some((cs, ce)) = cur {
            len += ce - cs;
        }
        covered += len * (b - a);
    }
    (covered / area).clamp(0.0, 1.0)
}

/// Whether other apps' windows sit under enough of `w` to show through it.
/// `None` when it can't be determined (other platforms, window not on screen).
pub fn content_below(w: &WebviewWindow) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        let (own, below) = mac::window_and_below(w)?;
        Some(covered_fraction(own, &below) > COVERED_THRESHOLD)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = w;
        None
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::Rect;
    use std::ffi::c_void;
    use tauri::WebviewWindow;

    type CFTypeRef = *const c_void;

    #[repr(C)]
    #[derive(Default)]
    struct CGRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    const ON_SCREEN_BELOW_WINDOW: u32 = 1 << 2;
    const INCLUDING_WINDOW: u32 = 1 << 3;
    const EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;
    const CF_NUMBER_SINT32: isize = 3;
    const CF_NUMBER_FLOAT64: isize = 6;
    /// Normal (0) through modal-panel (8) levels; the Dock, menu bar and
    /// desktop live outside this range.
    const APP_LAYERS: std::ops::RangeInclusive<i32> = 0..=8;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        static kCGWindowBounds: CFTypeRef;
        static kCGWindowLayer: CFTypeRef;
        static kCGWindowOwnerPID: CFTypeRef;
        static kCGWindowAlpha: CFTypeRef;
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
        fn CGRectMakeWithDictionaryRepresentation(dict: CFTypeRef, rect: *mut CGRect) -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFArrayGetCount(array: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, i: isize) -> CFTypeRef;
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFNumberGetValue(n: CFTypeRef, ty: isize, out: *mut c_void) -> bool;
        fn CFRelease(cf: CFTypeRef);
    }

    struct Info {
        bounds: Rect,
        layer: i32,
        pid: i32,
        alpha: f64,
    }

    /// The window-server info for `relative_to` under `option`, front to back.
    /// Only geometry is read, which needs no Screen Recording permission.
    fn window_list(option: u32, relative_to: u32) -> Option<Vec<Info>> {
        unsafe {
            let list = CGWindowListCopyWindowInfo(option, relative_to);
            if list.is_null() {
                return None;
            }
            let num = |d: CFTypeRef, key: CFTypeRef, ty: isize, out: *mut c_void| {
                let v = CFDictionaryGetValue(d, key);
                !v.is_null() && CFNumberGetValue(v, ty, out)
            };
            let mut out = Vec::new();
            for i in 0..CFArrayGetCount(list) {
                let d = CFArrayGetValueAtIndex(list, i);
                let b = CFDictionaryGetValue(d, kCGWindowBounds);
                let mut r = CGRect::default();
                if b.is_null() || !CGRectMakeWithDictionaryRepresentation(b, &mut r) {
                    continue;
                }
                let (mut layer, mut pid, mut alpha) = (0i32, 0i32, 1.0f64);
                num(
                    d,
                    kCGWindowLayer,
                    CF_NUMBER_SINT32,
                    &mut layer as *mut _ as _,
                );
                num(
                    d,
                    kCGWindowOwnerPID,
                    CF_NUMBER_SINT32,
                    &mut pid as *mut _ as _,
                );
                num(
                    d,
                    kCGWindowAlpha,
                    CF_NUMBER_FLOAT64,
                    &mut alpha as *mut _ as _,
                );
                out.push(Info {
                    bounds: Rect {
                        x: r.x,
                        y: r.y,
                        w: r.w,
                        h: r.h,
                    },
                    layer,
                    pid,
                    alpha,
                });
            }
            CFRelease(list);
            Some(out)
        }
    }

    /// Our window's bounds and those of other apps' visible windows below it,
    /// all in the window server's global coordinates.
    pub fn window_and_below(w: &WebviewWindow) -> Option<(Rect, Vec<Rect>)> {
        let ns_window = w.ns_window().ok()? as *const objc2::runtime::AnyObject;
        if ns_window.is_null() {
            return None;
        }
        let number: isize = unsafe { objc2::msg_send![&*ns_window, windowNumber] };
        let number = u32::try_from(number).ok().filter(|n| *n > 0)?;
        let own = window_list(INCLUDING_WINDOW, number)?
            .into_iter()
            .next()?
            .bounds;
        let me = std::process::id() as i32;
        let below = window_list(ON_SCREEN_BELOW_WINDOW | EXCLUDE_DESKTOP_ELEMENTS, number)?
            .into_iter()
            .filter(|i| i.pid != me && APP_LAYERS.contains(&i.layer) && i.alpha > 0.0)
            .map(|i| i.bounds)
            .collect();
        Some((own, below))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: Rect = Rect {
        x: 100.0,
        y: 100.0,
        w: 400.0,
        h: 600.0,
    };
    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn nothing_below_is_zero() {
        assert_eq!(covered_fraction(WIN, &[]), 0.0);
        assert_eq!(covered_fraction(WIN, &[r(600.0, 0.0, 100.0, 100.0)]), 0.0);
    }

    #[test]
    fn full_cover_is_one_even_when_larger() {
        assert_eq!(covered_fraction(WIN, &[r(0.0, 0.0, 2000.0, 2000.0)]), 1.0);
    }

    #[test]
    fn overlaps_are_not_double_counted() {
        // Left half twice, plus a quarter that overlaps it: still half covered.
        let half = r(100.0, 100.0, 200.0, 600.0);
        let quarter = r(100.0, 100.0, 200.0, 300.0);
        assert!((covered_fraction(WIN, &[half, half, quarter]) - 0.5).abs() < 1e-9);
        // Two disjoint quarters plus one straddling them.
        let a = r(100.0, 100.0, 100.0, 600.0);
        let b = r(400.0, 100.0, 100.0, 600.0);
        let mid = r(150.0, 100.0, 300.0, 600.0);
        assert!((covered_fraction(WIN, &[a, b]) - 0.5).abs() < 1e-9);
        assert!((covered_fraction(WIN, &[a, b, mid]) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn minor_edge_overlap_stays_under_the_threshold() {
        // A neighbour poking 40px into a 400px-wide window: 10%.
        let f = covered_fraction(WIN, &[r(0.0, 0.0, 140.0, 2000.0)]);
        assert!((f - 0.1).abs() < 1e-9);
        assert!(f <= COVERED_THRESHOLD);
        let f = covered_fraction(WIN, &[r(0.0, 0.0, 300.0, 2000.0)]);
        assert!(f > COVERED_THRESHOLD);
    }
}
