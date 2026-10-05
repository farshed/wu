use super::{ChatView, ui};
use crate::chat_style::{hairline, icon, page};
use anyhow::{Context as _, Result, bail};
use gpui::{
    ClipboardEntry, Context, ExternalPaths, Image, ImageFormat, IntoElement, ObjectFit,
    ParentElement, PathPromptOptions, SharedString, Styled, Window, div, img, px,
};
use std::path::{Path, PathBuf};
use ui::{IconName, Tooltip, prelude::*};
use util::ResultExt as _;

const MAX_IMAGE_BYTES: u64 = 24 * 1024 * 1024;
// Claude's 5 MB limit applies to the base64 text, which is a third larger than the file.
const MAX_SENT_IMAGE_BYTES: u64 = 3_700_000;
const MAX_IMAGE_SIDE: u32 = 2048;
const THUMBNAIL_SIZE: f32 = 56.;
const STRIP_GAP: f32 = 8.;
const STRIP_PAD_TOP: f32 = 12.;
const STRIP_PAD_X: f32 = 16.;
const CLOSE_CIRCLE_DIAMETER: f32 = 11.5;
const CLOSE_CIRCLE_GLYPH: f32 = 7.;
const REMOVE_OVERHANG: f32 = 6.;

pub(crate) fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "svg"
            )
        })
}

fn uploads_dir() -> PathBuf {
    paths::data_dir().join("agent_sessions").join("uploads")
}

fn write_upload(uploads: &Path, extension: &str, bytes: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(uploads)?;
    let path = uploads.join(format!("{}.{extension}", uuid::Uuid::new_v4()));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Agents only read PNG, JPEG, GIF and WebP, so other formats are converted to PNG.
fn stage_image_file(path: &Path, uploads: &Path) -> Result<PathBuf> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .len();
    if size > MAX_IMAGE_BYTES {
        bail!("{} is larger than 24 MB", path.display());
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" if size <= MAX_SENT_IMAGE_BYTES => {
            Ok(path.to_path_buf())
        }
        "png" | "jpg" | "jpeg" | "webp" => shrink(path, uploads),
        "svg" => {
            let svg = std::fs::read(path)?;
            let tree = usvg::Tree::from_data(&svg, &usvg::Options::default())?;
            let size = tree.size().to_int_size();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())
                .context("the SVG has no size")?;
            resvg::render(&tree, Default::default(), &mut pixmap.as_mut());
            write_upload(uploads, "png", &pixmap.encode_png()?)
        }
        _ => {
            let image = image::open(path)
                .with_context(|| format!("cannot open {}", path.display()))?;
            let mut png = Vec::new();
            image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
            let converted = write_upload(uploads, "png", &png)?;
            if png.len() as u64 <= MAX_SENT_IMAGE_BYTES {
                return Ok(converted);
            }
            let shrunk = shrink(&converted, uploads);
            std::fs::remove_file(&converted).log_err();
            shrunk
        }
    }
}

fn shrink(path: &Path, uploads: &Path) -> Result<PathBuf> {
    let image = image::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut side = MAX_IMAGE_SIDE;
    loop {
        let resized = if image.width().max(image.height()) > side {
            image.resize(side, side, image::imageops::FilterType::Lanczos3)
        } else {
            image.clone()
        };
        let mut encoded = Vec::new();
        let (format, extension) = if resized.color().has_alpha() {
            (image::ImageFormat::Png, "png")
        } else {
            (image::ImageFormat::Jpeg, "jpg")
        };
        let rgb;
        let to_write = if format == image::ImageFormat::Jpeg {
            rgb = image::DynamicImage::ImageRgb8(resized.to_rgb8());
            &rgb
        } else {
            &resized
        };
        to_write.write_to(&mut std::io::Cursor::new(&mut encoded), format)?;
        if encoded.len() as u64 <= MAX_SENT_IMAGE_BYTES || side <= 512 {
            return write_upload(uploads, extension, &encoded);
        }
        side = side * 3 / 4;
    }
}

fn stage_clipboard_image(image: &Image, uploads: &Path) -> Result<PathBuf> {
    let extension = match image.format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Gif => "gif",
        ImageFormat::Webp => "webp",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        _ => bail!("this image format is not supported"),
    };
    let path = write_upload(uploads, extension, &image.bytes)?;
    if matches!(extension, "png" | "jpg" | "gif" | "webp")
        && image.bytes.len() as u64 <= MAX_SENT_IMAGE_BYTES
    {
        return Ok(path);
    }
    let converted = stage_image_file(&path, uploads);
    std::fs::remove_file(&path).log_err();
    converted
}

