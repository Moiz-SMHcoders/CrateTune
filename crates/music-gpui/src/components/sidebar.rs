//! Left navigation: app title, the browse / library entries from
//! [`crate::pages::NAV`], and Settings at the bottom.

use gpui::{
    Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};

use crate::app::MusicApp;
use crate::icons::Lucide;
use crate::pages::{NAV, NavItem};
use crate::router::Route;
use crate::settings::SectionId;
use crate::theme::AppColors;

pub const SIDEBAR_WIDTH: f32 = 216.;

impl MusicApp {
    pub(crate) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let current = self.router.route().clone();
        let theme = cx.theme();
        let colors = AppColors::get(cx);
        let (bg, border, muted, fg, hover, active_bg) = (
            theme.sidebar,
            theme.sidebar_border,
            theme.muted_foreground,
            theme.sidebar_foreground,
            theme.list_hover,
            theme.sidebar_accent,
        );

        let entry =
            |id: SharedString,
             icon: Lucide,
             title: &'static str,
             route: Route,
             cx: &mut Context<Self>| {
                let active = route.same_page(&current);
                h_flex()
                    .id(id)
                    .px_3()
                    .py(px(7.))
                    .gap_3()
                    .rounded(px(8.))
                    .text_sm()
                    .cursor_pointer()
                    .text_color(if active { fg } else { muted })
                    .when(active, |this| {
                        this.bg(active_bg).font_weight(FontWeight::MEDIUM)
                    })
                    .when(!active, |this| {
                        this.hover(move |s| s.bg(hover).text_color(fg))
                    })
                    .child(
                        div()
                            .text_color(if active { colors.accent } else { muted })
                            .child(Icon::new(icon).small()),
                    )
                    .child(title)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate(route.clone(), window, cx)
                    }))
            };
        let group_label = |label: &'static str| {
            div()
                .px_3()
                .pt_4()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(label)
        };

        let mut list = v_flex().gap_0p5();
        for (i, item) in NAV.iter().enumerate() {
            match item {
                NavItem::Group(label) => list = list.child(group_label(label)),
                NavItem::Page { title, icon, route } => {
                    list = list.child(entry(
                        SharedString::from(format!("nav-{i}")),
                        *icon,
                        title,
                        route(),
                        cx,
                    ))
                }
            }
        }

        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .bg(bg)
            .border_r_1()
            .border_color(border)
            .px_2()
            .py_3()
            .child(
                h_flex()
                    .px_3()
                    .pb_2()
                    .gap_2()
                    .child(
                        div()
                            .size(px(26.))
                            .rounded(px(7.))
                            .bg(colors.accent)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(crate::theme::on_color(colors.accent))
                            .child(Icon::new(Lucide::Music).small()),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child(crate::identity::APP_NAME),
                    ),
            )
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            )
            .child(div().pt_2().border_t_1().border_color(border).child(entry(
                "nav-settings".into(),
                Lucide::Settings,
                "Settings",
                Route::Settings {
                    section: match &current {
                        Route::Settings { section } => *section,
                        _ => SectionId::default(),
                    },
                },
                cx,
            )))
    }
}
