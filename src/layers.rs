//! The layer panel.
//!
//! A `PortalList` rather than a plain stack of views: a DXF file can carry hundreds of layers, and
//! the list only builds rows for the ones actually on screen.

use makepad_widgets::*;

use crate::scene::Rgb;

live_design! {
    use link::theme::*;
    use link::shaders::*;
    use link::widgets::*;

    LayerRow = <View> {
        width: Fill, height: 22.0,
        flow: Right, align: { x: 0.0, y: 0.5 },
        spacing: 6.0,
        padding: { left: 8.0, right: 8.0 }

        vis = <CheckBox> { width: Fit, text: "" }
        // The swatch shows the colour as it will appear on the sheet, which on a light sheet can
        // be dark. The border keeps it visible against this panel either way.
        swatch = <RoundedView> {
            width: 11.0, height: 11.0,
            show_bg: true,
            draw_bg: {
                color: #fff,
                border_radius: 2.0,
                border_size: 1.0,
                border_color: #5a5a62
            }
        }
        name = <Label> {
            width: Fill,
            draw_text: { color: #c8c8d0, text_style: { font_size: 9.0 } }
        }
        count = <Label> {
            width: Fit,
            draw_text: { color: #6e6e78, text_style: { font_size: 8.0 } }
        }
    }

    pub LayerPanelBase = {{LayerPanel}} {}

    pub LayerPanel = <LayerPanelBase> {
        width: Fill, height: Fill,
        flow: Down,
        show_bg: true,
        draw_bg: { color: #1a1a1e }

        <View> {
            width: Fill, height: 26.0,
            flow: Right, align: { y: 0.5 },
            padding: { left: 8.0, right: 8.0 },
            spacing: 6.0
            <Label> {
                width: Fill,
                text: "Layers",
                draw_text: { color: #85858f, text_style: { font_size: 9.0 } }
            }
            all_btn = <Button> {
                text: "All",
                padding: { left: 7.0, right: 7.0, top: 2.0, bottom: 2.0 }
                draw_text: { text_style: { font_size: 8.0 } }
            }
        }

        list = <PortalList> {
            width: Fill, height: Fill,
            Row = <LayerRow> {}
        }
    }
}

/// One row's worth of data, snapshotted from the scene so the panel never borrows it.
#[derive(Clone, Debug, Default)]
pub struct LayerItem {
    pub name: String,
    pub color: Rgb,
    pub count: u32,
    pub visible: bool,
    /// False when the file itself switched the layer off; the row is shown but disabled.
    pub on_in_file: bool,
}

/// The panel's data, installed into the [`Scope`] by the app.
#[derive(Clone, Debug, Default)]
pub struct LayerList(pub Vec<LayerItem>);

#[derive(Clone, Debug, DefaultNone)]
pub enum LayerPanelAction {
    /// One layer's checkbox changed.
    SetVisible(usize, bool),
    ShowAll,
    None,
}

#[derive(Live, LiveHook, Widget)]
pub struct LayerPanel {
    #[deref]
    view: View,
}

impl Widget for LayerPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // WidgetMatchEvent is not dispatched for us; without this the checkboxes do nothing.
        self.widget_match_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let layers = scope.data.get::<LayerList>().cloned().unwrap_or_default();
        while let Some(next) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = next.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else { continue };
            list.set_item_range(cx, 0, layers.0.len());
            while let Some(row) = list.next_visible_item(cx) {
                let Some(l) = layers.0.get(row) else { continue };
                // `live_id!`, not `id!`: a PortalList template is addressed by a single id.
                let item = list.item(cx, row, live_id!(Row));
                item.label(id!(name)).set_text(cx, &l.name);
                item.label(id!(count)).set_text(cx, &l.count.to_string());
                // CheckBoxRef::set_text is the one setter that does not take cx, so the row's
                // label lives on a sibling Label instead.
                item.check_box(id!(vis)).set_active(cx, l.visible && l.on_in_file);
                let c = if l.on_in_file { l.color } else { Rgb(0x50, 0x50, 0x58) };
                item.view(id!(swatch)).apply_over(
                    cx,
                    live! { draw_bg: { color: (vec4(
                        c.0 as f32 / 255.0, c.1 as f32 / 255.0, c.2 as f32 / 255.0, 1.0
                    )) } },
                );
                item.draw_all(cx, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }
}

impl WidgetMatchEvent for LayerPanel {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, scope: &mut Scope) {
        let uid = self.widget_uid();
        if self.button(id!(all_btn)).clicked(actions) {
            cx.widget_action(uid, &scope.path, LayerPanelAction::ShowAll);
        }
        let list = self.portal_list(id!(list));
        for (row, item) in list.items_with_actions(actions) {
            if let Some(on) = item.check_box(id!(vis)).changed(actions) {
                cx.widget_action(uid, &scope.path, LayerPanelAction::SetVisible(row, on));
            }
        }
    }
}
