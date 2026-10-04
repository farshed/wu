mod blur_kernel;
mod cosmic_text_system;
mod wgpu_atlas;
mod wgpu_context;
mod wgpu_renderer;

pub use cosmic_text_system::*;
pub use wgpu;
pub use wgpu_atlas::*;
pub use wgpu_context::*;
pub use wgpu_renderer::{GpuContext, WgpuRenderer, WgpuSurfaceConfig};

#[cfg(test)]
mod shader_tests {
    #[test]
    fn image_alpha_mask_shader_validates_and_matches_host_layout() {
        use std::mem::{offset_of, size_of};
        let module = naga::front::wgsl::parse_str(concat!(
            include_str!("shaders.wgsl"),
            include_str!("shaders_storage.wgsl"),
        ))
        .unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        for (name, size, offsets) in [
            (
                "ImageAlphaMaskParams",
                size_of::<gpui::ImageAlphaMaskParams>(),
                vec![
                    ("bounds", offset_of!(gpui::ImageAlphaMaskParams, bounds)),
                    ("radius", offset_of!(gpui::ImageAlphaMaskParams, radius)),
                    ("feather", offset_of!(gpui::ImageAlphaMaskParams, feather)),
                    (
                        "clearance",
                        offset_of!(gpui::ImageAlphaMaskParams, clearance),
                    ),
                    ("bottom_y", offset_of!(gpui::ImageAlphaMaskParams, bottom_y)),
                    (
                        "bottom_feather",
                        offset_of!(gpui::ImageAlphaMaskParams, bottom_feather),
                    ),
                    ("pad", offset_of!(gpui::ImageAlphaMaskParams, pad)),
                ],
            ),
            (
                "PolychromeSprite",
                size_of::<gpui::PolychromeSprite>(),
                vec![
                    ("fade", offset_of!(gpui::PolychromeSprite, fade)),
                    ("alpha_mask", offset_of!(gpui::PolychromeSprite, alpha_mask)),
                    ("tile", offset_of!(gpui::PolychromeSprite, tile)),
                ],
            ),
        ] {
            let ty = module
                .types
                .iter()
                .find(|(_, ty)| ty.name.as_deref() == Some(name))
                .unwrap()
                .1;
            let naga::TypeInner::Struct { members, span } = &ty.inner else {
                panic!("expected struct")
            };
            assert_eq!(*span as usize, size, "{name} stride");
            for (field, offset) in offsets {
                assert_eq!(
                    members
                        .iter()
                        .find(|m| m.name.as_deref() == Some(field))
                        .unwrap()
                        .offset as usize,
                    offset,
                    "{name}.{field}"
                );
            }
        }
    }
    #[test]
    fn text_fade_shaders_validate_and_match_host_layout() {
        use std::mem::{offset_of, size_of};
        let source = format!(
            "enable dual_source_blending;\n{}\n{}\n{}",
            include_str!("shaders.wgsl"),
            include_str!("shaders_storage.wgsl"),
            include_str!("shaders_subpixel.wgsl"),
        );
        let module = naga::front::wgsl::parse_str(&source).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        for (name, size, fade_offset) in [
            (
                "MonochromeSprite",
                size_of::<gpui::MonochromeSprite>(),
                offset_of!(gpui::MonochromeSprite, fade),
            ),
            (
                "SubpixelSprite",
                size_of::<gpui::SubpixelSprite>(),
                offset_of!(gpui::SubpixelSprite, fade),
            ),
        ] {
            let ty = module
                .types
                .iter()
                .find(|(_, ty)| ty.name.as_deref() == Some(name))
                .unwrap()
                .1;
            let naga::TypeInner::Struct { members, span } = &ty.inner else {
                panic!("expected struct")
            };
            assert_eq!(*span as usize, size, "{name} stride");
            assert_eq!(
                members
                    .iter()
                    .find(|m| m.name.as_deref() == Some("fade"))
                    .unwrap()
                    .offset as usize,
                fade_offset,
                "{name}.fade"
            );
        }
    }
}
