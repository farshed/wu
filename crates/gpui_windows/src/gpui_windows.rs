#![cfg(target_os = "windows")]

mod clipboard;
mod destination_list;
mod direct_manipulation;
mod direct_write;
mod directx_atlas;
mod directx_devices;
mod directx_renderer;
mod dispatcher;
mod display;
mod events;
mod keyboard;
mod platform;
mod system_notifications;
mod system_settings;
mod util;
mod vsync;
mod window;
mod wrapper;

pub(crate) use clipboard::*;
pub(crate) use destination_list::*;
pub(crate) use direct_write::*;
pub(crate) use directx_atlas::*;
pub(crate) use directx_devices::*;
pub(crate) use directx_renderer::*;
pub(crate) use dispatcher::*;
pub(crate) use display::*;
pub(crate) use events::*;
pub(crate) use keyboard::*;
pub(crate) use platform::*;
pub(crate) use system_notifications::*;
pub(crate) use system_settings::*;
pub(crate) use util::*;
pub(crate) use vsync::*;
pub(crate) use window::*;
pub(crate) use wrapper::*;

pub use platform::WindowsPlatform;

pub(crate) use windows::Win32::Foundation::HWND;

#[cfg(test)]
mod tests {
    use gpui::{AtlasTile, EdgeFadeParams, ImageAlphaMaskParams, PolychromeSprite, Quad};
    use std::mem::{align_of, offset_of, size_of};

    const SHADERS: &str = include_str!("shaders.hlsl");

    fn hlsl_struct(name: &str) -> &str {
        let declaration = format!("struct {name} {{");
        SHADERS
            .split_once(&declaration)
            .unwrap_or_else(|| panic!("missing HLSL {name} declaration"))
            .1
            .split_once("};")
            .unwrap_or_else(|| panic!("unterminated HLSL {name} declaration"))
            .0
    }

    fn assert_fields_in_order(body: &str, fields: &[&str]) {
        let mut remainder = body;
        for field in fields {
            remainder = remainder
                .split_once(field)
                .unwrap_or_else(|| {
                    panic!("missing or out-of-order HLSL field `{field}` in:\n{body}")
                })
                .1;
        }
    }

    #[test]
    fn edge_fade_layout_is_eight_packed_floats() {
        assert_eq!(align_of::<EdgeFadeParams>(), align_of::<f32>());
        assert_eq!(size_of::<EdgeFadeParams>(), 32);
        assert_fields_in_order(
            hlsl_struct("EdgeFadeParams"),
            &[
                "float top_y;",
                "float bottom_y;",
                "float band_top;",
                "float band_bottom;",
                "float left_x;",
                "float right_x;",
                "float band_left;",
                "float band_right;",
            ],
        );
    }

    #[test]
    fn quad_hlsl_layout_includes_trailing_edge_fade() {
        assert_eq!(offset_of!(Quad, fade), 160);
        assert_eq!(size_of::<Quad>(), 192);
        assert_fields_in_order(
            hlsl_struct("Quad"),
            &[
                "uint order;",
                "uint border_style;",
                "Bounds bounds;",
                "Bounds content_mask;",
                "Background background;",
                "Hsla border_color;",
                "Corners corner_radii;",
                "Edges border_widths;",
                "EdgeFadeParams fade;",
            ],
        );
    }

    #[test]
    fn polychrome_hlsl_layout_keeps_fade_before_atlas_tile() {
        assert_eq!(offset_of!(PolychromeSprite, fade), 64);
        assert_eq!(offset_of!(PolychromeSprite, alpha_mask), 96);
        assert_eq!(offset_of!(PolychromeSprite, tile), 136);
        assert_eq!(size_of::<AtlasTile>(), 32);
        assert_eq!(size_of::<PolychromeSprite>(), 168);
        assert_fields_in_order(
            hlsl_struct("PolychromeSprite"),
            &[
                "uint order;",
                "uint pad;",
                "uint grayscale;",
                "float opacity;",
                "Bounds bounds;",
                "Bounds content_mask;",
                "Corners corner_radii;",
                "EdgeFadeParams fade;",
                "ImageAlphaMaskParams alpha_mask;",
                "AtlasTile tile;",
            ],
        );
    }

    #[test]
    fn image_alpha_mask_hlsl_layout_matches_host() {
        assert_eq!(size_of::<ImageAlphaMaskParams>(), 40);
        assert_eq!(offset_of!(ImageAlphaMaskParams, radius), 16);
        assert_eq!(offset_of!(ImageAlphaMaskParams, bottom_feather), 32);
        assert_fields_in_order(
            hlsl_struct("ImageAlphaMaskParams"),
            &[
                "Bounds bounds;",
                "float radius;",
                "float feather;",
                "float clearance;",
                "float bottom_y;",
                "float bottom_feather;",
                "float pad;",
            ],
        );
    }
}
