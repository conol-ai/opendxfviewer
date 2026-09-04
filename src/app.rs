use makepad_widgets::*;

live_design! {
    use link::theme::*;
    use link::shaders::*;
    use link::widgets::*;

    App = {{App}} {
        ui: <Root> {
            main_window = <Window> {
                window: { title: "opendxfviewer" },
                body = <View> {
                    flow: Down,
                    show_bg: true,
                    draw_bg: { color: #1e1e1e }
                    <Label> { text: "opendxfviewer", draw_text: { color: #ddd } }
                    open_btn = <Button> { text: "Open DXF…" }
                }
            }
        }
    }
}

app_main!(App);

#[derive(Live, LiveHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
}

impl LiveRegister for App {
    fn live_register(cx: &mut Cx) {
        crate::makepad_widgets::live_design(cx);
    }
}

impl MatchEvent for App {
    fn handle_actions(&mut self, _cx: &mut Cx, actions: &Actions) {
        if self.ui.button(id!(open_btn)).clicked(actions) {
            log!("open clicked");
        }
    }
}

impl AppMain for App {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
