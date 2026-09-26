use std::sync::Arc;
use std::{fmt::Debug, path::Path};

use anyhow::Result;
use collections::HashMap;
use gpui::{App, AssetSource, Global, SharedString};
use parking_lot::RwLock;
use thiserror::Error;

use crate::{
    Appearance, AppearanceContent, ChevronIcons, DEFAULT_ICON_THEME_NAME, DirectoryIcons,
    IconDefinition, IconTheme, IconThemeFamilyContent, MATERIAL_ICON_THEME_LIGHT_NAME,
    MATERIAL_ICON_THEME_NAME, Theme, ThemeFamily, default_icon_theme,
};

const BUNDLED_ICON_THEME_PATHS: &[&str] = &["icon_themes/material/icon_theme.json"];

/// The metadata for a theme.
#[derive(Debug, Clone)]
pub struct ThemeMeta {
    /// The name of the theme.
    pub name: SharedString,
    /// The appearance of the theme.
    pub appearance: Appearance,
}

/// An error indicating that the theme with the given name was not found.
#[derive(Debug, Error, Clone)]
#[error("theme not found: {0}")]
pub struct ThemeNotFoundError(pub SharedString);

/// An error indicating that the icon theme with the given name was not found.
#[derive(Debug, Error, Clone)]
#[error("icon theme not found: {0}")]
pub struct IconThemeNotFoundError(pub SharedString);

/// The global [`ThemeRegistry`].
///
/// This newtype exists for obtaining a unique [`TypeId`](std::any::TypeId) when
/// inserting the [`ThemeRegistry`] into the context as a global.
///
/// This should not be exposed outside of this module.
#[derive(Default)]
struct GlobalThemeRegistry(Arc<ThemeRegistry>);

impl std::ops::DerefMut for GlobalThemeRegistry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::ops::Deref for GlobalThemeRegistry {
    type Target = Arc<ThemeRegistry>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Global for GlobalThemeRegistry {}

struct ThemeRegistryState {
    themes: HashMap<SharedString, Arc<Theme>>,
    bundled_themes: HashMap<SharedString, Arc<Theme>>,
    icon_themes: HashMap<SharedString, Arc<IconTheme>>,
    /// Whether the extensions have been loaded yet.
    extensions_loaded: bool,
}

/// The registry for themes.
pub struct ThemeRegistry {
    state: RwLock<ThemeRegistryState>,
    assets: Box<dyn AssetSource>,
}

impl ThemeRegistry {
    /// Returns the global [`ThemeRegistry`].
    pub fn global(cx: &App) -> Arc<Self> {
        cx.global::<GlobalThemeRegistry>().0.clone()
    }

    /// Returns the global [`ThemeRegistry`].
    ///
    /// Inserts a default [`ThemeRegistry`] if one does not yet exist.
    pub fn default_global(cx: &mut App) -> Arc<Self> {
        cx.default_global::<GlobalThemeRegistry>().0.clone()
    }

    /// Returns the global [`ThemeRegistry`] if it exists.
    pub fn try_global(cx: &mut App) -> Option<Arc<Self>> {
        cx.try_global::<GlobalThemeRegistry>().map(|t| t.0.clone())
    }

    /// Sets the global [`ThemeRegistry`].
    pub(crate) fn set_global(assets: Box<dyn AssetSource>, cx: &mut App) {
        cx.set_global(GlobalThemeRegistry(Arc::new(ThemeRegistry::new(assets))));
    }

    /// Returns the asset source used by this registry.
    pub fn assets(&self) -> &dyn AssetSource {
        self.assets.as_ref()
    }

    /// Creates a new [`ThemeRegistry`] with the given [`AssetSource`].
    pub fn new(assets: Box<dyn AssetSource>) -> Self {
        let registry = Self {
            state: RwLock::new(ThemeRegistryState {
                themes: HashMap::default(),
                bundled_themes: HashMap::default(),
                icon_themes: HashMap::default(),
                extensions_loaded: false,
            }),
            assets,
        };

        // We're loading the Zed default theme, as we need a theme to be loaded
        // for tests.
        registry.insert_bundled_theme_families([crate::fallback_themes::zed_default_themes()]);

        let default_icon_theme = crate::default_icon_theme();
        registry
            .state
            .write()
            .icon_themes
            .insert(default_icon_theme.name.clone(), default_icon_theme);
        if let Err(error) = registry.load_bundled_icon_themes() {
            log::error!("failed to load bundled icon themes: {error:#}");
        }

        registry
    }

    fn load_bundled_icon_themes(&self) -> Result<()> {
        for path in BUNDLED_ICON_THEME_PATHS {
            let Some(bytes) = self.assets.load(path)? else {
                continue;
            };
            let family: IconThemeFamilyContent = serde_json::from_slice(&bytes)?;
            self.load_icon_theme(family, Path::new(""))?;
        }
        Ok(())
    }

