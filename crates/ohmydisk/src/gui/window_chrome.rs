//! Platform window controls and native drag/resize gestures, separate from jobs.
#[cfg(not(target_os = "windows"))]
use iced::widget::Space;
#[cfg(target_os = "windows")]
use iced::{
    Color,
    widget::{button, row, text, tooltip},
};
use iced::{
    Element, Event, Length, Point, Rectangle, Size, Subscription, Task, Theme, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
        widget::{Operation, Tree, tree},
    },
    window,
};
use std::time::{Duration, Instant};

pub(super) const MACOS_TRAFFIC_LIGHT_WIDTH: f32 = 78.0;
const INITIAL_SIZE: Size = Size::new(1370.0, 900.0);
const MINIMUM_SIZE: Size = Size::new(500.0, 480.0);
const RESIZE_EDGE: f32 = 6.0;
const RESIZE_CORNER: f32 = 12.0;
const DOUBLE_PRESS_INTERVAL: Duration = Duration::from_millis(300);
const DOUBLE_PRESS_DISTANCE: f32 = 6.0;

pub(super) fn settings() -> window::Settings {
    let settings = window::Settings {
        size: INITIAL_SIZE,
        min_size: Some(MINIMUM_SIZE),
        resizable: true,
        ..window::Settings::default()
    };
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let mut settings = settings;
    #[cfg(target_os = "windows")]
    {
        settings.decorations = false;
        settings.platform_specific.undecorated_shadow = true;
    }
    #[cfg(target_os = "macos")]
    {
        // Keep native traffic lights, fullscreen and resize handles. Programmatic
        // drag_resize is not supported by winit on macOS.
        settings.decorations = true;
        settings.platform_specific.title_hidden = true;
        settings.platform_specific.titlebar_transparent = true;
        settings.platform_specific.fullsize_content_view = true;
    }
    settings
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Action {
    Drag,
    ToggleMaximize,
    // macOS invokes its native caption controls instead of emitting these.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    Minimize,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    Close,
    Resize(window::Direction),
}

#[derive(Clone, Debug)]
pub(super) enum Message {
    Action(Action),
    InitialWindow(Option<window::Id>),
    WindowEvent(window::Id, window::Event),
    Maximized {
        id: window::Id,
        epoch: u64,
        maximized: bool,
    },
}

pub(super) struct State {
    pub size: Size,
    id: Option<window::Id>,
    maximized: bool,
    focused: bool,
    query_epoch: u64,
    initial_pending: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            size: INITIAL_SIZE,
            id: None,
            maximized: false,
            focused: true,
            query_epoch: 0,
            initial_pending: true,
        }
    }
}
impl State {
    pub fn initial_task() -> Task<Message> {
        window::oldest().map(Message::InitialWindow)
    }

    pub fn subscription() -> Subscription<Message> {
        // window::events uses listen_with, which excludes redraw requests.
        // There is no frame subscription or periodic state polling here.
        window::events().map(|(id, event)| Message::WindowEvent(id, event))
    }

    pub fn is_maximized(&self) -> bool {
        self.maximized
    }

    fn query_maximized(&mut self) -> Task<Message> {
        let Some(id) = self.id else {
            return Task::none();
        };
        self.query_epoch = self.query_epoch.wrapping_add(1);
        let epoch = self.query_epoch;
        window::is_maximized(id).map(move |maximized| Message::Maximized {
            id,
            epoch,
            maximized,
        })
    }

    fn forget_window(&mut self) {
        self.id = None;
        self.maximized = false;
        self.focused = false;
        self.initial_pending = false;
        self.query_epoch = self.query_epoch.wrapping_add(1);
    }