enum Pending {
    Files(Vec<PathBuf>),
    Clipboard(Vec<Image>),
}

impl ChatView {
    fn stage(&mut self, pending: Pending, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move {
            let uploads = uploads_dir();
            match pending {
                Pending::Files(paths) => paths
                    .iter()
                    .map(|path| stage_image_file(path, &uploads))
                    .collect::<Vec<_>>(),
                Pending::Clipboard(images) => images
                    .iter()
                    .map(|image| stage_clipboard_image(image, &uploads))
                    .collect::<Vec<_>>(),
            }
        });
        self.staging_count += 1;
        cx.spawn(async move |this, cx| {
            let results = task.await;
            this.update(cx, |this, cx| {
                this.staging_count = this.staging_count.saturating_sub(1);
                this.attachment_error = None;
                for result in results {
                    match result {
                        Ok(path) if !this.attachments.contains(&path) => {
                            this.attachments.push(path)
                        }
                        Ok(_) => {}
                        Err(error) => this.attachment_error = Some(error.to_string().into()),
                    }
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
        cx.notify();
    }

    pub(super) fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let images: Vec<PathBuf> = paths.into_iter().filter(|path| is_image_path(path)).collect();
        if !images.is_empty() {
            self.stage(Pending::Files(images), cx);
        }
    }

    pub(super) fn pick_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(paths) = picked.await.ok().and_then(|result| result.log_err()).flatten()
            else {
                return;
            };
            this.update(cx, |this, cx| this.attach_paths(paths, cx))
                .log_err();
        })
        .detach();
    }

