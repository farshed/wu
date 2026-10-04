use super::*;
use windows::{
    Win32::{Foundation::HMODULE, UI::WindowsAndMessaging::*},
    core::w,
};

struct TestWindow(HWND);

impl Drop for TestWindow {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0).unwrap() };
    }
}

fn warp_devices() -> Result<DirectXDevices> {
    unsafe {
        let mut device = None;
        let mut context = None;
        let debug_result = D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_WARP,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_DEBUG,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        );
        if let Err(error) = debug_result {
            if error.code() != DXGI_ERROR_SDK_COMPONENT_MISSING {
                return Err(error.into());
            }
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device = device.unwrap();
        let dxgi: IDXGIDevice = device.cast()?;
        Ok(DirectXDevices {
            adapter: dxgi.GetAdapter()?.cast()?,
            dxgi_factory: CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?,
            device,
            device_context: context.unwrap(),
        })
    }
}

fn warp_renderer() -> Result<(DirectXRenderer, TestWindow)> {
    unsafe {
        let devices = warp_devices()?;
        let window = TestWindow(CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("GPUI WARP regression"),
            WS_OVERLAPPED,
            0,
            0,
            64,
            64,
            None,
            None,
            None,
            None,
        )?);
        let mut renderer = DirectXRenderer::new(window.0, &devices, true)?;
        renderer.resize(size(DevicePixels(64), DevicePixels(64)))?;
        Ok((renderer, window))
    }
}

fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
    Bounds::new(
        point(ScaledPixels(x), ScaledPixels(y)),
        size(ScaledPixels(width), ScaledPixels(height)),
    )
}

fn quad(order: u32, rect: Bounds<ScaledPixels>, color: Hsla) -> Quad {
    Quad {
        order,
        bounds: rect,
        content_mask: ContentMask {
            bounds: bounds(0., 0., 64., 64.),
        },
        background: color.into(),
        ..Default::default()
    }
}

fn blur(order: u32) -> BackdropBlur {
    BackdropBlur {
        order,
        blur_radius: ScaledPixels(3.),
        bounds: bounds(8., 8., 48., 48.),
        content_mask: ContentMask {
            bounds: bounds(0., 0., 64., 64.),
        },
        corner_radii: Corners::all(ScaledPixels(8.)),
    }
}

fn pixels(renderer: &DirectXRenderer) -> Result<Vec<[u8; 4]>> {
    let devices = renderer.devices.as_ref().unwrap();
    let target = renderer
        .resources
        .as_ref()
        .unwrap()
        .render_target
        .as_ref()
        .unwrap();
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        target.GetDesc(&mut desc);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        let mut staging = None;
        devices
            .device
            .CreateTexture2D(&desc, None, Some(&mut staging))?;
        let staging = staging.unwrap();
        devices.device_context.OMSetRenderTargets(None, None);
        devices.device_context.CopyResource(&staging, target);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        devices
            .device_context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let mut result = Vec::new();
        for y in 0..desc.Height {
            let row = slice::from_raw_parts(
                (mapped.pData as *const u8).add((y * mapped.RowPitch) as usize) as *const [u8; 4],
                desc.Width as usize,
            );
            result.extend_from_slice(row);
        }
        devices.device_context.Unmap(&staging, 0);
        assert_no_gpu_errors(&devices.device)?;
        Ok(result)
    }
}