    fn update_size(&mut self, size: Size) {
        // Minimized Windows windows can report 0x0. Retain the last usable
        // viewport for layout until a positive restored size arrives.
        if size.width.is_finite()
            && size.height.is_finite()
            && size.width > 0.0
            && size.height > 0.0
        {
            self.size = size;
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::InitialWindow(id) => {
                if !self.initial_pending {
                    return Task::none();
                }
                // The initial query may beat creation of the first native
                // window. Keep accepting its subsequent focus/resize event.
                let Some(id) = id else {
                    return Task::none();
                };
                self.initial_pending = false;
                if self.id.is_none() {
                    self.id = Some(id);
                }
                self.query_maximized()
            }
            Message::WindowEvent(id, event) => {
                if matches!(event, window::Event::Opened { .. }) {
                    if self.id.is_some_and(|current| current != id) {
                        return Task::none();
                    }
                    self.id = Some(id);
                    self.initial_pending = false;
                } else if self.id.is_none()
                    && self.initial_pending
                    && matches!(
                        event,
                        window::Event::Focused
                            | window::Event::Unfocused
                            | window::Event::Resized(_)
                    )
                {
                    // The initial focus/resize event can arrive before the ID
                    // query. It belongs to our single application window.
                    self.id = Some(id);
                    self.initial_pending = false;
                }
                if self.id != Some(id) {
                    if self.id.is_none() && matches!(event, window::Event::Closed) {
                        self.initial_pending = false;
                    }
                    return Task::none();
                }
                match event {
                    window::Event::Opened { size, .. } | window::Event::Resized(size) => {
                        self.update_size(size);
                        self.query_maximized()
                    }
                    window::Event::Focused => {
                        self.focused = true;
                        self.query_maximized()
                    }
                    window::Event::Unfocused => {
                        self.focused = false;
                        Task::none()
                    }
                    window::Event::Closed => {
                        self.forget_window();
                        Task::none()
                    }
                    window::Event::Rescaled(_) => self.query_maximized(),
                    _ => Task::none(),
                }
            }
            Message::Maximized {
                id,
                epoch,
                maximized,
            } => {
                if self.id == Some(id) && self.query_epoch == epoch {
                    self.maximized = maximized;
                }
                Task::none()
            }
            Message::Action(action) => {
                let Some(id) = self.id else {
                    return Task::none();
                };
                match action {
                    Action::Drag => window::drag(id),
                    Action::ToggleMaximize => {
                        // Query the OS after the task; external Win+arrow/Snap
                        // changes are also observed through resize/focus events.
                        window::toggle_maximize(id).chain(self.query_maximized())
                    }
                    Action::Minimize => window::minimize(id, true),
                    Action::Close => {
                        self.forget_window();
                        window::close(id)
                    }
                    Action::Resize(direction) => {
                        #[cfg(target_os = "windows")]
                        if !self.maximized {
                            return window::drag_resize(id, direction);
                        }
                        let _ = direction;
                        Task::none()
                    }
                }
            }
        }
    }

    pub fn controls(&self) -> Element<'_, Message> {
        #[cfg(target_os = "windows")]
        {
            let caption = |symbol: &'static str,
                           label: &'static str,
                           action,
                           close|
             -> Element<'static, Message> {
                let focused = self.focused;
                tooltip(
                    button(text(symbol).size(15).align_x(iced::Alignment::Center))
                        .width(44)
                        .height(34)
                        .padding(0)
                        .on_press(Message::Action(action))
                        .style(move |_, status| caption_style(status, close, focused)),
                    text(label).size(12),
                    tooltip::Position::Bottom,
                )
                .into()
            };
            row![
                caption("−", "最小化", Action::Minimize, false),
                caption(
                    if self.maximized { "❐" } else { "□" },
                    if self.maximized {
                        "还原"
                    } else {
                        "最大化"
                    },
                    Action::ToggleMaximize,
                    false
                ),
                caption("×", "关闭", Action::Close, true),
            ]
            .spacing(0)
            .into()
        }
        #[cfg(not(target_os = "windows"))]
        Space::new().width(0).height(0).into()
    }
}

