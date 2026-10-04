use super::*;
use gpui::{ContentMask, hsla, point, size};
use wgpu::util::DeviceExt;

fn rectangle(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
    Bounds {
        origin: point(ScaledPixels(x), ScaledPixels(y)),
        size: size(ScaledPixels(width), ScaledPixels(height)),
    }
}

fn quad() -> Quad {
    Quad {
        bounds: rectangle(0.0, 0.0, 256.0, 256.0),
        content_mask: ContentMask {
            bounds: rectangle(0.0, 0.0, 256.0, 256.0),
        },
        background: hsla(0.13, 0.6, 0.4, 0.7).into(),
        ..Quad::default()
    }
}

#[test]
fn simple_fill_selection_preserves_effects() {
    assert!(is_simple_solid_quad(&quad()));
    let mut q = quad();
    q.corner_radii.top_left = ScaledPixels(0.01);
    assert!(!is_simple_solid_quad(&q));
    q = quad();
    q.border_widths.bottom = ScaledPixels(0.01);
    assert!(!is_simple_solid_quad(&q));
    for edge in 0..4 {
        q = quad();
        match edge {
            0 => q.fade.band_top = 0.01,
            1 => q.fade.band_bottom = 0.01,
            2 => q.fade.band_left = 0.01,
            _ => q.fade.band_right = 0.01,
        }
        assert!(!is_simple_solid_quad(&q));
    }
    q = quad();
    q.background = gpui::linear_gradient(
        90.0,
        gpui::linear_color_stop(hsla(0.0, 1.0, 0.5, 1.0), 0.0),
        gpui::linear_color_stop(hsla(0.5, 1.0, 0.5, 0.0), 1.0),
    );
    assert!(!is_simple_solid_quad(&q));
}

#[test]
#[ignore = "requires a GPU adapter"]
fn simple_fill_matches_general_shader_pixels() {
    compare_shader_pixels(false);
}

#[test]
#[ignore = "requires a GPU adapter"]
fn large_interiors_match_general_shader_pixels() {
    compare_shader_pixels(true);
}