fn assert_no_gpu_errors(device: &ID3D11Device) -> Result<()> {
    let Ok(queue) = device.cast::<ID3D11InfoQueue>() else {
        return Ok(());
    };
    unsafe {
        for index in 0..queue.GetNumStoredMessagesAllowedByRetrievalFilter() {
            let mut length = 0;
            queue.GetMessage(index, None, &mut length)?;
            // usize storage guarantees the alignment required by D3D11_MESSAGE.
            let mut buffer = vec![0usize; length.div_ceil(std::mem::size_of::<usize>())];
            let message = buffer.as_mut_ptr().cast::<D3D11_MESSAGE>();
            queue.GetMessage(index, Some(message), &mut length)?;
            let message = &*message;
            assert!(
                !matches!(
                    message.Severity,
                    D3D11_MESSAGE_SEVERITY_CORRUPTION
                        | D3D11_MESSAGE_SEVERITY_ERROR
                        | D3D11_MESSAGE_SEVERITY_WARNING
                ),
                "D3D11 validation: {}",
                std::ffi::CStr::from_ptr(message.pDescription.cast()).to_string_lossy()
            );
        }
        queue.ClearStoredMessages();
    }
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_backdrop_blurs_pixels() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    let mut scene = Scene::default();
    scene
        .quads
        .push(quad(0, bounds(0., 0., 32., 64.), hsla(0., 0., 0., 1.)));
    scene
        .quads
        .push(quad(0, bounds(32., 0., 32., 64.), hsla(0., 0., 1., 1.)));
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    let sharp = pixels(&renderer)?;
    scene.backdrop_blurs.push(blur(1));
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    let blurred = pixels(&renderer)?;
    assert_eq!(sharp[32 * 64 + 31][0], 0);
    assert!(
        blurred[32 * 64 + 31][0] > 50,
        "backdrop should soften black/white boundary: {:?}",
        blurred[32 * 64 + 31]
    );
    assert!(blurred[32 * 64 + 32][0] < 205);
    assert_eq!(
        blurred[2 * 64 + 31],
        sharp[2 * 64 + 31],
        "outside blur remains untouched"
    );
    Ok(())
}

fn sample(image: &[[u8; 4]], width: usize, height: usize, x: f32, y: f32) -> [f32; 4] {
    let x = x.clamp(0., (width - 1) as f32);
    let y = y.clamp(0., (height - 1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    std::array::from_fn(|c| {
        let top =
            image[y0 * width + x0][c] as f32 * (1. - tx) + image[y0 * width + x1][c] as f32 * tx;
        let bottom =
            image[y1 * width + x0][c] as f32 * (1. - tx) + image[y1 * width + x1][c] as f32 * tx;
        top * (1. - ty) + bottom * ty
    })
}

fn cpu_blur(image: &[[u8; 4]], width: usize, height: usize, blur: BackdropBlur) -> Vec<[u8; 4]> {
    let sigma = blur.blur_radius.0.clamp(1., 64.);
    let stride = ((sigma / 8.) as usize).clamp(1, 4);
    let bw = width.div_ceil(stride);
    let bh = height.div_ceil(stride);
    let radius = (sigma * 3.).ceil() as i32;
    let weights: Vec<f32> = (-radius..=radius)
        .map(|k| (-(k * k) as f32 / (2. * sigma * sigma)).exp())
        .collect();
    let total: f32 = weights.iter().sum();
    let mut horizontal = Vec::with_capacity(bw * bh);
    let mut vertical = Vec::with_capacity(bw * bh);
    for y in 0..height {
        for x in 0..bw {
            let mut sum = [0.; 4];
            for (k, weight) in (-radius..=radius).zip(&weights) {
                let value = sample(
                    image,
                    width,
                    height,
                    (x as f32 + 0.5) * width as f32 / bw as f32 - 0.5 + k as f32,
                    y as f32,
                );
                for c in 0..4 {
                    sum[c] += value[c] * weight / total;
                }
            }
            horizontal.push(sum.map(|v| v.round() as u8));
        }
    }
    for y in 0..bh {
        for x in 0..bw {
            let mut sum = [0.; 4];
            for (k, weight) in (-radius..=radius).zip(&weights) {
                let value = sample(
                    &horizontal,
                    bw,
                    height,
                    x as f32,
                    (y as f32 + 0.5) * height as f32 / bh as f32 - 0.5 + k as f32,
                );
                for c in 0..4 {
                    sum[c] += value[c] * weight / total;
                }
            }
            vertical.push(sum.map(|v| v.round() as u8));
        }
    }
    let mut result = image.to_vec();
    let rect = blur.bounds;
    let clip = rect.intersect(&blur.content_mask.bounds);
    for y in 0..height {
        for x in 0..width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            if px < clip.origin.x.0
                || px > clip.right().0
                || py < clip.origin.y.0
                || py > clip.bottom().0
            {
                continue;
            }
            let left = px < rect.center().x.0;
            let top = py < rect.center().y.0;
            let radius = match (left, top) {
                (true, true) => blur.corner_radii.top_left.0,
                (false, true) => blur.corner_radii.top_right.0,
                (false, false) => blur.corner_radii.bottom_right.0,
                (true, false) => blur.corner_radii.bottom_left.0,
            };
            let cx = if left {
                rect.left().0 + radius
            } else {
                rect.right().0 - radius
            };
            let cy = if top {
                rect.top().0 + radius
            } else {
                rect.bottom().0 - radius
            };
            if (left && px < cx || !left && px > cx)
                && (top && py < cy || !top && py > cy)
                && (px - cx).hypot(py - cy) > radius
            {
                continue;
            }
            result[y * width + x] = sample(
                &vertical,
                bw,
                bh,
                px * bw as f32 / width as f32 - 0.5,
                py * bh as f32 / height as f32 - 0.5,
            )
            .map(|v| v.round() as u8);
        }
    }
    result
}

fn assert_pixels(actual: &[[u8; 4]], expected: &[[u8; 4]], tolerance: u8) {
    assert_eq!(actual.len(), expected.len());
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        for c in 0..4 {
            assert!(
                actual[c].abs_diff(expected[c]) <= tolerance,
                "pixel {index} channel {c}: actual {actual:?}, expected {expected:?}"
            );
        }
    }
}