#[cfg(target_os = "windows")]
fn caption_style(status: button::Status, close: bool, focused: bool) -> button::Style {
    let background = match (status, close) {
        (button::Status::Hovered, true) => Some(Color::from_rgb(0.76, 0.16, 0.20).into()),
        (button::Status::Pressed, true) => Some(Color::from_rgb(0.57, 0.11, 0.14).into()),
        (button::Status::Hovered, false) => Some(Color::from_rgb(0.17, 0.19, 0.25).into()),
        (button::Status::Pressed, false) => Some(Color::from_rgb(0.12, 0.14, 0.20).into()),
        _ => None,
    };
    button::Style {
        background,
        text_color: if focused {
            Color::from_rgb(0.91, 0.93, 0.98)
        } else {
            Color::from_rgb(0.59, 0.63, 0.72)
        },
        ..button::Style::default()
    }
}

#[derive(Default)]
struct TitleGesture {
    previous: Option<(Point, Instant)>,
}
impl TitleGesture {
    fn press(&mut self, position: Point, now: Instant) -> Action {
        let double = self.previous.is_some_and(|(point, at)| {
            now.checked_duration_since(at)
                .is_some_and(|elapsed| elapsed <= DOUBLE_PRESS_INTERVAL)
                && point.distance(position) < DOUBLE_PRESS_DISTANCE
        });
        self.previous = (!double).then_some((position, now));
        if double {
            Action::ToggleMaximize
        } else {
            Action::Drag
        }
    }
    fn window_event(&mut self, event: &window::Event) {
        if matches!(
            event,
            window::Event::Moved(_) | window::Event::Resized(_) | window::Event::Unfocused
        ) {
            self.previous = None;
        }
    }
}

fn resize_direction(size: Size, position: Point, maximized: bool) -> Option<window::Direction> {
    if maximized
        || !size.width.is_finite()
        || !size.height.is_finite()
        || position.x < 0.0
        || position.y < 0.0
        || position.x >= size.width
        || position.y >= size.height
    {
        return None;
    }
    let left = position.x < RESIZE_EDGE;
    let right = position.x >= size.width - RESIZE_EDGE;
    let top = position.y < RESIZE_EDGE;
    let bottom = position.y >= size.height - RESIZE_EDGE;
    let near_left = position.x < RESIZE_CORNER;
    let near_right = position.x >= size.width - RESIZE_CORNER;
    let near_top = position.y < RESIZE_CORNER;
    let near_bottom = position.y >= size.height - RESIZE_CORNER;
    match (left, right, top, bottom) {
        _ if (left && near_top) || (top && near_left) => Some(window::Direction::NorthWest),
        _ if (right && near_top) || (top && near_right) => Some(window::Direction::NorthEast),
        _ if (left && near_bottom) || (bottom && near_left) => Some(window::Direction::SouthWest),
        _ if (right && near_bottom) || (bottom && near_right) => Some(window::Direction::SouthEast),
        (true, _, _, _) => Some(window::Direction::West),
        (_, true, _, _) => Some(window::Direction::East),
        (_, _, true, _) => Some(window::Direction::North),
        (_, _, _, true) => Some(window::Direction::South),
        _ => None,
    }
}

fn resize_cursor(direction: window::Direction) -> mouse::Interaction {
    match direction {
        window::Direction::North | window::Direction::South => {
            mouse::Interaction::ResizingVertically
        }
        window::Direction::East | window::Direction::West => {
            mouse::Interaction::ResizingHorizontally
        }
        window::Direction::NorthEast | window::Direction::SouthWest => {
            mouse::Interaction::ResizingDiagonallyUp
        }
        window::Direction::NorthWest | window::Direction::SouthEast => {
            mouse::Interaction::ResizingDiagonallyDown
        }
    }
}

pub(super) fn drag_area<'a, M: Clone + 'a>(
    content: Element<'a, M>,
    on_action: impl Fn(Action) -> M + 'a,
) -> Element<'a, M> {
    Element::new(ChromeArea {
        content,
        on_action: Box::new(on_action),
        kind: AreaKind::Drag,
    })
}

pub(super) fn frame<'a, M: Clone + 'a>(
    content: Element<'a, M>,
    maximized: bool,
    on_action: impl Fn(Action) -> M + 'a,
) -> Element<'a, M> {
    if cfg!(target_os = "windows") {
        Element::new(ChromeArea {
            content,
            on_action: Box::new(on_action),
            kind: AreaKind::Resize { maximized },
        })
    } else {
        content
    }
}

