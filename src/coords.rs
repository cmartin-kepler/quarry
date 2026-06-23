//! The two coordinate maps where bugs hide (build-plan §3 — the pinned bug-nests).
//!
//! Both layout (YOLO) and escalation (docling-on-crop) hand back boxes in a
//! coordinate system that is NOT the page's PDF-point system litparse spans use. A
//! wrong conversion here silently offsets every downstream anchor — so these are
//! tiny, pure, and unit-tested, ready for the moment the sidecars run.
//!
//! Convention: [`crate::core::BBox`] is in PDF points, origin top-left.

use crate::core::BBox;

/// Coordinate map #1 — **YOLO pixels → PDF points.** YOLO sees the page rendered
/// at `scale` pixels-per-point (`px = pt · scale`), top-left origin. Invert to get
/// points: `pt = px / scale`.
pub fn pixels_to_points(px: BBox, scale: f32) -> BBox {
    debug_assert!(scale > 0.0, "render scale must be positive");
    BBox::new(px.x0 / scale, px.y0 / scale, px.x1 / scale, px.y1 / scale)
}

/// Inverse of [`pixels_to_points`] — **PDF points → render pixels.** Used to turn a
/// table's point bbox into the pixel crop region for the image-crop fallback.
pub fn points_to_pixels(pt: BBox, scale: f32) -> BBox {
    debug_assert!(scale > 0.0, "render scale must be positive");
    BBox::new(pt.x0 * scale, pt.y0 * scale, pt.x1 * scale, pt.y1 * scale)
}

/// Coordinate map #2 — **docling-on-crop → page points.** Docling parsing a crop
/// whose origin sits at page point `(crop_x0, crop_y0)` returns cell boxes relative
/// to that crop origin; translate back: `page = crop + (crop_x0, crop_y0)`.
///
/// Ordering note: apply docling's own bottom-left→top-left flip FIRST (the existing
/// `docling` adapter), THEN this translate — both operate in top-left points.
pub fn crop_to_page(crop_box: BBox, crop_x0: f32, crop_y0: f32) -> BBox {
    BBox::new(
        crop_box.x0 + crop_x0,
        crop_box.y0 + crop_y0,
        crop_box.x1 + crop_x0,
        crop_box.y1 + crop_y0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_to_points_divides_by_scale() {
        // a 200px-wide box rendered at 2x is 100pt wide
        let px = BBox::new(0.0, 0.0, 200.0, 100.0);
        let pt = pixels_to_points(px, 2.0);
        assert_eq!((pt.x0, pt.y0, pt.x1, pt.y1), (0.0, 0.0, 100.0, 50.0));
    }

    #[test]
    fn pixel_point_round_trips() {
        let pt = BBox::new(72.0, 144.0, 300.0, 400.0);
        let back = pixels_to_points(points_to_pixels(pt, 2.5), 2.5);
        for (a, b) in [(back.x0, pt.x0), (back.y0, pt.y0), (back.x1, pt.x1), (back.y1, pt.y1)] {
            assert!((a - b).abs() < 1e-3, "{a} != {b}");
        }
    }

    #[test]
    fn crop_to_page_adds_the_crop_origin() {
        // a cell at (5,5)-(15,25) within a crop whose top-left is page (100, 200)
        let cell = BBox::new(5.0, 5.0, 15.0, 25.0);
        let page = crop_to_page(cell, 100.0, 200.0);
        assert_eq!((page.x0, page.y0, page.x1, page.y1), (105.0, 205.0, 115.0, 225.0));
    }

    #[test]
    fn crop_to_page_is_identity_at_the_page_origin() {
        let cell = BBox::new(5.0, 5.0, 15.0, 25.0);
        assert_eq!(crop_to_page(cell, 0.0, 0.0), cell);
    }

    #[test]
    fn full_chain_pixel_crop_then_back_to_page() {
        // YOLO finds a table at page points (50,60)-(250,160). We render the crop at
        // 2x → pixel box (100,120)-(500,320). docling returns a cell at crop-pixel
        // (40,20)-(80,40) → /2 = (20,10)-(40,20) points within the crop, whose origin
        // is the table's top-left (50,60) → page (70,70)-(90,80).
        let table_pt = BBox::new(50.0, 60.0, 250.0, 160.0);
        let crop_px = points_to_pixels(table_pt, 2.0);
        assert_eq!((crop_px.x0, crop_px.y0), (100.0, 120.0));
        let cell_pt = pixels_to_points(BBox::new(40.0, 20.0, 80.0, 40.0), 2.0);
        let page = crop_to_page(cell_pt, table_pt.x0, table_pt.y0);
        assert_eq!((page.x0, page.y0, page.x1, page.y1), (70.0, 70.0, 90.0, 80.0));
    }
}