fn compare_shader_pixels(include_interiors: bool) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: wgpu::InstanceFlags::default(),
        backend_options: wgpu::BackendOptions::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let layouts = WgpuRenderer::create_bind_group_layouts(&device, false);
    let mut quads = vec![quad()];
    for i in 0..192 {
        let mut q = quad();
        q.bounds = rectangle(
            (i * 37 % 280) as f32 - 20.25,
            (i * 71 % 280) as f32 - 20.75,
            (i % 47 + 1) as f32 + 0.5,
            (i % 61 + 1) as f32 + 0.25,
        );
        q.content_mask.bounds = rectangle(
            (i % 17) as f32 + 0.125,
            (i % 23) as f32 + 0.625,
            if i % 11 == 0 { 0.0 } else { 217.5 },
            224.25,
        );
        q.background = hsla(
            (i % 31) as f32 / 31.0,
            (i % 7) as f32 / 6.0,
            (i % 9) as f32 / 8.0,
            (i % 5) as f32 / 4.0,
        )
        .into();
        assert!(is_simple_solid_quad(&q));
        quads.push(q);
    }
    for epsilon in [-0.001, -0.0001, 0.0, 0.0001, 0.001] {
        for padding in [0.0, 1.0] {
            let mut q = quad();
            q.bounds = rectangle(
                10.5 + epsilon,
                15.5 - epsilon,
                127.0 + epsilon,
                133.0 - epsilon,
            );
            q.background = hsla(0.3, 0.7, 0.4, 1.0).into();
            q.content_mask.bounds = rectangle(
                q.bounds.origin.x.0 - padding,
                q.bounds.origin.y.0 - padding,
                q.bounds.size.width.0 + 2.0 * padding,
                q.bounds.size.height.0 + 2.0 * padding,
            );
            assert_eq!(is_unclipped_opaque_quad(&q), padding == 1.0);
            quads.push(q);
        }
    }
    if include_interiors {
        for i in 0..64 {
            let mut q = quad();
            q.bounds = rectangle(
                -20.25 + (i % 3) as f32 * 17.0,
                -19.75 + (i % 5) as f32 * 11.0,
                257.5,
                261.25,
            );
            q.corner_radii = gpui::Corners {
                top_left: ScaledPixels((i % 7) as f32 + 0.125),
                top_right: ScaledPixels((i % 11) as f32 + 0.75),
                bottom_left: ScaledPixels((i % 19) as f32 + 0.375),
                bottom_right: ScaledPixels((i % 23) as f32 + 0.5),
            };
            q.border_widths = gpui::Edges {
                top: ScaledPixels((i % 9) as f32),
                right: ScaledPixels((i % 3) as f32 + 0.25),
                bottom: ScaledPixels((i % 5) as f32 + 0.75),
                left: ScaledPixels((i % 7) as f32 + 0.5),
            };
            q.border_style = if i % 2 == 0 {
                gpui::BorderStyle::Dashed
            } else {
                gpui::BorderStyle::Solid
            };
            q.border_color = hsla(0.55, 0.9, 0.7, 0.6);
            q.background = hsla((i % 13) as f32 / 13.0, 0.6, 0.4, (i % 5) as f32 / 4.0).into();
            q.content_mask.bounds = rectangle(3.25, 4.75, 249.25, 242.5);
            if i % 3 == 0 {
                q.fade = gpui::EdgeFadeParams {
                    top_y: 2.5,
                    bottom_y: 253.25,
                    left_x: 0.75,
                    right_x: 254.5,
                    band_top: 25.25,
                    band_bottom: 30.5,
                    band_left: 13.5,
                    band_right: 20.75,
                };
            }
            quads.push(q);
        }
        assert!(
            quads
                .iter()
                .filter(|q| solid_quad_interior(q, [256, 256]).is_some())
                .count()
                > 20
        );
    }
    let instances = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: unsafe { WgpuRenderer::instance_bytes(&quads) },
        usage: wgpu::BufferUsages::STORAGE,
    });
    let instance_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layouts.instances,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: instances.as_entire_binding(),
        }],
    });
    for format in [
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Bgra8UnormSrgb,
    ] {
        for premultiplied in [false, true] {
            let mode = if premultiplied {
                wgpu::CompositeAlphaMode::PreMultiplied
            } else {
                wgpu::CompositeAlphaMode::PostMultiplied
            };
            let pipelines =
                WgpuRenderer::create_pipelines(&device, &layouts, format, mode, 4, false, false);
            let globals = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::bytes_of(&GlobalParams {
                    viewport_size: [256.0, 256.0],
                    premultiplied_alpha: u32::from(premultiplied),
                    pad: 0,
                }),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let gamma = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &layouts.globals,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: globals.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: gamma.as_entire_binding(),
                    },
                ],
            });
            let render = |specialized: bool, range: std::ops::Range<usize>| {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width: 256,
                        height: 256,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let readback = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 256 * 256 * 4,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut encoder = device.create_command_encoder(&Default::default());
                {
                    let view = texture.create_view(&Default::default());
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                    pass.set_bind_group(0, &globals_group, &[]);
                    pass.set_bind_group(1, &instance_group, &[]);
                    if specialized {
                        for (i, quad) in quads.iter().enumerate().take(range.end).skip(range.start)
                        {
                            let range = i as u32..i as u32 + 1;
                            if is_simple_solid_quad(quad) {
                                pass.set_pipeline(if is_unclipped_opaque_quad(quad) {
                                    &pipelines.opaque_solid_quads
                                } else {
                                    &pipelines.solid_quads
                                });
                                pass.draw(0..4, range);
                            } else if let Some(interior) = solid_quad_interior(quad, [256, 256]) {
                                pass.set_pipeline(&pipelines.quads);
                                for [x, y, w, h] in exterior_scissors(interior, [256, 256]) {
                                    if w > 0 && h > 0 {
                                        pass.set_scissor_rect(x, y, w, h);
                                        pass.draw(0..4, range.clone());
                                    }
                                }
                                pass.set_pipeline(
                                    if quad
                                        .background
                                        .as_solid()
                                        .is_some_and(|color| color.a == 1.0)
                                    {
                                        &pipelines.opaque_solid_quads
                                    } else {
                                        &pipelines.solid_quads
                                    },
                                );
                                pass.set_scissor_rect(
                                    interior[0],
                                    interior[1],
                                    interior[2],
                                    interior[3],
                                );
                                pass.draw(0..4, range);
                                pass.set_scissor_rect(0, 0, 256, 256);
                            } else {
                                pass.set_pipeline(&pipelines.quads);
                                pass.draw(0..4, range);
                            }
                        }
                    } else {
                        pass.set_pipeline(&pipelines.quads);
                        pass.draw(0..4, range.start as u32..range.end as u32);
                    }
                }
                encoder.copy_texture_to_buffer(
                    texture.as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &readback,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(1024),
                            rows_per_image: None,
                        },
                    },
                    texture.size(),
                );
                queue.submit([encoder.finish()]);
                let (tx, rx) = std::sync::mpsc::channel();
                readback
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |result| {
                        tx.send(result).unwrap();
                    });
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                rx.recv().unwrap().unwrap();
                let pixels = readback.slice(..).get_mapped_range().to_vec();
                readback.unmap();
                pixels
            };
            let ranges = (0..quads.len())
                .map(|i| i..i + 1)
                .chain(std::iter::once(0..quads.len()));
            for range in ranges {
                let expected = render(false, range.clone());
                let actual = render(true, range.clone());
                let different = expected.iter().zip(&actual).filter(|(a, b)| a != b).count();
                assert_eq!(
                    different, 0,
                    "{format:?}, premultiplied={premultiplied}, range={range:?}"
                );
            }
        }
    }
}

