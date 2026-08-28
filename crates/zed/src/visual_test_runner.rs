// Allow blocking process commands in this binary - it's a synchronous test runner
#![allow(clippy::disallowed_methods)]

//! Visual Test Runner
//!
//! This binary runs visual regression tests for Zed's UI. It captures screenshots
//! of real Zed windows and compares them against baseline images.
//!
//! **Note: This tool is macOS-only** because it uses `VisualTestAppContext` which
//! depends on the macOS Metal renderer for accurate screenshot capture.
//!
//! ## How It Works
//!
//! This tool uses `VisualTestAppContext` which combines:
//! - Real Metal/compositor rendering for accurate screenshots
//! - Deterministic task scheduling via TestDispatcher
//! - Controllable time via `advance_clock` for testing time-based behaviors
//!
//! This approach:
//! - Does NOT require Screen Recording permission
//! - Does NOT require the window to be visible on screen
//! - Captures raw GPUI output without system window chrome
//! - Is fully deterministic - tooltips, animations, etc. work reliably
//!
//! ## Usage
//!
//! Run the visual tests:
//!   cargo run -p zed --bin zed_visual_test_runner --features visual-tests
//!
//! Update baseline images (when UI intentionally changes):
//!   UPDATE_BASELINE=1 cargo run -p zed --bin zed_visual_test_runner --features visual-tests
//!
//! ## Environment Variables
//!
//!   UPDATE_BASELINE - Set to update baseline images instead of comparing
//!   VISUAL_TEST_OUTPUT_DIR - Directory to save test output (default: target/visual_tests)

// Stub main for non-macOS platforms
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Visual test runner is only supported on macOS");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    // Set ZED_STATELESS early to prevent file system access to real config directories
    // This must be done before any code accesses zed_env_vars::ZED_STATELESS
    // SAFETY: We're at the start of main(), before any threads are spawned
    unsafe {
        std::env::set_var("ZED_STATELESS", "1");
    }

    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();

    let update_baseline = std::env::var("UPDATE_BASELINE").is_ok();

    // Create a temporary directory for test files
    // Canonicalize the path to resolve symlinks (on macOS, /var -> /private/var)
    // which prevents "path does not exist" errors during worktree scanning
    // Use keep() to prevent auto-cleanup - background worktree tasks may still be running
    // when tests complete, so we let the OS clean up temp directories on process exit
    let temp_dir = tempfile::tempdir().expect("Failed to create temp directory");
    let temp_path = temp_dir.keep();
    let canonical_temp = temp_path
        .canonicalize()
        .expect("Failed to canonicalize temp directory");
    let project_path = canonical_temp.join("project");
    std::fs::create_dir_all(&project_path).expect("Failed to create project directory");

    // Create test files in the real filesystem
    create_test_files(&project_path);

    let test_result = std::panic::catch_unwind(|| run_visual_tests(project_path, update_baseline));

    // Note: We don't delete temp_path here because background worktree tasks may still
    // be running. The directory will be cleaned up when the process exits or by the OS.

    match test_result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("Visual tests failed: {}", e);
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("Visual tests panicked");
            std::process::exit(1);
        }
    }
}

// All macOS-specific imports grouped together
#[cfg(target_os = "macos")]
use {
    anyhow::{Context as _, Result},
    assets::Assets,
    gpui::{
        App, AppContext as _, Bounds, KeyBinding, Modifiers, VisualTestAppContext, WindowBounds,
        WindowHandle, WindowOptions, point, px, size,
    },
    image::RgbaImage,
    project_panel::ProjectPanel,
    settings_ui::SettingsWindow,
    std::{
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    },
    util::ResultExt as _,
    workspace::{AppState, MultiWorkspace, Workspace},
    zed_actions::OpenSettingsAt,
};

// All macOS-specific constants grouped together
#[cfg(target_os = "macos")]
mod constants {
    use std::time::Duration;

    /// Baseline images are stored relative to this file
    pub const BASELINE_DIR: &str = "crates/zed/test_fixtures/visual_tests";

    /// Threshold for image comparison (0.0 to 1.0)
    /// Images must match at least this percentage to pass
    pub const MATCH_THRESHOLD: f64 = 0.99;

    /// Tooltip show delay - must match TOOLTIP_SHOW_DELAY in gpui/src/elements/div.rs
    pub const TOOLTIP_SHOW_DELAY: Duration = Duration::from_millis(500);
}

#[cfg(target_os = "macos")]
use constants::*;