#[derive(Clone, Copy)]
enum AreaKind {
    Drag,
    Resize { maximized: bool },
}
struct ChromeArea<'a, M> {
    content: Element<'a, M>,
    on_action: Box<dyn Fn(Action) -> M + 'a>,
    kind: AreaKind,
}
impl<M: Clone> Widget<M, Theme, iced::Renderer> for ChromeArea<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<TitleGesture>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(TitleGesture::default())
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.content.as_widget())]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let gesture = tree.state.downcast_mut::<TitleGesture>();
        if let Event::Window(event) = event {
            gesture.window_event(event);
        }
        // Window-frame edges take precedence over content at the outer pixels.
        if let AreaKind::Resize { maximized } = self.kind
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            )
            && let Some(point) = cursor.position_in(layout.bounds())
            && let Some(direction) = resize_direction(layout.bounds().size(), point, maximized)
        {
            shell.publish((self.on_action)(Action::Resize(direction)));
            shell.capture_event();
            return;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if shell.is_event_captured() {
            return;
        }
        if let AreaKind::Drag = self.kind
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            )
            && let Some(point) = cursor.position_in(layout.bounds())
        {
            let action = tree
                .state
                .downcast_mut::<TitleGesture>()
                .press(point, Instant::now());
            shell.publish((self.on_action)(action));
            shell.capture_event();
        }
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if let AreaKind::Resize { maximized } = self.kind
            && let Some(point) = cursor.position_in(layout.bounds())
            && let Some(direction) = resize_direction(layout.bounds().size(), point, maximized)
        {
            return resize_cursor(direction);
        }
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn second_nearby_press_only_toggles_and_move_resets_the_candidate() {
        let now = Instant::now();
        let mut gesture = TitleGesture::default();
        assert!(matches!(
            gesture.press(Point::new(80.0, 20.0), now),
            Action::Drag
        ));
        assert!(matches!(
            gesture.press(Point::new(82.0, 21.0), now + Duration::from_millis(100)),
            Action::ToggleMaximize
        ));
        assert!(gesture.previous.is_none());
        assert!(matches!(
            gesture.press(Point::new(82.0, 21.0), now + Duration::from_millis(150)),
            Action::Drag
        ));
        gesture.window_event(&window::Event::Moved(Point::new(200.0, 200.0)));
        assert!(matches!(
            gesture.press(Point::new(82.0, 21.0), now + Duration::from_millis(200)),
            Action::Drag
        ));
    }
    #[test]
    fn distant_expired_and_unfocused_presses_start_fresh_drags() {
        let now = Instant::now();
        let mut gesture = TitleGesture::default();
        let _ = gesture.press(Point::ORIGIN, now);
        assert!(matches!(
            gesture.press(Point::new(40.0, 0.0), now + Duration::from_millis(10)),
            Action::Drag
        ));
        assert!(matches!(
            gesture.press(Point::new(40.0, 0.0), now + Duration::from_secs(1)),
            Action::Drag
        ));
        gesture.window_event(&window::Event::Unfocused);
        assert!(matches!(
            gesture.press(Point::new(40.0, 0.0), now + Duration::from_millis(1010)),
            Action::Drag
        ));
    }
    #[test]
    fn edges_corners_interior_and_maximization_have_distinct_resize_behavior() {
        let size = Size::new(500.0, 480.0);
        for (point, expected, cursor) in [
            (
                Point::new(1.0, 1.0),
                window::Direction::NorthWest,
                mouse::Interaction::ResizingDiagonallyDown,
            ),
            (
                Point::new(499.0, 1.0),
                window::Direction::NorthEast,
                mouse::Interaction::ResizingDiagonallyUp,
            ),
            (
                Point::new(1.0, 479.0),
                window::Direction::SouthWest,
                mouse::Interaction::ResizingDiagonallyUp,
            ),
            (
                Point::new(499.0, 479.0),
                window::Direction::SouthEast,
                mouse::Interaction::ResizingDiagonallyDown,
            ),
            (
                Point::new(250.0, 1.0),
                window::Direction::North,
                mouse::Interaction::ResizingVertically,
            ),
            (
                Point::new(250.0, 479.0),
                window::Direction::South,
                mouse::Interaction::ResizingVertically,
            ),
            (
                Point::new(1.0, 240.0),
                window::Direction::West,
                mouse::Interaction::ResizingHorizontally,
            ),
            (
                Point::new(499.0, 240.0),
                window::Direction::East,
                mouse::Interaction::ResizingHorizontally,
            ),
        ] {
            let actual = resize_direction(size, point, false).unwrap();
            assert_eq!(
                std::mem::discriminant(&actual),
                std::mem::discriminant(&expected)
            );
            assert_eq!(resize_cursor(actual), cursor);
            assert!(resize_direction(size, point, true).is_none());
        }
        for point in [
            Point::new(200.0, 200.0),
            Point::new(-1.0, 20.0),
            Point::new(500.0, 20.0),
        ] {
            assert!(resize_direction(size, point, false).is_none());
        }
    }
    #[test]
    fn stale_queries_cannot_change_replaced_or_closed_window_state() {
        let id = window::Id::unique();
        let other = window::Id::unique();
        let mut state = State {
            id: Some(id),
            query_epoch: 4,
            initial_pending: false,
            ..State::default()
        };
        let _ = state.update(Message::Maximized {
            id,
            epoch: 3,
            maximized: true,
        });
        assert!(!state.is_maximized());
        let _ = state.update(Message::Maximized {
            id: other,
            epoch: 4,
            maximized: true,
        });
        assert!(!state.is_maximized());
        let _ = state.update(Message::Maximized {
            id,
            epoch: 4,
            maximized: true,
        });
        assert!(state.is_maximized());
        let _ = state.update(Message::WindowEvent(id, window::Event::Closed));
        let _ = state.update(Message::Maximized {
            id,
            epoch: 4,
            maximized: true,
        });
        let _ = state.update(Message::InitialWindow(Some(id)));
        assert!(state.id.is_none());
        assert!(!state.is_maximized());
        let _ = state.update(Message::WindowEvent(
            other,
            window::Event::Opened {
                position: None,
                size: Size::new(800.0, 600.0),
            },
        ));
        let epoch = state.query_epoch;
        let _ = state.update(Message::Maximized {
            id,
            epoch,
            maximized: true,
        });
        assert_eq!(state.id, Some(other));
        assert!(!state.is_maximized());
        assert_eq!(state.size, Size::new(800.0, 600.0));
    }
    #[test]
    fn external_resize_updates_viewport_but_minimized_zero_does_not_erase_it() {
        let id = window::Id::unique();
        let mut state = State::default();
        let _ = state.update(Message::WindowEvent(
            id,
            window::Event::Resized(Size::new(500.0, 480.0)),
        ));
        let epoch = state.query_epoch;
        let _ = state.update(Message::Maximized {
            id,
            epoch,
            maximized: true,
        });
        assert!(state.is_maximized());
        let _ = state.update(Message::WindowEvent(id, window::Event::Resized(Size::ZERO)));
        assert_eq!(state.size, MINIMUM_SIZE);
        let _ = state.update(Message::Maximized {
            id,
            epoch,
            maximized: false,
        });
        assert!(
            state.is_maximized(),
            "the preceding resize invalidated this old reply"
        );
    }

    #[test]
    fn an_early_empty_id_query_does_not_disable_later_native_window_events() {
        let id = window::Id::unique();
        let mut state = State::default();
        let _ = state.update(Message::InitialWindow(None));
        assert!(state.id.is_none());
        let _ = state.update(Message::WindowEvent(id, window::Event::Focused));
        assert_eq!(state.id, Some(id));
        let epoch = state.query_epoch;
        let _ = state.update(Message::Maximized {
            id,
            epoch,
            maximized: true,
        });
        assert!(state.is_maximized());
    }
}