#[test]
fn interior_regions_are_bounded_and_disjoint() {
    for viewport in [[256, 256], [1279, 799], [1920, 1080]] {
        let mut q = quad();
        q.bounds = rectangle(
            -2.25,
            -3.75,
            viewport[0] as f32 + 5.5,
            viewport[1] as f32 + 7.25,
        );
        q.content_mask.bounds = rectangle(0.0, 0.0, viewport[0] as f32, viewport[1] as f32);
        q.corner_radii.top_left = ScaledPixels(8.5);
        let interior = solid_quad_interior(&q, viewport).unwrap();
        let mut regions = exterior_scissors(interior, viewport).to_vec();
        regions.push(interior);
        assert_eq!(
            regions
                .iter()
                .map(|r| u64::from(r[2]) * u64::from(r[3]))
                .sum::<u64>(),
            u64::from(viewport[0]) * u64::from(viewport[1])
        );
        for (i, &[x, y, w, h]) in regions.iter().enumerate() {
            assert!(x + w <= viewport[0] && y + h <= viewport[1]);
            for &[x2, y2, w2, h2] in &regions[i + 1..] {
                assert!(x + w <= x2 || x2 + w2 <= x || y + h <= y2 || y2 + h2 <= y);
            }
        }
        q.bounds.origin.x = ScaledPixels(f32::INFINITY);
        assert!(solid_quad_interior(&q, viewport).is_none());
        q.bounds.origin.x = ScaledPixels(-16_777_216.0);
        assert!(solid_quad_interior(&q, viewport).is_none());
    }
}

#[test]
fn opaque_pipeline_requires_complete_coverage() {
    let mut q = quad();
    assert!(!is_unclipped_opaque_quad(&q));
    q.background = hsla(0.2, 0.7, 0.3, 1.0).into();
    assert!(is_unclipped_opaque_quad(&q));
    q.content_mask.bounds.origin.x = ScaledPixels(0.5);
    assert!(!is_unclipped_opaque_quad(&q));
    q.content_mask.bounds.origin.x = ScaledPixels(0.0);
    q.fade.band_top = 0.1;
    assert!(!is_unclipped_opaque_quad(&q));
}