#[cfg(target_os = "macos")]
fn run_visual_tests(project_path: PathBuf, update_baseline: bool) -> Result<()> {
    // Create the visual test context with deterministic task scheduling
    // Use real Assets so that SVG icons render properly
    let mut cx = VisualTestAppContext::with_asset_source(
        gpui_platform::current_platform(false),
        Arc::new(Assets),
    );

    // Load embedded fonts (IBM Plex Sans, Lilex, etc.) so UI renders with correct fonts
    cx.update(|cx| {
        Assets.load_fonts(cx).unwrap();
    });

    // Initialize settings store with real default settings (not test settings)
    // Test settings use Courier font, but we want the real Zed fonts for visual tests
    cx.update(|cx| {
        settings::init(cx);
    });

    // Create AppState using the test initialization
    let app_state = cx.update(|cx| init_app_state(cx));

    // Set the global app state so settings_ui and other subsystems can find it
    cx.update(|cx| {
        AppState::set_global(app_state.clone(), cx);
    });

    // Initialize all Zed subsystems
    cx.update(|cx| {
        gpui_tokio::init(cx);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        workspace::init(app_state.clone(), cx);
        release_channel::init(semver::Version::new(0, 0, 0), cx);
        command_palette::init(cx);
        editor::init(cx);
        title_bar::init(cx);
        project_panel::init(cx);
        outline_panel::init(cx);
        terminal_view::init(cx);
        image_viewer::init(cx);
        search::init(cx);
        lsp_locations::init(cx);
        cx.set_global(workspace::PaneSearchBarCallbacks {
            setup_search_bar: |languages, toolbar, window, cx| {
                let search_bar = cx.new(|cx| search::BufferSearchBar::new(languages, window, cx));
                toolbar.update(cx, |toolbar, cx| {
                    toolbar.add_item(search_bar, window, cx);
                });
            },
            wrap_div_with_search_actions: search::buffer_search::register_pane_search_actions,
        });
        git_ui::init(cx);
        settings_ui::init(cx);

        // Load default keymaps so tooltips can show keybindings like "f9" for ToggleBreakpoint
        // We load a minimal set of editor keybindings needed for visual tests
        cx.bind_keys([KeyBinding::new(
            "f9",
            editor::actions::ToggleBreakpoint,
            Some("Editor"),
        )]);
    });

    // Run until all initialization tasks complete
    cx.run_until_parked();

    // Open workspace window
    let window_size = size(px(1280.0), px(800.0));
    let bounds = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: window_size,
    };

    // Create a project for the workspace
    let project = cx.update(|cx| {
        project::Project::local(
            app_state.client.clone(),
            app_state.node_runtime.clone(),
            app_state.user_store.clone(),
            app_state.languages.clone(),
            app_state.fs.clone(),
            None,
            project::LocalProjectFlags {
                init_worktree_trust: false,
                ..Default::default()
            },
            cx,
        )
    });

    let workspace_window: WindowHandle<Workspace> = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |window, cx| {
                    cx.new(|cx| {
                        Workspace::new(None, project.clone(), app_state.clone(), window, cx)
                    })
                },
            )
        })
        .context("Failed to open workspace window")?;

    cx.run_until_parked();

    // Add the test project as a worktree
    let add_worktree_task = workspace_window
        .update(&mut cx, |workspace, _window, cx| {
            let project = workspace.project().clone();
            project.update(cx, |project, cx| {
                project.find_or_create_worktree(&project_path, true, cx)
            })
        })
        .context("Failed to start adding worktree")?;

    // Use block_test to wait for the worktree task
    // block_test runs both foreground and background tasks, which is needed because
    // worktree creation spawns foreground tasks via cx.spawn
    // Allow parking since filesystem operations happen outside the test dispatcher
    cx.background_executor.allow_parking();
    let worktree_result = cx.foreground_executor.block_test(add_worktree_task);
    cx.background_executor.forbid_parking();
    worktree_result.context("Failed to add worktree")?;

    cx.run_until_parked();

    // Create and add the project panel
    let (weak_workspace, async_window_cx) = workspace_window
        .update(&mut cx, |workspace, window, cx| {
            (workspace.weak_handle(), window.to_async(cx))
        })
        .context("Failed to get workspace handle")?;

    cx.background_executor.allow_parking();
    let panel = cx
        .foreground_executor
        .block_test(ProjectPanel::load(weak_workspace, async_window_cx))
        .context("Failed to load project panel")?;
    cx.background_executor.forbid_parking();

    workspace_window
        .update(&mut cx, |workspace, window, cx| {
            workspace.add_panel(panel, window, cx);
        })
        .log_err();

    cx.run_until_parked();

    // Open the project panel
    workspace_window
        .update(&mut cx, |workspace, window, cx| {
            workspace.open_panel::<ProjectPanel>(window, cx);
        })
        .log_err();

    cx.run_until_parked();

    // Open main.rs in the editor
    let open_file_task = workspace_window
        .update(&mut cx, |workspace, window, cx| {
            let worktree = workspace.project().read(cx).worktrees(cx).next();
            if let Some(worktree) = worktree {
                let worktree_id = worktree.read(cx).id();
                let rel_path: std::sync::Arc<util::rel_path::RelPath> =
                    util::rel_path::rel_path("src/main.rs").into();
                let project_path: project::ProjectPath = (worktree_id, rel_path).into();
                Some(workspace.open_path(project_path, None, true, window, cx))
            } else {
                None
            }
        })
        .log_err()
        .flatten();

    if let Some(task) = open_file_task {
        cx.background_executor.allow_parking();
        let block_result = cx.foreground_executor.block_test(task);
        cx.background_executor.forbid_parking();
        if let Ok(item) = block_result {
            workspace_window
                .update(&mut cx, |workspace, window, cx| {
                    let pane = workspace.active_pane().clone();
                    pane.update(cx, |pane, cx| {
                        if let Some(index) = pane.index_for_item(item.as_ref()) {
                            pane.activate_item(index, true, true, window, cx);
                        }
                    });
                })
                .log_err();
        }
    }

    cx.run_until_parked();

    // Request a window refresh
    cx.update_window(workspace_window.into(), |_, window, _cx| {
        window.refresh();
    })
    .log_err();

    cx.run_until_parked();

    // Track test results
    let mut passed = 0;
    let mut failed = 0;
    let mut updated = 0;

    // Run Test 1: Project Panel (with project panel visible)
    println!("\n--- Test 1: project_panel ---");
    match run_visual_test(
        "project_panel",
        workspace_window.into(),
        &mut cx,
        update_baseline,
    ) {
        Ok(TestResult::Passed) => {
            println!("✓ project_panel: PASSED");
            passed += 1;
        }
        Ok(TestResult::BaselineUpdated(_)) => {
            println!("✓ project_panel: Baseline updated");
            updated += 1;
        }
        Err(e) => {
            eprintln!("✗ project_panel: FAILED - {}", e);
            failed += 1;
        }
    }

    // Run Test 2: Workspace with Editor
    println!("\n--- Test 2: workspace_with_editor ---");

    // Close project panel for this test
    workspace_window
        .update(&mut cx, |workspace, window, cx| {
            workspace.close_panel::<ProjectPanel>(window, cx);
        })
        .log_err();

    cx.run_until_parked();

    match run_visual_test(
        "workspace_with_editor",
        workspace_window.into(),
        &mut cx,
        update_baseline,
    ) {
        Ok(TestResult::Passed) => {
            println!("✓ workspace_with_editor: PASSED");
            passed += 1;
        }
        Ok(TestResult::BaselineUpdated(_)) => {
            println!("✓ workspace_with_editor: Baseline updated");
            updated += 1;
        }
        Err(e) => {
            eprintln!("✗ workspace_with_editor: FAILED - {}", e);
            failed += 1;
        }
    }

    // Run Test 4: Error wrapping visual tests
    println!("\n--- Test 4: error_message_wrapping ---");
    match run_error_wrapping_visual_tests(app_state.clone(), &mut cx, update_baseline) {
        Ok(TestResult::Passed) => {
            println!("✓ error_message_wrapping: PASSED");
            passed += 1;
        }
        Ok(TestResult::BaselineUpdated(_)) => {
            println!("✓ error_message_wrapping: Baselines updated");
            updated += 1;
        }
        Err(e) => {
            eprintln!("✗ error_message_wrapping: FAILED - {}", e);
            failed += 1;
        }
    }

    // Run Test 6: Breakpoint Hover visual tests
    println!("\n--- Test 6: breakpoint_hover (3 variants) ---");
    match run_breakpoint_hover_visual_tests(app_state.clone(), &mut cx, update_baseline) {
        Ok(TestResult::Passed) => {
            println!("✓ breakpoint_hover: PASSED");
            passed += 1;
        }
        Ok(TestResult::BaselineUpdated(_)) => {
            println!("✓ breakpoint_hover: Baselines updated");
            updated += 1;
        }
        Err(e) => {
            eprintln!("✗ breakpoint_hover: FAILED - {}", e);
            failed += 1;
        }
    }

    // Run Test 10: Settings UI sub-page auto-open visual tests
    println!("\n--- Test 10: settings_ui_subpage_auto_open (2 variants) ---");
    match run_settings_ui_subpage_visual_tests(app_state.clone(), &mut cx, update_baseline) {
        Ok(TestResult::Passed) => {
            println!("✓ settings_ui_subpage_auto_open: PASSED");
            passed += 1;
        }
        Ok(TestResult::BaselineUpdated(_)) => {
            println!("✓ settings_ui_subpage_auto_open: Baselines updated");
            updated += 1;
        }
        Err(e) => {
            eprintln!("✗ settings_ui_subpage_auto_open: FAILED - {}", e);
            failed += 1;
        }
    }

    // Clean up the main workspace's worktree to stop background scanning tasks
    // This prevents "root path could not be canonicalized" errors when main() drops temp_dir
    workspace_window
        .update(&mut cx, |workspace, _window, cx| {
            let project = workspace.project().clone();
            project.update(cx, |project, cx| {
                let worktree_ids: Vec<_> =
                    project.worktrees(cx).map(|wt| wt.read(cx).id()).collect();
                for id in worktree_ids {
                    project.remove_worktree(id, cx);
                }
            });
        })
        .log_err();

    cx.run_until_parked();

    // Close the main window
    cx.update_window(workspace_window.into(), |_, window, _cx| {
        window.remove_window();
    })
    .log_err();

    // Run until all cleanup tasks complete
    cx.run_until_parked();

    // Give background tasks time to finish, including scrollbar hide timers (1 second)
    for _ in 0..15 {
        cx.advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }

    // Print summary
    println!("\n=== Test Summary ===");
    println!("Passed: {}", passed);
    println!("Failed: {}", failed);
    if updated > 0 {
        println!("Baselines Updated: {}", updated);
    }

    if failed > 0 {
        eprintln!("\n=== Visual Tests FAILED ===");
        Err(anyhow::anyhow!("{} tests failed", failed))
    } else {
        println!("\n=== All Visual Tests PASSED ===");
        Ok(())
    }
}

