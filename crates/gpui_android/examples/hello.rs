//! A GPUI window on Android (phone-android-tessera task 2.5): text, a scrolling list, and a button
//! a screen reader names. Built as `libhello.so`, loaded by the GameActivity in
//! crates/gpui_android/android (see its README).

#[cfg(target_os = "android")]
mod app {
    use gpui::{
        App, Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
        StatefulInteractiveElement, Styled, Window, WindowOptions, div, prelude::*, px, rgb,
    };

    struct Hello {
        taps: usize,
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
            cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| Hello { taps: 0 }))
                .expect("could not open the window");
            cx.activate(true);
        });
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("gpui"));
    gpui_android::set_android_app(app);
    app::run();
}

#[cfg(not(target_os = "android"))]
fn main() {}