    /// Returns true when the clipboard held an image, so the editor's own paste should not run.
    pub(super) fn paste_images(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        let images: Vec<Image> = item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image.clone()),
                _ => None,
            })
            .collect();
        let files: Vec<PathBuf> = item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
                _ => None,
            })
            .flatten()
            .filter(|path| is_image_path(path))
            .collect();
        if images.is_empty() && files.is_empty() {
            return false;
        }
        if !images.is_empty() {
            self.stage(Pending::Clipboard(images), cx);
        }
        if !files.is_empty() {
            self.stage(Pending::Files(files), cx);
        }
        true
    }

    pub(super) fn drop_external_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.drop_paths(paths.paths().to_vec(), window, cx);
    }

    pub(super) fn drop_paths(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (images, others): (Vec<PathBuf>, Vec<PathBuf>) =
            paths.into_iter().partition(|path| is_image_path(path));
        self.attach_paths(images, cx);
        self.insert_mentions(others, window, cx);
        cx.notify();
    }

    pub(super) fn selection_paths(
        &self,
        selection: &workspace::DraggedSelection,
        cx: &App,
    ) -> Vec<PathBuf> {
        let project = self.project.read(cx);
        selection
            .items()
            .filter_map(|entry| {
                let path = project.path_for_entry(entry.entry_id, cx)?;
                project.absolute_path(&path, cx)
            })
            .collect()
    }

    pub(super) fn render_attachment_strip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.attachments.is_empty() && self.attachment_error.is_none() && self.staging_count == 0 {
            return None;
        }
        let colors = cx.theme().colors();
        let frame = hairline(0.10, cx);
        let plate = page(cx);
        let muted = colors.text_muted;
        Some(
            v_flex()
                .w_full()
                .flex_none()
                .px(px(STRIP_PAD_X))
                .pt(px(STRIP_PAD_TOP))
                .gap(px(6.))
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap(px(STRIP_GAP))
                        .children(self.attachments.iter().enumerate().map(|(index, path)| {
                            let group: SharedString = format!("agent-attachment-{index}").into();
                            div()
                                .group(group.clone())
                                .flex_none()
                                .relative()
                                .p(px(REMOVE_OVERHANG))
                                .m(px(-REMOVE_OVERHANG))
                                .child(
                                    div()
                                        .id(("agent-attachment", index))
                                        .size(px(THUMBNAIL_SIZE))
                                        .rounded(px(8.))
                                        .overflow_hidden()
                                        .border_1()
                                        .border_color(frame)
                                        .cursor_pointer()
                                        .child(
                                            img(path.clone())
                                                .w(px(THUMBNAIL_SIZE - 2.))
                                                .h(px(THUMBNAIL_SIZE - 2.))
                                                .rounded(px(7.))
                                                .object_fit(ObjectFit::Cover),
                                        )
                                        .on_click({
                                            let path = path.clone();
                                            cx.listener(move |this, _, window, cx| {
                                                this.open_image(path.clone(), window, cx)
                                            })
                                        }),
                                )
                                .child(
                                    div()
                                        .id(("agent-attachment-remove", index))
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .size(px(18.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(plate)
                                        .shadow_sm()
                                        .cursor_pointer()
                                        .visible_on_hover(group)
                                        .tooltip(Tooltip::text("Remove"))
                                        .child(
                                            div()
                                                .size(px(CLOSE_CIRCLE_DIAMETER))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded_full()
                                                .border_1()
                                                .border_color(muted)
                                                .child(icon(
                                                    IconName::AgentClose,
                                                    px(CLOSE_CIRCLE_GLYPH),
                                                    muted,
                                                )),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            if index < this.attachments.len() {
                                                this.attachments.remove(index);
                                            }
                                            cx.notify();
                                        })),
                                )
                        })),
                )
                .when(self.staging_count > 0, |this| {
                    this.child(
                        div()
                            .text_size(ui(12.))
                            .text_color(colors.text_muted)
                            .child("Preparing image…"),
                    )
                })
                .when_some(self.attachment_error.clone(), |this, error| {
                    this.child(
                        div()
                            .text_size(ui(12.))
                            .text_color(cx.theme().status().error)
                            .child(error),
                    )
                })
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    async fn files_dropped_on_the_chat_tab_attach_instead_of_opening(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::{AgentKind, session::tests::new_store};
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let image = directory.path().join("screenshot.png");
        image::RgbImage::new(2, 2).save(&image).expect("write png");
        let notes = directory.path().join("notes.txt");
        std::fs::write(&notes, "hello").expect("write text");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(project::FakeFs::new(cx.executor()), [], cx).await;
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let chat = workspace.update_in(cx, |workspace, window, cx| {
            let weak = cx.entity().downgrade();
            let chat = cx.new(|cx| {
                ChatView::new(session.clone(), store.clone(), project.clone(), weak, window, cx)
            });
            workspace.add_item_to_active_pane(Box::new(chat.clone()), None, true, window, cx);
            chat
        });
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        let dropped = ExternalPaths([image.clone(), notes].into_iter().collect());
        let drop = |cx: &mut gpui::VisualTestContext| {
            pane.update_in(cx, |pane, window, cx| {
                let item = pane.active_item().expect("chat tab");
                item.handle_drop(pane, &dropped, window, cx)
            })
        };
        assert!(drop(cx), "the chat takes the drop");
        cx.run_until_parked();
        chat.read_with(cx, |chat, cx| {
            assert_eq!(chat.attachments, vec![image.clone()]);
            assert!(chat.composer.read(cx).text(cx).contains("notes.txt"));
        });

        pane.update(cx, |pane, _| {
            pane.drag_split_direction = Some(workspace::SplitDirection::Right)
        });
        assert!(!drop(cx), "edge drops still split the pane");
    }

    #[test]
    fn supported_images_are_used_in_place_and_others_become_png() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let uploads = directory.path().join("uploads");
        let png = directory.path().join("shot.png");
        image::RgbImage::new(2, 2).save(&png).expect("write png");
        assert_eq!(stage_image_file(&png, &uploads).expect("png"), png);

        let bmp = directory.path().join("shot.bmp");
        image::RgbImage::new(2, 2).save(&bmp).expect("write bmp");
        let converted = stage_image_file(&bmp, &uploads).expect("bmp");
        assert_eq!(converted.extension().and_then(|extension| extension.to_str()), Some("png"));
        assert!(converted.starts_with(&uploads));

        let big = directory.path().join("big.png");
        let noise = image::RgbImage::from_fn(2400, 1800, |x, y| {
            let value = (x.wrapping_mul(7919) ^ y.wrapping_mul(104_729)) as u8;
            image::Rgb([value, value.wrapping_add(85), value.wrapping_add(170)])
        });
        noise.save(&big).expect("write big png");
        assert!(std::fs::metadata(&big).expect("size").len() > MAX_SENT_IMAGE_BYTES);
        let shrunk = stage_image_file(&big, &uploads).expect("shrunk");
        assert!(std::fs::metadata(&shrunk).expect("size").len() <= MAX_SENT_IMAGE_BYTES);
        let (width, height) = image::image_dimensions(&shrunk).expect("dimensions");
        assert!(width.max(height) <= MAX_IMAGE_SIDE);

        assert!(is_image_path(Path::new("a/B.JPEG")));
        assert!(!is_image_path(Path::new("notes.txt")));
    }
}