#[cfg(target_os = "macos")]
enum TestResult {
    Passed,
    BaselineUpdated(PathBuf),
}

#[cfg(target_os = "macos")]
fn run_visual_test(
    test_name: &str,
    window: gpui::AnyWindowHandle,
    cx: &mut VisualTestAppContext,
    update_baseline: bool,
) -> Result<TestResult> {
    // Ensure all pending work is done
    cx.run_until_parked();

    // Refresh the window to ensure it's fully rendered
    cx.update_window(window, |_, window, _cx| {
        window.refresh();
    })?;

    cx.run_until_parked();

    // Capture the screenshot using direct texture capture
    let screenshot = cx.capture_screenshot(window)?;

    // Get paths
    let baseline_path = get_baseline_path(test_name);
    let output_dir = std::env::var("VISUAL_TEST_OUTPUT_DIR")
        .unwrap_or_else(|_| "target/visual_tests".to_string());
    let output_path = PathBuf::from(&output_dir).join(format!("{}.png", test_name));

    // Ensure output directory exists
    std::fs::create_dir_all(&output_dir)?;

    // Always save the current screenshot
    screenshot.save(&output_path)?;
    println!("  Screenshot saved to: {}", output_path.display());

    if update_baseline {
        // Update the baseline
        if let Some(parent) = baseline_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        screenshot.save(&baseline_path)?;
        println!("  Baseline updated: {}", baseline_path.display());
        return Ok(TestResult::BaselineUpdated(baseline_path));
    }

    // Compare with baseline
    if !baseline_path.exists() {
        return Err(anyhow::anyhow!(
            "Baseline not found: {}. Run with UPDATE_BASELINE=1 to create it.",
            baseline_path.display()
        ));
    }

    let baseline = image::open(&baseline_path)?.to_rgba8();
    let comparison = compare_images(&screenshot, &baseline);

    println!(
        "  Match: {:.2}% ({} different pixels)",
        comparison.match_percentage * 100.0,
        comparison.diff_pixel_count
    );

    if comparison.match_percentage >= MATCH_THRESHOLD {
        Ok(TestResult::Passed)
    } else {
        // Save diff image
        let diff_path = PathBuf::from(&output_dir).join(format!("{}_diff.png", test_name));
        comparison.diff_image.save(&diff_path)?;
        println!("  Diff image saved to: {}", diff_path.display());

        Err(anyhow::anyhow!(
            "Image mismatch: {:.2}% match (threshold: {:.2}%)",
            comparison.match_percentage * 100.0,
            MATCH_THRESHOLD * 100.0
        ))
    }
}

