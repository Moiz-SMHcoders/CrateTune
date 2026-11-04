//! Script actions for the per-view elements (see `script.rs` for the runner;
//! `settings/np_elements.rs` for the keys):
//!
//! * `@np-elements=DESIGN:like=on;shuffle=off` — switch elements of a Now Playing
//!   design (`split`, …, `turntable`, or `current`); `;` separates them because
//!   `,` separates script items. `DESIGN:all|none|default` shows everything the
//!   design offers, only what can't be hidden, or its defaults. Saved.
//! * `@np-elements-reset=DESIGN|all` — back to the defaults.
//! * `@mini-elements=LAYOUT:…`, `@mini-elements-reset=LAYOUT|all` — the same for
//!   the mini player's layouts (`bar`, `compact`, `card`, `lyrics`, `turntable`).
//! * `@np-elements-status` — prints what every view shows.
//! * `@np-customize[=off]` — Now Playing's "Customize this view" panel.
//! * `@mini-menu` — right-clicks the mini player (its elements menu).
//! * `@np-bounds` — checks the parts of Now Playing and the mini player on screen
//!   for overlaps and for leaving the window (`np_custom.rs`).

use gpui::{
    Context, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput, Window, point, px,
};

use crate::app::MusicApp;
use crate::settings::np_elements::{ElementView, Elements, NpElement};
use crate::settings::prefs::{MiniPlayerSkin, NowPlayingLayout};

impl MusicApp {
    /// Returns `false` if `name` isn't one of these actions.
    pub(crate) fn elements_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let np = self.settings.read(cx).settings().now_playing;
        let np_view = |key: &str| match key {
            "current" | "" => Some(ElementView::Np(np.layout)),
            key => NowPlayingLayout::parse(key).map(ElementView::Np),
        };
        let mini_view = |key: &str| match key {
            "current" | "" => Some(ElementView::Mini(np.mini_skin)),
            key => MiniPlayerSkin::parse(key).map(ElementView::Mini),
        };
        match name {
            "np-elements" | "mini-elements" => {
                let (view, changes) = arg.unwrap_or("").split_once(':').unwrap_or(("", ""));
                let view = if name == "np-elements" {
                    np_view(view)
                } else {
                    mini_view(view)
                };
                match view.map(|view| (view, parse_changes(view, np.shown(view), changes))) {
                    Some((view, Ok(shown))) => {
                        self.settings
                            .update(cx, |p, cx| p.set_np_elements(view, shown, cx));
                    }
                    Some((_, Err(bad))) => eprintln!(
                        "music-gpui: @{name}: unknown element {bad:?} (KEY=on|off;…, all, none, default; KEY: {})",
                        NpElement::ALL.map(NpElement::key).join("|")
                    ),
                    None => eprintln!("music-gpui: @{name} wants VIEW:KEY=on|off;…"),
                }
            }
            "np-elements-reset" | "mini-elements-reset" => {
                let key = arg.unwrap_or("current");
                let views: Vec<ElementView> = if key == "all" {
                    ElementView::all()
                        .filter(|v| {
                            matches!(v, ElementView::Np(_)) == (name == "np-elements-reset")
                        })
                        .collect()
                } else if name == "np-elements-reset" {
                    np_view(key).into_iter().collect()
                } else {
                    mini_view(key).into_iter().collect()
                };
                if views.is_empty() {
                    eprintln!("music-gpui: @{name} wants a design / layout key or all");
                }
                self.settings.update(cx, |p, cx| {
                    for view in views {
                        p.reset_np_elements(view, cx);
                    }
                });
            }
            "np-elements-status" => {
                for view in ElementView::all() {
                    let key = match view {
                        ElementView::Np(l) => l.key().to_owned(),
                        ElementView::Mini(s) => format!("mini-{}", s.key()),
                    };
                    eprintln!(
                        "music-gpui: elements {key}: shown [{}] offered [{}] default={}",
                        np.shown(view).keys(),
                        view.offered().keys(),
                        np.elements_default(view)
                    );
                }
            }
            "np-customize" => self.set_np_customize(arg != Some("off"), cx),
            "np-bounds" => self.np_bounds_check(window, cx),
            "mini-menu" => match self.mini {
                Some(handle) => {
                    // A real right-click in the middle of the mini player window.
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                let size = window.viewport_size();
                                let position = point(size.width * 0.5, px(60.));
                                window.dispatch_event(
                                    PlatformInput::MouseDown(MouseDownEvent {
                                        button: MouseButton::Right,
                                        position,
                                        modifiers: Modifiers::default(),
                                        click_count: 1,
                                        first_mouse: false,
                                    }),
                                    cx,
                                );
                                window.dispatch_event(
                                    PlatformInput::MouseUp(MouseUpEvent {
                                        button: MouseButton::Right,
                                        position,
                                        modifiers: Modifiers::default(),
                                        click_count: 1,
                                    }),
                                    cx,
                                );
                                window.refresh();
                            })
                            .ok();
                    });
                }
                None => eprintln!("music-gpui: @mini-menu: no mini player open"),
            },
            _ => return false,
        }
        true
    }
}

/// `like=on;shuffle=off` (or `all`, `none`, `default`) applied to `shown`; the
/// unknown key on failure.
fn parse_changes(view: ElementView, shown: Elements, changes: &str) -> Result<Elements, String> {
    let mut shown = shown;
    for change in changes
        .split([';', '+'])
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        match change {
            "all" => shown = view.offered(),
            "none" => shown = Elements::NONE,
            "default" => shown = view.defaults(),
            _ => {
                let (key, value) = change.split_once('=').unwrap_or((change, "on"));
                let e = NpElement::parse(key).ok_or_else(|| key.to_owned())?;
                shown = shown.with(e, value != "off");
            }
        }
    }
    Ok(shown.and(view.offered()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_parse() {
        let view = ElementView::Np(NowPlayingLayout::Turntable);
        let shown = parse_changes(view, view.defaults(), "like=on;times=off").unwrap();
        assert!(shown.has(NpElement::Like) && !shown.has(NpElement::Times));
        assert_eq!(parse_changes(view, shown, "all").unwrap(), view.offered());
        assert_eq!(
            parse_changes(view, shown, "none;like").unwrap(),
            Elements::of(&[NpElement::Like])
        );
        assert_eq!(
            parse_changes(view, shown, "default").unwrap(),
            view.defaults()
        );
        assert_eq!(parse_changes(view, shown, "laser=on"), Err("laser".into()));
        // Not offered by the layout: dropped.
        let compact = ElementView::Mini(MiniPlayerSkin::Compact);
        assert!(
            !parse_changes(compact, compact.defaults(), "times=on")
                .unwrap()
                .has(NpElement::Times)
        );
    }
}