    /// Returns whether the extensions have been loaded.
    pub fn extensions_loaded(&self) -> bool {
        self.state.read().extensions_loaded
    }

    /// Sets the flag indicating that the extensions have loaded.
    pub fn set_extensions_loaded(&self) {
        self.state.write().extensions_loaded = true;
    }

    /// Inserts the given theme families into the registry.
    pub fn insert_theme_families(&self, families: impl IntoIterator<Item = ThemeFamily>) {
        for family in families.into_iter() {
            self.insert_themes(family.themes);
        }
    }

    /// Inserts theme families that ship with Wu. Removing a user or extension theme with the same
    /// name restores the bundled one.
    pub fn insert_bundled_theme_families(&self, families: impl IntoIterator<Item = ThemeFamily>) {
        let mut state = self.state.write();
        for theme in families.into_iter().flat_map(|family| family.themes) {
            let theme = Arc::new(theme);
            state.themes.insert(theme.name.clone(), theme.clone());
            state.bundled_themes.insert(theme.name.clone(), theme);
        }
    }

    /// Registers theme families for use in tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn register_test_themes(&self, families: impl IntoIterator<Item = ThemeFamily>) {
        self.insert_theme_families(families);
    }

    /// Registers icon themes for use in tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn register_test_icon_themes(&self, icon_themes: impl IntoIterator<Item = IconTheme>) {
        let mut state = self.state.write();
        for icon_theme in icon_themes {
            state
                .icon_themes
                .insert(icon_theme.name.clone(), Arc::new(icon_theme));
        }
    }

    /// Inserts the given themes into the registry.
    pub fn insert_themes(&self, themes: impl IntoIterator<Item = Theme>) {
        let mut state = self.state.write();
        for theme in themes.into_iter() {
            state.themes.insert(theme.name.clone(), Arc::new(theme));
        }
    }

    /// Removes the themes with the given names from the registry.
    pub fn remove_user_themes(&self, themes_to_remove: &[SharedString]) {
        let mut state = self.state.write();
        let state = &mut *state;
        for name in themes_to_remove {
            match state.bundled_themes.get(name) {
                Some(bundled_theme) => {
                    state.themes.insert(name.clone(), bundled_theme.clone());
                }
                None => {
                    state.themes.remove(name);
                }
            }
        }
    }

    /// Returns the bundled default dark theme, which is always available.
    pub fn fallback_theme(&self) -> Arc<Theme> {
        self.state
            .read()
            .bundled_themes
            .get(crate::DEFAULT_DARK_THEME)
            .cloned()
            .unwrap_or_else(|| Arc::new(crate::fallback_themes::zed_default_dark()))
    }

    /// Removes all themes from the registry.
    pub fn clear(&self) {
        self.state.write().themes.clear();
    }

    /// Returns the names of all themes in the registry.
    pub fn list_names(&self) -> Vec<SharedString> {
        let mut names = self.state.read().themes.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }

    /// Returns the metadata of all themes in the registry.
    pub fn list(&self) -> Vec<ThemeMeta> {
        self.state
            .read()
            .themes
            .values()
            .map(|theme| ThemeMeta {
                name: theme.name.clone(),
                appearance: theme.appearance(),
            })
            .collect()
    }

    /// Returns the theme with the given name.
    pub fn get(&self, name: &str) -> Result<Arc<Theme>, ThemeNotFoundError> {
        self.state
            .read()
            .themes
            .get(name)
            .ok_or_else(|| ThemeNotFoundError(name.to_string().into()))
            .cloned()
    }

    /// Returns the default icon theme.
    pub fn default_icon_theme(&self) -> Result<Arc<IconTheme>, IconThemeNotFoundError> {
        self.get_icon_theme(DEFAULT_ICON_THEME_NAME)
    }

    /// Returns the metadata of all icon themes in the registry.
    pub fn list_icon_themes(&self) -> Vec<ThemeMeta> {
        self.state
            .read()
            .icon_themes
            .values()
            .map(|theme| ThemeMeta {
                name: theme.name.clone(),
                appearance: theme.appearance,
            })
            .collect()
    }

    /// Returns the icon theme with the specified name.
    pub fn get_icon_theme(&self, name: &str) -> Result<Arc<IconTheme>, IconThemeNotFoundError> {
        self.state
            .read()
            .icon_themes
            .get(name)
            .ok_or_else(|| IconThemeNotFoundError(name.to_string().into()))
            .cloned()
    }

    /// Removes the icon themes with the given names from the registry.
    pub fn remove_icon_themes(&self, icon_themes_to_remove: &[SharedString]) {
        self.state
            .write()
            .icon_themes
            .retain(|name, _| !icon_themes_to_remove.contains(name));
        if icon_themes_to_remove
            .iter()
            .any(|name| name == MATERIAL_ICON_THEME_NAME || name == MATERIAL_ICON_THEME_LIGHT_NAME)
            && let Err(error) = self.load_bundled_icon_themes()
        {
            log::error!("failed to reload bundled icon themes: {error:#}");
        }
    }

    /// Loads the icon theme from the icon theme family and adds it to the registry.
    ///
    /// The `icons_root_dir` parameter indicates the root directory from which
    /// the relative paths to icons in the theme should be resolved against.
    pub fn load_icon_theme(
        &self,
        icon_theme_family: IconThemeFamilyContent,
        icons_root_dir: &Path,
    ) -> Result<()> {
        let resolve_icon_path = |path: SharedString| {
            icons_root_dir
                .join(path.as_ref())
                .to_string_lossy()
                .to_string()
                .into()
        };

        let default_icon_theme = default_icon_theme();

        let mut state = self.state.write();
        for icon_theme in icon_theme_family.themes {
            let mut file_stems = default_icon_theme.file_stems.clone();
            file_stems.extend(icon_theme.file_stems);

            let mut file_suffixes = default_icon_theme.file_suffixes.clone();
            file_suffixes.extend(icon_theme.file_suffixes);

            let mut named_directory_icons = default_icon_theme.named_directory_icons.clone();
            named_directory_icons.extend(icon_theme.named_directory_icons.into_iter().map(
                |(key, value)| {
                    (
                        key,
                        DirectoryIcons {
                            collapsed: value.collapsed.map(resolve_icon_path),
                            expanded: value.expanded.map(resolve_icon_path),
                        },
                    )
                },
            ));

            let icon_theme = IconTheme {
                id: uuid::Uuid::new_v4().to_string(),
                name: icon_theme.name.into(),
                appearance: match icon_theme.appearance {
                    AppearanceContent::Light => Appearance::Light,
                    AppearanceContent::Dark => Appearance::Dark,
                },
                directory_icons: DirectoryIcons {
                    collapsed: icon_theme.directory_icons.collapsed.map(resolve_icon_path),
                    expanded: icon_theme.directory_icons.expanded.map(resolve_icon_path),
                },
                named_directory_icons,
                chevron_icons: ChevronIcons {
                    collapsed: icon_theme.chevron_icons.collapsed.map(resolve_icon_path),
                    expanded: icon_theme.chevron_icons.expanded.map(resolve_icon_path),
                },
                file_stems,
                file_suffixes,
                file_icons: icon_theme
                    .file_icons
                    .into_iter()
                    .map(|(key, icon)| {
                        (
                            key,
                            IconDefinition {
                                path: resolve_icon_path(icon.path),
                            },
                        )
                    })
                    .collect(),
            };

            state
                .icon_themes
                .insert(icon_theme.name.clone(), Arc::new(icon_theme));
        }

        Ok(())
    }
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::new(Box::new(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removing_an_extension_theme_restores_the_bundled_theme_with_that_name() {
        let registry = ThemeRegistry::new(Box::new(()));
        let bundled_id = registry.get(crate::DEFAULT_DARK_THEME).unwrap().id.clone();

        let mut extension_theme = crate::fallback_themes::zed_default_dark();
        extension_theme.id = "extension".to_string();
        let mut other_extension_theme = crate::fallback_themes::zed_default_dark();
        other_extension_theme.id = "other".to_string();
        other_extension_theme.name = "Extension Only".into();
        registry.insert_themes([extension_theme, other_extension_theme]);
        assert_eq!(
            registry.get(crate::DEFAULT_DARK_THEME).unwrap().id,
            "extension"
        );

        registry.remove_user_themes(&[crate::DEFAULT_DARK_THEME.into(), "Extension Only".into()]);
        assert_eq!(
            registry.get(crate::DEFAULT_DARK_THEME).unwrap().id,
            bundled_id
        );
        assert!(registry.get("Extension Only").is_err());
        assert_eq!(registry.fallback_theme().id, bundled_id);
    }

    #[test]
    fn bundled_icon_themes_reference_existing_assets() {
        let registry = ThemeRegistry::new(Box::new(assets::Assets));
        for name in [MATERIAL_ICON_THEME_NAME, MATERIAL_ICON_THEME_LIGHT_NAME] {
            let icon_theme = registry.get_icon_theme(name).unwrap();
            assert_icon_paths_exist(&registry, &icon_theme);
        }
    }

    fn assert_icon_paths_exist(registry: &ThemeRegistry, icon_theme: &IconTheme) {
        let mut paths = Vec::new();
        paths.extend(icon_theme.directory_icons.collapsed.clone());
        paths.extend(icon_theme.directory_icons.expanded.clone());
        paths.extend(icon_theme.chevron_icons.collapsed.clone());
        paths.extend(icon_theme.chevron_icons.expanded.clone());
        for icons in icon_theme.named_directory_icons.values() {
            paths.extend(icons.collapsed.clone());
            paths.extend(icons.expanded.clone());
        }
        paths.extend(icon_theme.file_icons.values().map(|icon| icon.path.clone()));

        for path in paths {
            assert!(
                registry.assets().load(&path).unwrap().is_some(),
                "missing icon asset {path}"
            );
        }
    }
}