fn checkerboard() -> Scene {
    let mut scene = Scene::default();
    for y in 0..16 {
        for x in 0..16 {
            scene.quads.push(quad(
                0,
                bounds((x * 4) as f32, (y * 4) as f32, 4., 4.),
                hsla(0., 0., ((x + y) % 2) as f32, 1.),
            ));
        }
    }
    scene
}

#[::core::prelude::v1::test]
fn warp_nested_backdrops_match_sequential_reference() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    let mut scene = checkerboard();
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    let original = pixels(&renderer)?;
    let parent = blur(1);
    let child = BackdropBlur {
        order: 3,
        bounds: bounds(16., 16., 32., 32.),
        content_mask: ContentMask {
            bounds: bounds(20., 18., 24., 28.),
        },
        corner_radii: Corners::all(ScaledPixels(9.)),
        ..blur(3)
    };
    let mut expected = cpu_blur(&original, 64, 64, parent);
    for y in 0..64 {
        for x in 30..34 {
            expected[y * 64 + x] = [0, 0, 255, 255];
        }
    }
    expected = cpu_blur(&expected, 64, 64, child);
    for y in 28..36 {
        for x in 28..36 {
            expected[y * 64 + x] = [0, 255, 0, 255];
        }
    }
    scene
        .quads
        .push(quad(2, bounds(30., 0., 4., 64.), hsla(0., 1., 0.5, 1.)));
    scene.quads.push(quad(
        3,
        bounds(28., 28., 8., 8.),
        hsla(1. / 3., 1., 0.5, 1.),
    ));
    scene.backdrop_blurs = vec![parent, child];
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    assert_pixels(&pixels(&renderer)?, &expected, 2);
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_backdrop_preserves_premultiplied_alpha() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    let mut scene = Scene::default();
    scene
        .quads
        .push(quad(0, bounds(0., 0., 32., 64.), hsla(0., 1., 0.5, 0.5)));
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
    let original = pixels(&renderer)?;
    let expected = cpu_blur(&original, 64, 64, blur(1));
    scene.backdrop_blurs = vec![blur(1)];
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
    let actual = pixels(&renderer)?;
    assert_pixels(&actual, &expected, 2);
    assert_eq!(
        actual[32 * 64 + 16][3],
        original[32 * 64 + 16][3],
        "replacement must not accumulate alpha"
    );
    for pixel in actual {
        assert!(
            pixel[2] <= pixel[3] + 1,
            "premultiplied red cannot exceed alpha"
        );
    }
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_backdrop_downsampling_and_large_kernel() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    renderer.resize(size(DevicePixels(65), DevicePixels(67)))?;
    let original: Vec<[u8; 4]> = (0..67)
        .flat_map(|y| {
            (0..65).map(move |x| {
                [
                    (x * 3) as u8,
                    (y * 3) as u8,
                    if x < 32 { 0 } else { 255 },
                    255,
                ]
            })
        })
        .collect();
    for sigma in [0., 2.5, 16., 24., 32., 64., 180., f32::MAX] {
        renderer.pre_draw(&[0.; 4])?;
        let context = &renderer.devices.as_ref().unwrap().device_context;
        unsafe {
            context.VSSetConstantBuffers(0, Some(&[None]));
            context.PSSetConstantBuffers(0, Some(&[None]));
            context.OMSetRenderTargets(None, None);
            context.UpdateSubresource(
                renderer
                    .resources
                    .as_ref()
                    .unwrap()
                    .render_target
                    .as_ref()
                    .unwrap(),
                0,
                None,
                original.as_ptr().cast(),
                65 * 4,
                0,
            );
        }
        let blur = BackdropBlur {
            blur_radius: ScaledPixels(sigma),
            bounds: bounds(0., 0., 65., 67.),
            content_mask: ContentMask {
                bounds: bounds(0., 0., 65., 67.),
            },
            corner_radii: Corners::default(),
            ..blur(0)
        };
        renderer.draw_backdrop_blur(&blur)?;
        assert_pixels(&pixels(&renderer)?, &cpu_blur(&original, 65, 67, blur), 2);
    }
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_backdrop_cache_lifecycle() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    let empty = Scene::default();
    renderer.draw_scene(&empty, WindowBackgroundAppearance::Opaque)?;
    assert!(renderer.resources.as_ref().unwrap().backdrop.is_none());
    let mut scene = checkerboard();
    scene.backdrop_blurs = vec![blur(1)];
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    let expected = pixels(&renderer)?;
    let snapshot = renderer
        .resources
        .as_ref()
        .unwrap()
        .backdrop
        .as_ref()
        .unwrap()
        .snapshot()
        .clone();
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    assert_eq!(
        &snapshot,
        renderer
            .resources
            .as_ref()
            .unwrap()
            .backdrop
            .as_ref()
            .unwrap()
            .snapshot(),
        "stable blur reuses GPU scratch"
    );
    assert_pixels(&pixels(&renderer)?, &expected, 0);
    drop(snapshot);
    for _ in 0..119 {
        renderer.draw_scene(&empty, WindowBackgroundAppearance::Opaque)?;
    }
    assert!(
        renderer
            .resources
            .as_ref()
            .unwrap()
            .backdrop
            .as_ref()
            .unwrap()
            .has_scratch()
    );
    renderer.draw_scene(&empty, WindowBackgroundAppearance::Opaque)?;
    assert!(
        renderer
            .resources
            .as_ref()
            .unwrap()
            .backdrop
            .as_ref()
            .is_some_and(|backdrop| !backdrop.has_scratch()),
        "idle scratch must be released while shaders stay cached"
    );
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    assert_pixels(&pixels(&renderer)?, &expected, 0);
    renderer.resize(size(DevicePixels(81), DevicePixels(73)))?;
    assert!(
        renderer
            .resources
            .as_ref()
            .unwrap()
            .backdrop
            .as_ref()
            .is_some_and(|backdrop| !backdrop.has_scratch()),
        "resize releases scratch while shaders stay cached"
    );
    renderer.resize(size(DevicePixels(64), DevicePixels(64)))?;
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    assert_pixels(&pixels(&renderer)?, &expected, 0);
    let new_devices = warp_devices()?;
    renderer.handle_device_lost(&new_devices)?;
    assert!(
        renderer.resources.as_ref().unwrap().backdrop.is_none(),
        "recovery releases all old-device blur state"
    );
    renderer.draw(&scene, WindowBackgroundAppearance::Opaque)?;
    assert!(renderer.resources.as_ref().unwrap().backdrop.is_none());
    renderer.mark_drawable();
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    assert_pixels(&pixels(&renderer)?, &expected, 0);
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_backdrop_clipped_and_blur_only_frames() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    let mut scene = Scene::default();
    scene.backdrop_blurs = vec![BackdropBlur {
        bounds: bounds(-100., -100., 8., 8.),
        ..blur(0)
    }];
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    assert!(
        renderer.resources.as_ref().unwrap().backdrop.is_none(),
        "offscreen blur allocates nothing"
    );
    for sigma in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        scene.backdrop_blurs = vec![BackdropBlur {
            blur_radius: ScaledPixels(sigma),
            ..blur(0)
        }];
        renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
        assert!(renderer.resources.as_ref().unwrap().backdrop.is_none());
    }
    scene.backdrop_blurs = (0..40).map(blur).collect();
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    assert!(pixels(&renderer)?.iter().all(|pixel| *pixel == [255; 4]));
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_backdrop_reuses_cropped_snapshot_at_window_edges() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    renderer.resize(size(DevicePixels(193), DevicePixels(131)))?;
    let original: Vec<[u8; 4]> = (0..131)
        .flat_map(|y| {
            (0..193).map(move |x| {
                [
                    (x % 256) as u8,
                    (y % 256) as u8,
                    if (x / 4 + y / 4) % 2 == 0 { 0 } else { 255 },
                    255,
                ]
            })
        })
        .collect();
    renderer.pre_draw(&[0.; 4])?;
    unsafe {
        let context = &renderer.devices.as_ref().unwrap().device_context;
        context.OMSetRenderTargets(None, None);
        context.UpdateSubresource(
            renderer
                .resources
                .as_ref()
                .unwrap()
                .render_target
                .as_ref()
                .unwrap(),
            0,
            None,
            original.as_ptr().cast(),
            193 * 4,
            0,
        );
    }
    let mut expected = original;
    for rect in [
        bounds(166., 108., 24., 20.),
        bounds(0., 0., 21., 18.),
        bounds(80., 60., 8., 9.),
    ] {
        let region = BackdropBlur {
            bounds: rect,
            content_mask: ContentMask {
                bounds: bounds(0., 0., 193., 131.),
            },
            corner_radii: Corners::all(ScaledPixels(4.)),
            ..blur(0)
        };
        renderer.draw_backdrop_blur(&region)?;
        expected = cpu_blur(&expected, 193, 131, region);
        assert_pixels(&pixels(&renderer)?, &expected, 2);
    }
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_composer_tint_preserves_translucent_backdrop() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    for shell_alpha in [0.8, 1.0] {
        for with_blur in [true, false] {
            let mut scene = Scene::default();
            scene.quads.push(quad(
                0,
                bounds(0., 0., 64., 64.),
                hsla(0., 0., 0.2, shell_alpha),
            ));
            if with_blur {
                scene.backdrop_blurs = vec![blur(1)];
            }
            scene
                .quads
                .push(quad(1, bounds(8., 8., 48., 48.), hsla(0., 0., 0.1, 0.15)));
            renderer.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
            let actual = pixels(&renderer)?;
            let pixel = actual[32 * 64 + 32];
            let expected_alpha = 0.15 + shell_alpha * 0.85;
            assert!(
                (pixel[3] as f32 / 255. - expected_alpha).abs() < 0.01,
                "shell={shell_alpha}, blur={with_blur}: alpha {:?}, expected {expected_alpha}",
                pixel
            );
            let expected_rgb = 0.1 * 0.15 + 0.2 * shell_alpha * 0.85 + 1. - expected_alpha;
            for channel in &pixel[..3] {
                let over_white = *channel as f32 / 255. + 1. - pixel[3] as f32 / 255.;
                assert!(
                    (over_white - expected_rgb).abs() < 0.02,
                    "composer must retain desktop contribution: {over_white} vs {expected_rgb}"
                );
            }
            assert!((actual[0][3] as f32 / 255. - shell_alpha).abs() < 0.01);
            if with_blur {
                let mut child = blur(2);
                child.bounds = bounds(20., 20., 24., 24.);
                scene.backdrop_blurs.push(child);
                scene
                    .quads
                    .push(quad(2, child.bounds, hsla(0., 0., 0.1, 0.15)));
                renderer.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
                let nested = pixels(&renderer)?[32 * 64 + 32];
                let nested_alpha = 0.15 + expected_alpha * 0.85;
                assert!(
                    (nested[3] as f32 / 255. - nested_alpha).abs() < 0.01,
                    "nested tint must retain backdrop alpha: {nested:?}, expected {nested_alpha}"
                );
            }
        }
    }
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_path_tint_preserves_translucent_backdrop() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    let mut scene = Scene::default();
    scene
        .quads
        .push(quad(0, bounds(0., 0., 64., 64.), hsla(0., 0., 0.2, 0.8)));
    scene.backdrop_blurs = vec![blur(1)];
    let mut path = gpui::Path::new(point(gpui::px(8.), gpui::px(8.)));
    path.line_to(point(gpui::px(56.), gpui::px(8.)));
    path.line_to(point(gpui::px(56.), gpui::px(56.)));
    let mut path = path.scale(1.);
    path.order = 1;
    path.color = hsla(0., 0., 0.1, 0.15).into();
    path.content_mask.bounds = bounds(0., 0., 64., 64.);
    for vertex in &mut path.vertices {
        vertex.content_mask = path.content_mask;
    }
    scene.paths.push(path);
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
    let actual = pixels(&renderer)?;
    let pixel = actual[24 * 64 + 40];
    assert!(
        (pixel[3] as f32 / 255. - 0.83).abs() < 0.01,
        "path tint must retain backdrop alpha: {pixel:?}"
    );
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_fractional_blur_clip_matches_primitive_clip() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let mut scene = Scene::default();
        for x in 0..64 {
            scene.quads.push(quad(
                0,
                bounds(x as f32, 0., 1., 64.),
                hsla(0., 0., if x % 2 == 0 { 0. } else { 1. }, 0.8),
            ));
        }
        renderer.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
        let original = pixels(&renderer)?;
        let mut region = blur(1);
        region.corner_radii = Corners::default();
        scene.backdrop_blurs = vec![region];
        renderer.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
        let filtered = pixels(&renderer)?;

        let clip = bounds(10.5 * scale, 11.5 * scale, 12. * scale, 13. * scale);
        let mut mask = quad(0, bounds(0., 0., 64., 64.), hsla(0., 0., 1., 1.));
        mask.content_mask.bounds = clip;
        let mut mask_scene = Scene::default();
        mask_scene.quads.push(mask);
        renderer.draw_scene(&mask_scene, WindowBackgroundAppearance::Transparent)?;
        let mask_pixels = pixels(&renderer)?;
        region.content_mask.bounds = clip;
        scene.backdrop_blurs = vec![region];
        renderer.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
        let actual = pixels(&renderer)?;
        let expected: Vec<_> = original
            .iter()
            .zip(&filtered)
            .zip(&mask_pixels)
            .map(|((before, after), mask)| if mask[3] > 0 { *after } else { *before })
            .collect();
        assert_pixels(&actual, &expected, 2);
    }
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_translucent_sprites_keep_backdrop_at_scaled_edges() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    let tile_size = size(DevicePixels(8), DevicePixels(8));
    for monochrome in [true, false] {
        let key = if monochrome {
            AtlasKey::Svg(RenderSvgParams {
                path: "coverage-fixture".into(),
                size: tile_size,
            })
        } else {
            AtlasKey::Image(RenderImageParams {
                image_id: ImageId(1001),
                frame_index: 0,
            })
        };
        let coverage = [0, 16, 64, 128, 192, 255, 128, 0];
        let bytes: Vec<u8> = (0..64)
            .flat_map(|i| {
                if monochrome {
                    vec![coverage[i % 8]]
                } else {
                    vec![40, 90, 180, coverage[i % 8]]
                }
            })
            .collect();
        let tile = renderer
            .atlas
            .get_or_insert_with(&key, &mut || {
                Ok(Some((tile_size, std::borrow::Cow::Borrowed(&bytes))))
            })?
            .unwrap();
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let sprite_bounds = bounds(16.25, 16.5, 8. * scale, 8. * scale);
            let content_mask = ContentMask {
                bounds: bounds(0., 0., 64., 64.),
            };
            let mut scene = Scene::default();
            if monochrome {
                scene.monochrome_sprites.push(MonochromeSprite {
                    order: 1,
                    pad: 0,
                    bounds: sprite_bounds,
                    content_mask,
                    color: hsla(0.1, 0.6, 0.8, 0.6),
                    fade: EdgeFadeParams::default(),
                    tile,
                    transformation: TransformationMatrix::default(),
                });
            } else {
                scene.polychrome_sprites.push(PolychromeSprite {
                    order: 1,
                    pad: 0,
                    grayscale: false.into(),
                    opacity: 0.6,
                    bounds: sprite_bounds,
                    content_mask,
                    corner_radii: Corners::all(ScaledPixels(2. * scale)),
                    fade: EdgeFadeParams::default(),
                    alpha_mask: ImageAlphaMaskParams::default(),
                    tile,
                });
            }
            renderer.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
            let foreground = pixels(&renderer)?;
            assert!(
                foreground
                    .iter()
                    .any(|pixel| pixel[3] > 0 && pixel[3] < 150),
                "fixture must exercise partial glyph/image coverage"
            );
            scene
                .quads
                .push(quad(0, bounds(0., 0., 64., 64.), hsla(0., 0., 0.3, 0.8)));
            renderer.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
            let actual = pixels(&renderer)?;
            for (src, actual) in foreground.iter().zip(actual) {
                let a = src[3] as f32 / 255.;
                let expected = [
                    src[0] as f32 + 255. * 0.3 * 0.8 * (1. - a),
                    src[1] as f32 + 255. * 0.3 * 0.8 * (1. - a),
                    src[2] as f32 + 255. * 0.3 * 0.8 * (1. - a),
                    255. * (a + 0.8 * (1. - a)),
                ];
                for channel in 0..4 {
                    assert!(
                        (actual[channel] as f32 - expected[channel]).abs() <= 2.,
                        "mono={monochrome} scale={scale} channel={channel}: {actual:?} vs {expected:?}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_animated_nested_blur_matches_fresh_resources() -> Result<()> {
    let (mut cached, _cached_window) = warp_renderer()?;
    let (mut fresh, _fresh_window) = warp_renderer()?;
    for (width, height) in [(193, 131), (137, 99), (211, 143)] {
        cached.resize(size(DevicePixels(width), DevicePixels(height)))?;
        fresh.resize(size(DevicePixels(width), DevicePixels(height)))?;
        for frame in 0..12 {
            let viewport = bounds(0., 0., width as f32, height as f32);
            let mut scene = Scene::default();
            for x in 0..width {
                let mut stripe = quad(
                    0,
                    bounds(x as f32, 0., 1., height as f32),
                    hsla(0.1, 0.5, if (x + frame) % 7 < 3 { 0.2 } else { 0.8 }, 0.7),
                );
                stripe.content_mask.bounds = viewport;
                scene.quads.push(stripe);
            }
            let offset = frame as f32 * 7.25;
            let parent = BackdropBlur {
                bounds: bounds(offset - 12., 4.25, 100., 80.),
                content_mask: ContentMask { bounds: viewport },
                blur_radius: ScaledPixels(if frame % 3 == 0 { 16. } else { 3. }),
                ..blur(1)
            };
            let mut tint = quad(2, parent.bounds, hsla(0., 0., 0.1, 0.15));
            tint.content_mask.bounds = viewport;
            scene.quads.push(tint);
            scene.backdrop_blurs = vec![
                parent,
                BackdropBlur {
                    bounds: bounds(offset + 9.5, 21.25, 41.5, 39.5),
                    content_mask: ContentMask {
                        bounds: parent.bounds.intersect(&viewport),
                    },
                    ..blur(3)
                },
            ];
            cached.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
            let actual = pixels(&cached)?;
            fresh.resources.as_mut().unwrap().backdrop = None;
            fresh.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
            assert_pixels(&actual, &pixels(&fresh)?, 2);
        }
    }
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_large_blur_removes_fine_stripes() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    renderer.resize(size(DevicePixels(256), DevicePixels(256)))?;
    for phase in 0..4 {
        for horizontal in [false, true] {
            let mut scene = Scene::default();
            for n in 0..256 {
                let rect = if horizontal {
                    bounds(0., n as f32, 256., 1.)
                } else {
                    bounds(n as f32, 0., 1., 256.)
                };
                let mut stripe = quad(
                    0,
                    rect,
                    hsla(0., 0., if (n + phase) % 4 < 2 { 0. } else { 1. }, 1.),
                );
                stripe.content_mask.bounds = bounds(0., 0., 256., 256.);
                scene.quads.push(stripe);
            }
            scene.backdrop_blurs.push(BackdropBlur {
                bounds: bounds(0., 0., 256., 256.),
                content_mask: ContentMask {
                    bounds: bounds(0., 0., 256., 256.),
                },
                blur_radius: ScaledPixels(32.),
                corner_radii: Corners::default(),
                ..blur(1)
            });
            renderer.draw_scene(&scene, WindowBackgroundAppearance::Transparent)?;
            let image = pixels(&renderer)?;

            assert!(
                (image[128 * 256 + 128][0] as i32 - 128).abs() <= 3,
                "fine stripes should average to gray: orientation={horizontal}, center={:?}",
                image[128 * 256 + 128]
            );
        }
    }
    Ok(())
}

#[::core::prelude::v1::test]
fn warp_invalid_geometry_allocates_nothing_and_idle_cache_expires() -> Result<()> {
    let (mut renderer, _window) = warp_renderer()?;
    let mut scene = Scene::default();
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.] {
        for field in 0..4 {
            let mut region = blur(0);
            match field {
                0 => region.bounds.size.width = ScaledPixels(value),
                1 => region.content_mask.bounds.size.height = ScaledPixels(value),
                2 => region.corner_radii.top_left = ScaledPixels(value),
                _ => {
                    region.bounds.origin.x =
                        ScaledPixels(if value == -1. { f32::INFINITY } else { value })
                }
            }
            scene.backdrop_blurs = vec![region];
            renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
            assert!(renderer.resources.as_ref().unwrap().backdrop.is_none());
            assert!(pixels(&renderer)?.iter().all(|pixel| *pixel == [255; 4]));
        }
    }
    scene.backdrop_blurs = vec![BackdropBlur {
        bounds: bounds(f32::MAX, 0., f32::MAX, 10.),
        ..blur(0)
    }];
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    assert!(renderer.resources.as_ref().unwrap().backdrop.is_none());
    scene.backdrop_blurs = vec![blur(0)];
    renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    assert!(renderer.resources.as_ref().unwrap().backdrop.is_some());
    scene.backdrop_blurs[0].bounds = bounds(-100., -100., 8., 8.);
    for _ in 0..120 {
        renderer.draw_scene(&scene, WindowBackgroundAppearance::Opaque)?;
    }
    assert!(
        renderer
            .resources
            .as_ref()
            .unwrap()
            .backdrop
            .as_ref()
            .is_some_and(|backdrop| !backdrop.has_scratch())
    );
    Ok(())
}