#[cfg(target_os = "macos")]
fn get_baseline_path(test_name: &str) -> PathBuf {
    // Get the workspace root (where Cargo.toml is)
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let workspace_root = PathBuf::from(manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    workspace_root
        .join(BASELINE_DIR)
        .join(format!("{}.png", test_name))
}

#[cfg(target_os = "macos")]
struct ImageComparison {
    match_percentage: f64,
    diff_image: RgbaImage,
    diff_pixel_count: u32,
    #[allow(dead_code)]
    total_pixels: u32,
}

#[cfg(target_os = "macos")]
fn compare_images(actual: &RgbaImage, expected: &RgbaImage) -> ImageComparison {
    let width = actual.width().max(expected.width());
    let height = actual.height().max(expected.height());
    let total_pixels = width * height;

    let mut diff_image = RgbaImage::new(width, height);
    let mut matching_pixels = 0u32;

    for y in 0..height {
        for x in 0..width {
            let actual_pixel = if x < actual.width() && y < actual.height() {
                *actual.get_pixel(x, y)
            } else {
                image::Rgba([0, 0, 0, 0])
            };

            let expected_pixel = if x < expected.width() && y < expected.height() {
                *expected.get_pixel(x, y)
            } else {
                image::Rgba([0, 0, 0, 0])
            };

            if pixels_are_similar(&actual_pixel, &expected_pixel) {
                matching_pixels += 1;
                // Semi-transparent green for matching pixels
                diff_image.put_pixel(x, y, image::Rgba([0, 255, 0, 64]));
            } else {
                // Bright red for differing pixels
                diff_image.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
    }

    let match_percentage = matching_pixels as f64 / total_pixels as f64;
    let diff_pixel_count = total_pixels - matching_pixels;

    ImageComparison {
        match_percentage,
        diff_image,
        diff_pixel_count,
        total_pixels,
    }
}

#[cfg(target_os = "macos")]
fn pixels_are_similar(a: &image::Rgba<u8>, b: &image::Rgba<u8>) -> bool {
    const TOLERANCE: i16 = 2;
    (a.0[0] as i16 - b.0[0] as i16).abs() <= TOLERANCE
        && (a.0[1] as i16 - b.0[1] as i16).abs() <= TOLERANCE
        && (a.0[2] as i16 - b.0[2] as i16).abs() <= TOLERANCE
        && (a.0[3] as i16 - b.0[3] as i16).abs() <= TOLERANCE
}

#[cfg(target_os = "macos")]
fn create_test_files(project_path: &Path) {
    // Create src directory
    let src_dir = project_path.join("src");
    std::fs::create_dir_all(&src_dir).expect("Failed to create src directory");

    // Create main.rs
    let main_rs = r#"fn main() {
    println!("Hello, world!");

    let x = 42;
    let y = x * 2;

    if y > 50 {
        println!("y is greater than 50");
    } else {
        println!("y is not greater than 50");
    }

    for i in 0..10 {
        println!("i = {}", i);
    }
}

fn helper_function(a: i32, b: i32) -> i32 {
    a + b
}

struct MyStruct {
    field1: String,
    field2: i32,
}

impl MyStruct {
    fn new(name: &str, value: i32) -> Self {
        Self {
            field1: name.to_string(),
            field2: value,
        }
    }

    fn get_value(&self) -> i32 {
        self.field2
    }
}
"#;
    std::fs::write(src_dir.join("main.rs"), main_rs).expect("Failed to write main.rs");

    // Create lib.rs
    let lib_rs = r#"//! A sample library for visual testing

pub mod utils;

/// A public function in the library
pub fn library_function() -> String {
    "Hello from lib".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!(library_function(), "Hello from lib");
    }
}
"#;
    std::fs::write(src_dir.join("lib.rs"), lib_rs).expect("Failed to write lib.rs");

    // Create utils.rs
    let utils_rs = r#"//! Utility functions

/// Format a number with commas
pub fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

/// Calculate fibonacci number
pub fn fibonacci(n: u32) -> u64 {
    match n {
        0 => 0,
        1 => 1,
        _ => fibonacci(n - 1) + fibonacci(n - 2),
    }
}
"#;
    std::fs::write(src_dir.join("utils.rs"), utils_rs).expect("Failed to write utils.rs");

    // Create Cargo.toml
    let cargo_toml = r#"[package]
name = "test_project"
version = "0.1.0"
edition = "2021"

[dependencies]
"#;
    std::fs::write(project_path.join("Cargo.toml"), cargo_toml)
        .expect("Failed to write Cargo.toml");

    // Create README.md
    let readme = r#"# Test Project

This is a test project for visual testing of Zed.

## Features

- Feature 1
- Feature 2
- Feature 3

## Usage

```bash
cargo run
```
"#;
    std::fs::write(project_path.join("README.md"), readme).expect("Failed to write README.md");
}

#[cfg(target_os = "macos")]
fn init_app_state(cx: &mut App) -> Arc<AppState> {
    use fs::Fs;
    use node_runtime::NodeRuntime;
    use session::Session;
    use settings::SettingsStore;

    if !cx.has_global::<SettingsStore>() {
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
    }

    // Use the real filesystem instead of FakeFs so we can access actual files on disk
    let fs: Arc<dyn Fs> = Arc::new(fs::RealFs::new(None, cx.background_executor().clone()));
    <dyn Fs>::set_global(fs.clone(), cx);

    let languages = Arc::new(language::LanguageRegistry::test(
        cx.background_executor().clone(),
    ));
    let http_client = http_client::FakeHttpClient::with_404_response();
    let client = client::Client::new(http_client);
    let session = cx.new(|cx| session::AppSession::new(Session::test(), cx));
    let user_store = cx.new(|cx| client::UserStore::new(client.clone(), cx));
    let workspace_store = cx.new(|cx| workspace::WorkspaceStore::new(cx));

    theme_settings::init(theme::LoadThemes::JustBase, cx);

    let app_state = Arc::new(AppState {
        client,
        fs,
        languages,
        user_store,
        workspace_store,
        node_runtime: NodeRuntime::unavailable(),
        build_window_options: |_, _| Default::default(),
        session,
    });
    AppState::set_global(app_state.clone(), cx);
    app_state
}

/// Runs visual tests for breakpoint hover states in the editor gutter.
///
/// This test captures three states:
/// 1. Gutter with line numbers, no breakpoint hover (baseline)
/// 2. Gutter with breakpoint hover indicator (gray circle)
/// 3. Gutter with breakpoint hover AND tooltip
#[cfg(target_os = "macos")]
fn run_breakpoint_hover_visual_tests(
    app_state: Arc<AppState>,
    cx: &mut VisualTestAppContext,
    update_baseline: bool,
) -> Result<TestResult> {
    // Create a temporary directory with a simple test file
    let temp_dir = tempfile::tempdir()?;
    let temp_path = temp_dir.keep();
    let canonical_temp = temp_path.canonicalize()?;
    let project_path = canonical_temp.join("project");
    std::fs::create_dir_all(&project_path)?;

    // Create a simple file with a few lines
    let src_dir = project_path.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let test_content = r#"fn main() {
    println!("Hello");
    let x = 42;
}
"#;
    std::fs::write(src_dir.join("test.rs"), test_content)?;

    // Create a small window - just big enough to show gutter and a few lines
    let window_size = size(px(300.0), px(200.0));
    let bounds = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: window_size,
    };

    // Create project
    let project = cx.update(|cx| {
        project::Project::local(
            app_state.client.clone(),
            app_state.node_runtime.clone(),
            app_state.user_store.clone(),
            app_state.languages.clone(),
            app_state.fs.clone(),
            None,
            project::LocalProjectFlags {
                init_worktree_trust: false,
                ..Default::default()
            },
            cx,
        )
    });

    // Open workspace window
    let workspace_window: WindowHandle<Workspace> = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |window, cx| {
                    cx.new(|cx| {
                        Workspace::new(None, project.clone(), app_state.clone(), window, cx)
                    })
                },
            )
        })
        .context("Failed to open breakpoint test window")?;

    cx.run_until_parked();

    // Add the project as a worktree
    let add_worktree_task = workspace_window
        .update(cx, |workspace, _window, cx| {
            let project = workspace.project().clone();
            project.update(cx, |project, cx| {
                project.find_or_create_worktree(&project_path, true, cx)
            })
        })
        .context("Failed to start adding worktree")?;

    cx.background_executor.allow_parking();
    let worktree_result = cx.foreground_executor.block_test(add_worktree_task);
    cx.background_executor.forbid_parking();
    worktree_result.context("Failed to add worktree")?;

    cx.run_until_parked();

    // Open the test file
    let open_file_task = workspace_window
        .update(cx, |workspace, window, cx| {
            let worktree = workspace.project().read(cx).worktrees(cx).next();
            if let Some(worktree) = worktree {
                let worktree_id = worktree.read(cx).id();
                let rel_path: std::sync::Arc<util::rel_path::RelPath> =
                    util::rel_path::rel_path("src/test.rs").into();
                let project_path: project::ProjectPath = (worktree_id, rel_path).into();
                Some(workspace.open_path(project_path, None, true, window, cx))
            } else {
                None
            }
        })
        .log_err()
        .flatten();

    if let Some(task) = open_file_task {
        cx.background_executor.allow_parking();
        cx.foreground_executor.block_test(task).log_err();
        cx.background_executor.forbid_parking();
    }

    cx.run_until_parked();

    // Wait for the editor to fully load
    for _ in 0..10 {
        cx.advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }

    // Refresh window
    cx.update_window(workspace_window.into(), |_, window, _cx| {
        window.refresh();
    })?;

    cx.run_until_parked();

    // Test 1: Gutter visible with line numbers, no breakpoint hover
    let test1_result = run_visual_test(
        "breakpoint_hover_none",
        workspace_window.into(),
        cx,
        update_baseline,
    )?;

    // Test 2: Breakpoint hover indicator (circle) visible
    // The gutter is on the left side. We need to position the mouse over the gutter area
    // for line 1. The breakpoint indicator appears in the leftmost part of the gutter.
    //
    // The breakpoint hover requires multiple steps:
    // 1. Draw to register mouse listeners
    // 2. Mouse move to trigger gutter_hovered and create GutterHoverButton
    // 3. Wait 200ms for is_active to become true
    // 4. Draw again to render the indicator
    //
    // The gutter_position should be in the gutter area to trigger the gutter hover button.
    // The button_position should be directly over the breakpoint icon button for tooltip hover.
    // Based on debug output: button is at origin=(3.12, 66.5) with size=(14, 16)
    let gutter_position = point(px(30.0), px(85.0));
    let button_position = point(px(10.0), px(75.0)); // Center of the breakpoint button

    // Step 1: Initial draw to register mouse listeners
    cx.update_window(workspace_window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })?;
    cx.run_until_parked();

    // Step 2: Simulate mouse move into gutter area
    cx.simulate_mouse_move(
        workspace_window.into(),
        gutter_position,
        None,
        Modifiers::default(),
    );

    // Step 3: Advance clock past 200ms debounce
    cx.advance_clock(Duration::from_millis(300));
    cx.run_until_parked();

    // Step 4: Draw again to pick up the indicator state change
    cx.update_window(workspace_window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })?;
    cx.run_until_parked();

    // Step 5: Another mouse move to keep hover state active
    cx.simulate_mouse_move(
        workspace_window.into(),
        gutter_position,
        None,
        Modifiers::default(),
    );

    // Step 6: Final draw
    cx.update_window(workspace_window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })?;
    cx.run_until_parked();

    let test2_result = run_visual_test(
        "breakpoint_hover_circle",
        workspace_window.into(),
        cx,
        update_baseline,
    )?;

    // Test 3: Breakpoint hover with tooltip visible
    // The tooltip delay is 500ms (TOOLTIP_SHOW_DELAY constant)
    // We need to position the mouse directly over the breakpoint button for the tooltip to show.
    // The button hitbox is approximately at (3.12, 66.5) with size (14, 16).

    // Move mouse directly over the button to trigger tooltip hover
    cx.simulate_mouse_move(
        workspace_window.into(),
        button_position,
        None,
        Modifiers::default(),
    );

    // Draw to register the button's tooltip hover listener
    cx.update_window(workspace_window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })?;
    cx.run_until_parked();

    // Move mouse over button again to trigger tooltip scheduling
    cx.simulate_mouse_move(
        workspace_window.into(),
        button_position,
        None,
        Modifiers::default(),
    );

    // Advance clock past TOOLTIP_SHOW_DELAY (500ms)
    cx.advance_clock(TOOLTIP_SHOW_DELAY + Duration::from_millis(100));
    cx.run_until_parked();

    // Draw to render the tooltip
    cx.update_window(workspace_window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })?;
    cx.run_until_parked();

    // Refresh window
    cx.update_window(workspace_window.into(), |_, window, _cx| {
        window.refresh();
    })?;

    cx.run_until_parked();

    let test3_result = run_visual_test(
        "breakpoint_hover_tooltip",
        workspace_window.into(),
        cx,
        update_baseline,
    )?;

    // Clean up: remove worktrees to stop background scanning
    workspace_window
        .update(cx, |workspace, _window, cx| {
            let project = workspace.project().clone();
            project.update(cx, |project, cx| {
                let worktree_ids: Vec<_> =
                    project.worktrees(cx).map(|wt| wt.read(cx).id()).collect();
                for id in worktree_ids {
                    project.remove_worktree(id, cx);
                }
            });
        })
        .log_err();

    cx.run_until_parked();

    // Close the window
    cx.update_window(workspace_window.into(), |_, window, _cx| {
        window.remove_window();
    })
    .log_err();

    cx.run_until_parked();

    // Give background tasks time to finish
    for _ in 0..15 {
        cx.advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }

    // Return combined result
    match (&test1_result, &test2_result, &test3_result) {
        (TestResult::Passed, TestResult::Passed, TestResult::Passed) => Ok(TestResult::Passed),
        (TestResult::BaselineUpdated(p), _, _)
        | (_, TestResult::BaselineUpdated(p), _)
        | (_, _, TestResult::BaselineUpdated(p)) => Ok(TestResult::BaselineUpdated(p.clone())),
    }
}

