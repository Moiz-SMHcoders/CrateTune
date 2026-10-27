use gpui::{Action, App, Menu, MenuItem, SharedString};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MenuPlatform {
    MacOS,
    NonMacOS,
    All,
}

impl MenuPlatform {
    fn is_active(self) -> bool {
        match self {
            Self::MacOS => cfg!(target_os = "macos"),
            Self::NonMacOS => !cfg!(target_os = "macos"),
            Self::All => true,
        }
    }
}

pub struct MenusBuilder {
    menus: Vec<Menu>,
}

/// Builds a `Vec` of GPUI `Menu`s.
///
/// This file (and everything in it) is related to building **GPUI** application-level menus. There
/// is no relation between `menus_builder` and `menu`, though the menus built by this builder are
/// used to populate the application's menu bar.
impl MenusBuilder {
    pub fn new() -> Self {
        Self { menus: Vec::new() }
    }

    pub fn add_menu(mut self, builder: MenuBuilder) -> Self {
        if let Some(menu) = builder.build() {
            self.menus.push(menu);
        }
        self
    }

    pub fn set(self, cx: &mut App) {
        cx.set_menus(self.menus);
    }
}

/// Builds an individual GPUI `Menu`.
///
/// This file (and everything in it) is related to building **GPUI** application-level menus. There
/// is no relation between `menus_builder` and `menu`, though the menus built by this builder are
/// used to populate the application's menu bar.
pub struct MenuBuilder {
    name: SharedString,
    items: Vec<MenuItem>,
    platform: MenuPlatform,
    disabled: bool,
}

impl MenuBuilder {
    pub fn new(name: impl Into<SharedString>) -> Self {
        Self {
            name: name.into(),
            items: Vec::new(),
            platform: MenuPlatform::All,
            disabled: false,
        }
    }

    pub fn platform(mut self, platform: MenuPlatform) -> Self {
        self.platform = platform;
        self
    }

    pub fn add_item(mut self, item: impl Into<Option<MenuItem>>) -> Self {
        if let Some(item) = item.into() {
            self.items.push(item);
        }
        self
    }

    pub fn build(self) -> Option<Menu> {
        if !self.platform.is_active() {
            return None;
        }

        Some(Menu {
            name: self.name,
            items: self.items,
            disabled: self.disabled,
        })
    }
}

/// Creates a single GPUI `MenuItem`, unless the platform check fails.
///
/// This file (and everything in it) is related to building **GPUI** application-level menus. There
/// is no relation between `menus_builder` and `menu`, though the menus built by this builder are
/// used to populate the application's menu bar.
pub fn menu_item<A: Action>(
    name: impl Into<SharedString>,
    action: A,
    platform: MenuPlatform,
) -> Option<MenuItem> {
    if !platform.is_active() {
        return None;
    }

    Some(MenuItem::action(name, action))
}

/// Creates a single GPUI `MenuItem` separator, unless the platform check fails.
///
/// This file (and everything in it) is related to building **GPUI** application-level menus. There
/// is no relation between `menus_builder` and `menu`, though the menus built by this builder are
/// used to populate the application's menu bar.
pub fn menu_separator(platform: MenuPlatform) -> Option<MenuItem> {
    if !platform.is_active() {
        return None;
    }

    Some(MenuItem::separator())
}
