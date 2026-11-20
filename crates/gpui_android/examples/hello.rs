//! A GPUI window on Android (phone-android-tessera task 2.5): text, a field typed into with the
//! soft keyboard, a scrolling list, and a button a screen reader names. Built as `libhello.so`, loaded by the GameActivity in
//! crates/gpui_android/android (see its README).

// The field is GPUI's example editor, which binds these.
#[cfg(target_os = "android")]
gpui::actions!(hello, [Backspace, Delete, Left, Right, Home, End]);

#[cfg(target_os = "android")]
#[path = "../../gpui/examples/view_example/example_editor.rs"]
#[allow(dead_code)]
mod example_editor;

#[cfg(target_os = "android")]
mod app {
    use crate::example_editor::{Editor, standard_actions};
    use crate::{Backspace, Delete, End, Home, Left, Right};
    use gpui::{
        App, Context, Entity, InteractiveElement, IntoElement, KeyBinding, ParentElement, Render,
        SharedString, StatefulInteractiveElement, Styled, Window, WindowOptions, div, prelude::*,
        px, rgb,
    };

    struct Hello {
        taps: usize,
        field: Entity<Editor>,
    }

    impl Render for Hello {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let insets = window.insets();
            div()
                .size_full()
                .bg(rgb(0xffffff))
                .text_color(rgb(0x1f2328))
                .pt(insets.safe_area.top)
                .pb(insets.safe_area.bottom)
                .flex()
                .flex_col()
                .child(div().p_4().text_2xl().child("Hello from GPUI on Android"))
                .child({
                    let focus = self.field.read(cx).focus_handle.clone();
                    let focused = focus.is_focused(window);
                    div()
                        .id("field")
                        .key_context("TextInput")
                        .track_focus(&focus)
                        .map(standard_actions(self.field.clone()))
                        .on_click(move |_, window, cx| window.focus(&focus, cx))
                        .mx_4()
                        .mb_4()
                        .h(px(44.))
                        .px_3()
                        .flex()
                        .items_center()
                        .border_1()
                        .rounded_md()
                        .border_color(rgb(if focused { 0x0969da } else { 0xd0d7de }))
                        .child(self.field.clone())
                })
                .child(
                    div()
                        .id("tap")
                        .mx_4()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(0x0969da))
                        .text_color(rgb(0xffffff))
                        .child(SharedString::from(format!("Tapped {} times", self.taps)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.taps += 1;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("rows")
                        .flex_1()
                        .mt_4()
                        .overflow_y_scroll()
                        .children((0..200).map(|i| {
                            div()
                                .px_4()
                                .py_2()
                                .border_b_1()
                                .border_color(rgb(0xd0d7de))
                                .child(SharedString::from(format!("Row {i}: the quick brown fox")))
                        })),
                )
                .text_size(px(16.))
        }
    }

    pub fn run() {
        gpui_platform::application().run(|cx: &mut App| {
            cx.bind_keys([
                KeyBinding::new("backspace", Backspace, None),
                KeyBinding::new("delete", Delete, None),
                KeyBinding::new("left", Left, None),
                KeyBinding::new("right", Right, None),
                KeyBinding::new("home", Home, None),
                KeyBinding::new("end", End, None),
            ]);
            cx.open_window(WindowOptions::default(), |window, cx| {
                let field = cx.new(|cx| Editor::new("", window, cx));
                cx.new(|_| Hello { taps: 0, field })
            })
                .expect("could not open the window");
            cx.activate(true);
        });
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    // android_activity calls each poll that ran Choreographer's vsync callback "spurious", at error,
    // once a frame: its activity_impl is kept quiet.
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_filter(android_logger::FilterBuilder::new().parse("info,android_activity::activity_impl=off").build())
            .with_tag("gpui"),
    );
    gpui_android::set_android_app(app);
    app::run();
}

#[cfg(not(target_os = "android"))]
fn main() {}