/// Runs visual tests for the settings UI sub-page auto-open feature.
///
/// This test verifies that when opening settings via OpenSettingsAt with a path
/// that maps to a single SubPageLink, the sub-page is automatically opened.
///
/// This test captures two states:
/// 1. Settings opened with a path that maps to multiple items (no auto-open)
/// 2. Settings opened with a path that maps to a single SubPageLink (auto-opens sub-page)
#[cfg(target_os = "macos")]
fn run_settings_ui_subpage_visual_tests(
    app_state: Arc<AppState>,
    cx: &mut VisualTestAppContext,
    update_baseline: bool,
) -> Result<TestResult> {
    // Create a workspace window for dispatching actions
    let window_size = size(px(1280.0), px(800.0));
    let bounds = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: window_size,
    };

    let project = cx.update(|cx| {
        project::Project::local(
            app_state.client.clone(),
            app_state.node_runtime.clone(),
            app_state.user_store.clone(),
            app_state.languages.clone(),
            app_state.fs.clone(),
            None,
            project::LocalProjectFlags {
                init_worktree_trust: false,
                ..Default::default()
            },
            cx,
        )
    });

    let workspace_window: WindowHandle<MultiWorkspace> = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |window, cx| {
                    let workspace = cx.new(|cx| {
                        Workspace::new(None, project.clone(), app_state.clone(), window, cx)
                    });
                    cx.new(|cx| MultiWorkspace::new(workspace, window, cx))
                },
            )
        })
        .context("Failed to open workspace window")?;

    cx.run_until_parked();

    // Test 1: Open settings with a path that maps to multiple items (e.g., "git")
    // This should NOT auto-open a sub-page since multiple items match
    workspace_window
        .update(cx, |_workspace, window, cx| {
            window.dispatch_action(
                Box::new(OpenSettingsAt {
                    path: "git".to_string(),
                    target: None,
                }),
                cx,
            );
        })
        .context("Failed to dispatch OpenSettingsAt for multiple items")?;

    cx.run_until_parked();

    // Find the settings window
    let settings_window_1 = cx
        .update(|cx| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<SettingsWindow>())
        })
        .context("Settings window not found")?;

    // Refresh and capture screenshot
    cx.update_window(settings_window_1.into(), |_, window, _cx| {
        window.refresh();
    })?;
    cx.run_until_parked();

    let test1_result = run_visual_test(
        "settings_ui_no_auto_open",
        settings_window_1.into(),
        cx,
        update_baseline,
    )?;

    // Close the settings window
    cx.update_window(settings_window_1.into(), |_, window, _cx| {
        window.remove_window();
    })
    .log_err();
    cx.run_until_parked();

    // Test 2: Open settings with a path that maps to a single SubPageLink
    // "languages.Plain Text" maps to the "Plain Text" language SubPageLink
    // This should auto-open the sub-page
    workspace_window
        .update(cx, |_workspace, window, cx| {
            window.dispatch_action(
                Box::new(OpenSettingsAt {
                    path: "languages.Plain Text".to_string(),
                    target: None,
                }),
                cx,
            );
        })
        .context("Failed to dispatch OpenSettingsAt for single SubPageLink")?;

    cx.run_until_parked();

    // Find the new settings window
    let settings_window_2 = cx
        .update(|cx| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<SettingsWindow>())
        })
        .context("Settings window not found for sub-page test")?;

    // Refresh and capture screenshot
    cx.update_window(settings_window_2.into(), |_, window, _cx| {
        window.refresh();
    })?;
    cx.run_until_parked();

    let test2_result = run_visual_test(
        "settings_ui_subpage_auto_open",
        settings_window_2.into(),
        cx,
        update_baseline,
    )?;

    // Clean up: close the settings window
    cx.update_window(settings_window_2.into(), |_, window, _cx| {
        window.remove_window();
    })
    .log_err();
    cx.run_until_parked();

    // Clean up: close the workspace window
    cx.update_window(workspace_window.into(), |_, window, _cx| {
        window.remove_window();
    })
    .log_err();
    cx.run_until_parked();

    // Give background tasks time to finish
    for _ in 0..5 {
        cx.advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }

    // Return combined result
    match (&test1_result, &test2_result) {
        (TestResult::Passed, TestResult::Passed) => Ok(TestResult::Passed),
        (TestResult::BaselineUpdated(p), _) | (_, TestResult::BaselineUpdated(p)) => {
            Ok(TestResult::BaselineUpdated(p.clone()))
        }
    }
}

#[cfg(target_os = "macos")]
struct ErrorWrappingTestView;

#[cfg(target_os = "macos")]
impl gpui::Render for ErrorWrappingTestView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use ui::{Button, Callout, IconName, LabelSize, Severity, prelude::*, v_flex};

        let long_error_message = "Rate limit reached for gpt-5.2-codex in organization \
            org-QmYpir6k6dkULKU1XUSN6pal on tokens per min (TPM): Limit 500000, Used 442480, \
            Requested 59724. Please try again in 264ms. Visit \
            https://platform.openai.com/account/rate-limits to learn more.";

        let retry_description = "Retrying. Next attempt in 4 seconds (Attempt 1 of 2).";

        v_flex()
            .size_full()
            .bg(cx.theme().colors().background)
            .p_4()
            .gap_4()
            .child(
                Callout::new()
                    .icon(IconName::Warning)
                    .severity(Severity::Warning)
                    .title(long_error_message)
                    .description(retry_description),
            )
            .child(
                Callout::new()
                    .severity(Severity::Error)
                    .icon(IconName::XCircle)
                    .title("An Error Happened")
                    .description(long_error_message)
                    .actions_slot(Button::new("dismiss", "Dismiss").label_size(LabelSize::Small)),
            )
            .child(
                Callout::new()
                    .severity(Severity::Error)
                    .icon(IconName::XCircle)
                    .title(long_error_message)
                    .actions_slot(Button::new("retry", "Retry").label_size(LabelSize::Small)),
            )
    }
}

#[cfg(target_os = "macos")]
fn run_error_wrapping_visual_tests(
    _app_state: Arc<AppState>,
    cx: &mut VisualTestAppContext,
    update_baseline: bool,
) -> Result<TestResult> {
    let window_size = size(px(500.0), px(400.0));
    let bounds = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: window_size,
    };

    let window = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |_window, cx| cx.new(|_| ErrorWrappingTestView),
            )
        })
        .context("Failed to open error wrapping test window")?;

    cx.run_until_parked();

    cx.update_window(window.into(), |_, window, _cx| {
        window.refresh();
    })?;

    cx.run_until_parked();

    let test_result =
        run_visual_test("error_message_wrapping", window.into(), cx, update_baseline)?;

    cx.update_window(window.into(), |_, window, _cx| {
        window.remove_window();
    })
    .log_err();

    cx.run_until_parked();

    for _ in 0..15 {
        cx.advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }

    Ok(test_result)
}
