//! UI-thread-only drag ownership and the content/collector split. No service work.
use iced::{
    Border, Color, Element, Event as UiEvent, Length, Point, Rectangle, Size, Theme, Vector,
    advanced::{
        Clipboard, Layout, Renderer as _, Shell, Widget, layout, mouse, overlay, renderer,
        text::{self, Renderer as _},
        widget::{Operation, Tree, tree},
    },
    keyboard, window,
};
use ohmydisk_core::Node;
use std::{cell::RefCell, rc::Rc, sync::OnceLock};

pub(super) const COLLAPSED_HEIGHT: f32 = 64.0;
pub(super) const DEFAULT_HEIGHT: f32 = 180.0;
pub(super) const SPLIT_HEIGHT: f32 = 8.0;
pub(super) const CONTENT_RESERVE: f32 = 240.0;
const DRAG_DISTANCE: f32 = 6.0;
const USEFUL_EXPANDED_HEIGHT: f32 = 150.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Scope {
    pub scan_id: String,
    pub epoch: u64,
}
#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub scope: Scope,
    pub node: Node,
}
#[derive(Clone, Debug)]
pub(super) enum Event {
    Dropped(Entry),
    HeightChanged(f32),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PanelState {
    expanded: bool,
    preferred: f32,
    useful_expanded: f32,
}
impl Default for PanelState {
    fn default() -> Self {
        Self {
            expanded: false,
            preferred: DEFAULT_HEIGHT,
            useful_expanded: DEFAULT_HEIGHT,
        }
    }
}
impl PanelState {
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }
    pub fn toggle(&mut self) {
        if self.expanded {
            self.expanded = false;
        } else {
            self.open();
        }
    }
    pub fn open(&mut self) {
        self.expanded = true;
        self.preferred = self.useful_expanded;
    }
    pub fn resize(&mut self, height: f32) {
        if height.is_finite() {
            self.preferred = height.max(COLLAPSED_HEIGHT);
            self.expanded = height > COLLAPSED_HEIGHT;
            if height >= USEFUL_EXPANDED_HEIGHT {
                self.useful_expanded = height;
            }
        }
    }
    /// available is the complete workspace height, including the separator.
    pub fn effective_height(&self, available: f32) -> f32 {
        let maximum = maximum_height(available);
        if self.expanded {
            self.preferred.clamp(COLLAPSED_HEIGHT.min(maximum), maximum)
        } else {
            COLLAPSED_HEIGHT.min(maximum)
        }
    }
}
fn finite_height(height: f32) -> f32 {
    if height.is_finite() {
        height.max(0.0)
    } else {
        0.0
    }
}
fn maximum_height(available: f32) -> f32 {
    let available = finite_height(available);
    let usable = (available - SPLIT_HEIGHT).max(0.0);
    if usable >= CONTENT_RESERVE + COLLAPSED_HEIGHT {
        usable - CONTENT_RESERVE
    } else {
        usable.min(COLLAPSED_HEIGHT)
    }
}
fn clamp_height(height: f32, available: f32) -> f32 {
    let maximum = maximum_height(available);
    height.clamp(COLLAPSED_HEIGHT.min(maximum), maximum)
}
fn valid_point(point: Point) -> bool {
    point.x.is_finite() && point.y.is_finite()
}
fn trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("OMD_TRACE_COLLECTOR").is_ok_and(|value| value == "1"))
}
fn event_label(event: &UiEvent) -> &'static str {
    match event {
        UiEvent::Mouse(mouse::Event::CursorMoved { .. }) => "move",
        UiEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => "press",
        UiEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => "release",
        UiEvent::Mouse(mouse::Event::CursorLeft) => "leave",
        UiEvent::Window(window::Event::Unfocused) => "unfocused",
        UiEvent::Window(window::Event::Closed) => "closed",
        _ => "other",
    }
}

#[derive(Clone, Copy, Default)]
enum PointerHistory {
    #[default]
    Unknown,
    Present(Point),
    Absent,
}
impl PointerHistory {
    fn position(self) -> Option<Point> {
        match self {
            Self::Present(point) => Some(point),
            Self::Unknown | Self::Absent => None,
        }
    }
}

#[derive(Default)]
enum Session {
    #[default]
    Idle,
    Item {
        entry: Entry,
        origin: Point,
        point: Point,
        dragging: bool,
    },
    Resize {
        origin: Point,
        initial_height: f32,
        height: f32,
    },
}
impl Session {
    fn phase(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Item {
                dragging: false, ..
            } => "candidate",
            Self::Item { dragging: true, .. } => "dragging",
            Self::Resize { .. } => "resize",
        }
    }
}
struct Shared {
    session: Session,
    generation: u64,
    window_pointer: PointerHistory,
    scope: Option<Scope>,
    scope_bound: bool,
    item_enabled: bool,
    resize_enabled: bool,
}
impl Default for Shared {
    fn default() -> Self {
        Self {
            session: Session::Idle,
            generation: 0,
            window_pointer: PointerHistory::Unknown,
            scope: None,
            scope_bound: false,
            item_enabled: true,
            resize_enabled: true,
        }
    }
}

/// Never put this Rc in a Task or Message. Widgets borrow it only synchronously.
#[derive(Clone, Default)]
pub(super) struct Controller(Rc<RefCell<Shared>>);
impl Controller {
    pub fn arm(&self, entry: Entry, window_point: Point) -> bool {
        let trace = trace_enabled().then(|| (entry.scope.epoch, entry.node.id.clone()));
        let mut shared = self.0.borrow_mut();
        let accepted = valid_point(window_point)
            && shared.item_enabled
            && matches!(shared.session, Session::Idle)
            && (!shared.scope_bound || shared.scope.as_ref() == Some(&entry.scope));
        if accepted {
            shared.generation = shared.generation.wrapping_add(1);
            shared.session = Session::Item {
                entry,
                origin: window_point,
                point: window_point,
                dragging: false,
            };
        }
        let phase = shared.session.phase();
        let generation = shared.generation;
        drop(shared);
        if let Some((epoch, node_id)) = trace {
            eprintln!(
                "collector arm epoch={epoch} node_id={node_id:?} point={window_point:?} accepted={accepted} phase={phase} generation={generation}"
            );
        }
        accepted
    }
    pub fn is_dragging(&self) -> bool {
        matches!(
            self.0.borrow().session,
            Session::Item { dragging: true, .. }
        )
    }
    pub fn generation(&self) -> u64 {
        self.0.borrow().generation
    }
    pub fn cancel(&self) {
        let mut shared = self.0.borrow_mut();
        shared.session = Session::Idle;
        shared.generation = shared.generation.wrapping_add(1);
    }
    /// A source can finish its click, but must not consume a global drag release.
    pub fn cancel_candidate(&self) {
        let mut shared = self.0.borrow_mut();
        if matches!(
            shared.session,
            Session::Item {
                dragging: false,
                ..
            }
        ) {
            shared.session = Session::Idle;
            shared.generation = shared.generation.wrapping_add(1);
        }
    }
    fn is_idle(&self) -> bool {
        matches!(self.0.borrow().session, Session::Idle)
    }
    pub fn window_pointer(&self) -> Option<Point> {
        self.0.borrow().window_pointer.position()
    }
    /// iced supplies one final cursor for an entire event batch. Only movement
    /// events advance history; button events use the preceding ordered point.
    /// Unknown permits one initial fallback, while explicit exit stays absent.
    fn ordered_cursor(&self, event: &UiEvent, frame_cursor: mouse::Cursor) -> mouse::Cursor {
        let is_mouse = matches!(event, UiEvent::Mouse(_));
        let exited = matches!(
            event,
            UiEvent::Mouse(mouse::Event::CursorLeft)
                | UiEvent::Window(window::Event::Unfocused | window::Event::Closed)
        );
        let mut shared = self.0.borrow_mut();
        if exited {
            shared.window_pointer = PointerHistory::Absent;
        } else if let UiEvent::Mouse(mouse::Event::CursorMoved { position }) = event {
            shared.window_pointer = if valid_point(*position) {
                PointerHistory::Present(*position)
            } else {
                PointerHistory::Absent
            };
        } else if is_mouse && matches!(shared.window_pointer, PointerHistory::Unknown) {
            shared.window_pointer = frame_cursor
                .land()
                .position()
                .filter(|point| valid_point(*point))
                .map(PointerHistory::Present)
                .unwrap_or(PointerHistory::Absent);
        }
        let point = shared.window_pointer.position();
        let phase = shared.session.phase();
        let log_event = trace_enabled()
            && (phase != "idle"
                || matches!(
                    event,
                    UiEvent::Mouse(
                        mouse::Event::ButtonPressed(mouse::Button::Left)
                            | mouse::Event::ButtonReleased(mouse::Button::Left)
                    )
                ));
        drop(shared);
        if log_event {
            eprintln!(
                "collector pointer event={} ordered={point:?} frame={frame_cursor:?} phase={phase}",
                event_label(event)
            );
        }
        if !is_mouse && !exited {
            frame_cursor
        } else {
            point.map_or(mouse::Cursor::Unavailable, |point| {
                if frame_cursor.is_levitating() {
                    mouse::Cursor::Levitating(point)
                } else {
                    mouse::Cursor::Available(point)
                }
            })
        }
    }
    fn is_resizing(&self) -> bool {
        matches!(self.0.borrow().session, Session::Resize { .. })
    }
    fn candidate_for(&self, entry: &Entry) -> bool {
        matches!(&self.0.borrow().session, Session::Item { entry: current, dragging: false, .. }
            if current.scope == entry.scope && current.node.id == entry.node.id)
    }
    fn local_click_allowed(&self, entry: Option<&Entry>) -> bool {
        let shared = self.0.borrow();
        matches!(shared.session, Session::Idle)
            && entry.is_none_or(|entry| {
                !shared.scope_bound || shared.scope.as_ref() == Some(&entry.scope)
            })
    }
    fn synchronize(&self, scope: Option<&Scope>, item_enabled: bool, resize_enabled: bool) {
        let mut shared = self.0.borrow_mut();
        let changed = shared.scope_bound && shared.scope.as_ref() != scope;
        let invalid = match &shared.session {
            Session::Item { entry, .. } => !item_enabled || scope != Some(&entry.scope),
            Session::Resize { .. } => !resize_enabled,
            Session::Idle => false,
        };
        if changed || invalid {
            shared.session = Session::Idle;
            shared.generation = shared.generation.wrapping_add(1);
        }
        shared.scope = scope.cloned();
        shared.scope_bound = true;
        shared.item_enabled = item_enabled;
        shared.resize_enabled = resize_enabled;
    }
    fn start_resize(&self, origin: Point, height: f32) -> bool {
        let mut shared = self.0.borrow_mut();
        let accepted =
            valid_point(origin) && shared.resize_enabled && matches!(shared.session, Session::Idle);
        if accepted {
            shared.generation = shared.generation.wrapping_add(1);
            shared.session = Session::Resize {
                origin,
                initial_height: height,
                height,
            };
        }
        let phase = shared.session.phase();
        drop(shared);
        if trace_enabled() {
            eprintln!(
                "collector resize origin={origin:?} height={height} accepted={accepted} phase={phase}"
            );
        }
        accepted
    }
    fn track(&self, point: Point, available: f32) -> Option<f32> {
        if !valid_point(point) {
            return None;
        }
        let mut shared = self.0.borrow_mut();
        let before = shared.session.phase();
        let result = match &mut shared.session {
            Session::Item {
                origin,
                point: current,
                dragging,
                ..
            } => {
                *current = point;
                *dragging |= origin.distance(point) >= DRAG_DISTANCE;
                None
            }
            Session::Resize {
                origin,
                initial_height,
                height,
            } => {
                let next = clamp_height(*initial_height - (point.y - origin.y), available);
                if (next - *height).abs() > f32::EPSILON {
                    *height = next;
                    Some(next)
                } else {
                    None
                }
            }
            Session::Idle => None,
        };
        let after = shared.session.phase();
        drop(shared);
        if trace_enabled() && before != "idle" {
            eprintln!(
                "collector track point={point:?} before={before} after={after} height={result:?}"
            );
        }
        result
    }
    fn finish(
        &self,
        pointer: Option<Point>,
        collector: Rectangle,
        scope: Option<&Scope>,
        enabled: bool,
    ) -> Option<Entry> {
        let mut shared = self.0.borrow_mut();
        let session = std::mem::take(&mut shared.session);
        if !matches!(session, Session::Idle) {
            shared.generation = shared.generation.wrapping_add(1);
        }
        let phase = session.phase();
        let scope_match = match &session {
            Session::Item { entry, .. } => Some(scope == Some(&entry.scope)),
            _ => None,
        };
        let inside = pointer.is_some_and(|point| valid_point(point) && collector.contains(point));
        let result = match session {
            Session::Item {
                entry,
                dragging: true,
                ..
            } if enabled && scope == Some(&entry.scope) && inside => Some(entry),
            _ => None,
        };
        drop(shared);
        if trace_enabled() {
            eprintln!(
                "collector finish phase={phase} point={pointer:?} rect={collector:?} scope_match={scope_match:?} enabled={enabled} inside={inside} drop={}",
                result.is_some()
            );
        }
        result
    }
    fn ghost(&self) -> Option<(String, Point)> {
        let shared = self.0.borrow();
        if let Session::Item {
            entry,
            point,
            dragging: true,
            ..
        } = &shared.session
        {
            Some((entry.node.name.clone(), *point))
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Geometry {
    content: Rectangle,
    divider: Rectangle,
    collector: Rectangle,
}
impl Geometry {
    fn new(bounds: Rectangle, panel: PanelState) -> Self {
        let height = finite_height(bounds.height);
        let divider_height = SPLIT_HEIGHT.min(height);
        let collector_height = panel.effective_height(height);
        let content_height = (height - divider_height - collector_height).max(0.0);
        Self {
            content: Rectangle {
                height: content_height,
                ..bounds
            },
            divider: Rectangle {
                y: bounds.y + content_height,
                height: divider_height,
                ..bounds
            },
            collector: Rectangle {
                y: bounds.y + content_height + divider_height,
                height: collector_height,
                ..bounds
            },
        }
    }
}
fn canceled(event: &UiEvent) -> bool {
    matches!(
        event,
        UiEvent::Mouse(mouse::Event::CursorLeft)
            | UiEvent::Window(window::Event::Unfocused | window::Event::Closed)
            | UiEvent::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            })
    )
}
fn released(event: &UiEvent) -> bool {
    matches!(
        event,
        UiEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    )
}

pub(super) fn workspace<'a, M: Clone + 'a>(
    content: Element<'a, M>,
    collector: Element<'a, M>,
    controller: Controller,
    panel: PanelState,
    current_scope: Option<Scope>,
    item_enabled: bool,
    resize_enabled: bool,
    on_event: impl Fn(Event) -> M + 'a,
) -> Element<'a, M> {
    Element::new(Workspace {
        children: [content, collector],
        controller,
        panel,
        scope: current_scope,
        item_enabled,
        resize_enabled,
        on_event: Box::new(on_event),
    })
}
struct Workspace<'a, M> {
    children: [Element<'a, M>; 2],
    controller: Controller,
    panel: PanelState,
    scope: Option<Scope>,
    item_enabled: bool,
    resize_enabled: bool,
    on_event: Box<dyn Fn(Event) -> M + 'a>,
}
#[derive(Default)]
struct WorkspaceState {
    divider_hovered: bool,
}
impl<M: Clone> Widget<M, Theme, iced::Renderer> for Workspace<'_, M> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<WorkspaceState>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(WorkspaceState::default())
    }
    fn children(&self) -> Vec<Tree> {
        self.children
            .iter()
            .map(|child| Tree::new(child.as_widget()))
            .collect()
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.controller
            .synchronize(self.scope.as_ref(), self.item_enabled, self.resize_enabled);
        let resolved = limits.resolve(Length::Fill, Length::Fill, Size::new(500.0, 480.0));
        let size = Size::new(
            if resolved.width.is_finite() {
                resolved.width.max(0.0)
            } else {
                500.0
            },
            if resolved.height.is_finite() {
                resolved.height.max(0.0)
            } else {
                480.0
            },
        );
        let geometry = Geometry::new(Rectangle::with_size(size), self.panel);
        let mut nodes = Vec::with_capacity(2);
        for (index, bounds) in [geometry.content, geometry.collector]
            .into_iter()
            .enumerate()
        {
            let limits = layout::Limits::new(bounds.size(), bounds.size());
            let node = self.children[index].as_widget_mut().layout(
                &mut tree.children[index],
                renderer,
                &limits,
            );
            nodes.push(node.move_to(Point::new(bounds.x, bounds.y)));
        }
        layout::Node::with_children(size, nodes)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child
                .as_widget_mut()
                .operate(state, layout, renderer, operation);
        }
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &UiEvent,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        self.controller
            .synchronize(self.scope.as_ref(), self.item_enabled, self.resize_enabled);
        let geometry = Geometry::new(layout.bounds(), self.panel);
        // Use the event-ordered cursor for children as well: Scrollable then
        // translates it into content coordinates for each source's hit test.
        let cursor = self.controller.ordered_cursor(event, cursor);
        let pointer = self.controller.window_pointer();
        if canceled(event) {
            let active = !self.controller.is_idle();
            self.controller.cancel();
            if active {
                shell.request_redraw();
            }
        }
        if trace_enabled()
            && matches!(
                event,
                UiEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            )
        {
            eprintln!(
                "collector workspace press point={pointer:?} divider={:?} resize_enabled={} item_enabled={}",
                geometry.divider, self.resize_enabled, self.item_enabled
            );
        }
        if matches!(
            event,
            UiEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) && self.resize_enabled
            && pointer
                .is_some_and(|point| geometry.divider.contains(point) && viewport.contains(point))
        {
            if let Some(point) = pointer {
                self.controller
                    .start_resize(point, geometry.collector.height);
            }
            shell.capture_event();
            shell.request_redraw();
            return;
        }
        if (matches!(event, UiEvent::Mouse(mouse::Event::CursorMoved { .. })) || released(event))
            && let Some(point) = pointer
            && let Some(height) = self.controller.track(point, layout.bounds().height)
        {
            shell.publish((self.on_event)(Event::HeightChanged(height)));
        }
        let active = self.controller.is_dragging() || self.controller.is_resizing();
        let hovered =
            self.resize_enabled && pointer.is_some_and(|point| geometry.divider.contains(point));
        let memory = tree.state.downcast_mut::<WorkspaceState>();
        if memory.divider_hovered != hovered {
            memory.divider_hovered = hovered;
            shell.request_redraw();
        }
        if active {
            shell.request_redraw();
        }
        // Active moves belong to the owner, not a hover-preview/scroll child.
        if active && matches!(event, UiEvent::Mouse(mouse::Event::CursorMoved { .. })) {
            shell.capture_event();
            return;
        }
        // Release is deliberately forwarded first, even if another child marks
        // it captured. Sources clear their local press, see is_dragging(), and
        // cannot navigate. No RefCell guard is held while calling children.
        for ((child, state), child_layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            let region = child_layout
                .bounds()
                .intersection(viewport)
                .unwrap_or(Rectangle::with_size(Size::ZERO));
            child.as_widget_mut().update(
                state,
                event,
                child_layout,
                cursor,
                renderer,
                clipboard,
                shell,
                &region,
            );
        }
        if released(event) {
            let target = geometry
                .collector
                .intersection(viewport)
                .unwrap_or(Rectangle::with_size(Size::ZERO));
            if let Some(entry) =
                self.controller
                    .finish(pointer, target, self.scope.as_ref(), self.item_enabled)
            {
                shell.publish((self.on_event)(Event::Dropped(entry)));
            }
            if active {
                shell.capture_event();
                shell.request_redraw();
            }
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
        if self.controller.is_resizing() {
            return mouse::Interaction::ResizingVertically;
        }
        if self.controller.is_dragging() {
            return mouse::Interaction::Grabbing;
        }
        if self.resize_enabled && cursor.is_over(Geometry::new(layout.bounds(), self.panel).divider)
        {
            return mouse::Interaction::ResizingVertically;
        }
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        for ((child, state), child_layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            if let Some(region) = child_layout.bounds().intersection(viewport) {
                renderer.with_layer(region, |renderer| {
                    child.as_widget().draw(
                        state,
                        renderer,
                        theme,
                        style,
                        child_layout,
                        cursor,
                        &region,
                    );
                });
            }
        }
        let geometry = Geometry::new(layout.bounds(), self.panel);
        let active = self.controller.is_resizing()
            || tree.state.downcast_ref::<WorkspaceState>().divider_hovered;
        renderer.fill_quad(
            renderer::Quad {
                bounds: geometry.divider,
                ..Default::default()
            },
            Color::from_rgb(0.10, 0.12, 0.17),
        );
        let grip = Rectangle {
            x: geometry.divider.center_x() - 20.0,
            y: geometry.divider.center_y() - 1.0,
            width: 40.0,
            height: 2.0,
        };
        renderer.fill_quad(
            renderer::Quad {
                bounds: grip,
                border: Border {
                    radius: 1.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            if active {
                Color::from_rgb(0.47, 0.77, 0.70)
            } else {
                Color::from_rgb(0.30, 0.34, 0.43)
            },
        );
        if let Some((label, point)) = self.controller.ghost() {
            if self.item_enabled && geometry.collector.contains(point) {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: geometry.collector,
                        border: Border {
                            color: Color::from_rgb(0.38, 0.82, 0.69),
                            width: 2.0,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    Color::from_rgba(0.16, 0.50, 0.41, 0.13),
                );
            }
            if let Some(clip) = layout.bounds().intersection(viewport) {
                let width = clip.width.min(250.0);
                let height = clip.height.min(34.0);
                if width > 20.0 && height > 0.0 {
                    let bounds = Rectangle {
                        x: (point.x + 12.0).clamp(clip.x, clip.x + clip.width - width),
                        y: (point.y + 12.0).clamp(clip.y, clip.y + clip.height - height),
                        width,
                        height,
                    };
                    let font = renderer.default_font();
                    let label = if label.chars().count() > 32 {
                        format!("{}…", label.chars().take(32).collect::<String>())
                    } else {
                        label
                    };
                    renderer.with_layer(clip, |renderer| {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds,
                                border: Border {
                                    radius: 6.0.into(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            },
                            Color::from_rgb(0.10, 0.14, 0.20),
                        );
                        renderer.fill_text(
                            text::Text {
                                content: label,
                                bounds: Size::new(width - 20.0, height),
                                size: 12.0.into(),
                                line_height: text::LineHeight::Relative(1.0),
                                font,
                                align_x: text::Alignment::Left,
                                align_y: iced::alignment::Vertical::Center,
                                shaping: text::Shaping::Advanced,
                                wrapping: text::Wrapping::None,
                            },
                            // fill_text takes an alignment anchor: both renderers
                            // subtract half the shaped text height for Center.
                            Point::new(bounds.x + 10.0, bounds.center_y()),
                            Color::WHITE,
                            clip,
                        );
                    });
                }
            }
        }
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, iced::Renderer>> {
        // No interaction overlay is allocated, in particular none on idle frames.
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

pub(super) fn source<'a, M: Clone + 'a>(
    content: Element<'a, M>,
    controller: Controller,
    entry: Option<Entry>,
    on_click: M,
) -> Element<'a, M> {
    Element::new(Source {
        content,
        controller,
        entry,
        on_click,
    })
}
struct Source<'a, M> {
    content: Element<'a, M>,
    controller: Controller,
    entry: Option<Entry>,
    on_click: M,
}
#[derive(Default)]
struct SourceState {
    press: Option<Press>,
}
struct Press {
    origin: Point,
    generation: u64,
    moved: bool,
    armed: bool,
    key: Option<(Scope, String)>,
}
impl<M: Clone> Widget<M, Theme, iced::Renderer> for Source<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<SourceState>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(SourceState::default())
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
        event: &UiEvent,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let generation = self.controller.generation();
        let memory = tree.state.downcast_mut::<SourceState>();
        if canceled(event)
            || memory
                .press
                .as_ref()
                .is_some_and(|press| press.generation != generation)
        {
            memory.press = None;
        }
        let clip = layout.bounds().intersection(viewport);
        let local_point = cursor.position();
        let window_point = self.controller.window_pointer();
        if let UiEvent::Mouse(mouse::Event::CursorMoved { position }) = event
            && let Some(press) = &mut memory.press
        {
            press.moved |= press.origin.distance(*position) >= DRAG_DISTANCE;
        }
        let press = if released(event) {
            memory.press.take()
        } else {
            None
        };
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
        if let Some(press) = press {
            let key = self
                .entry
                .as_ref()
                .map(|entry| (entry.scope.clone(), entry.node.id.clone()));
            let owned = if press.armed {
                self.entry
                    .as_ref()
                    .is_some_and(|entry| self.controller.candidate_for(entry))
            } else {
                self.controller.local_click_allowed(self.entry.as_ref())
            };
            if press.generation == self.controller.generation()
                && press.key == key
                && owned
                && !press.moved
                && window_point.is_some_and(|point| press.origin.distance(point) < DRAG_DISTANCE)
                && local_point.is_some_and(|point| clip.is_some_and(|clip| clip.contains(point)))
            {
                self.controller.cancel_candidate();
                shell.publish(self.on_click.clone());
                shell.capture_event();
            }
            return;
        }
        if shell.is_event_captured() {
            return;
        }
        if matches!(
            event,
            UiEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) && local_point.is_some_and(|point| clip.is_some_and(|clip| clip.contains(point)))
            && let Some(point) = window_point
        {
            let armed = self
                .entry
                .as_ref()
                .is_some_and(|entry| self.controller.arm(entry.clone(), point));
            if armed || self.controller.local_click_allowed(self.entry.as_ref()) {
                tree.state.downcast_mut::<SourceState>().press = Some(Press {
                    origin: point,
                    generation: self.controller.generation(),
                    moved: false,
                    armed,
                    key: self
                        .entry
                        .as_ref()
                        .map(|entry| (entry.scope.clone(), entry.node.id.clone())),
                });
                shell.capture_event();
            }
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
        if self.controller.is_dragging() {
            return mouse::Interaction::Grabbing;
        }
        if cursor.is_over(layout.bounds())
            && cursor
                .position()
                .is_some_and(|point| viewport.contains(point))
        {
            return mouse::Interaction::Pointer;
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
    use ohmydisk_core::{NativePath, NodeKind};
    use std::path::Path;

    fn scope(epoch: u64) -> Scope {
        Scope {
            scan_id: "test-scan".into(),
            epoch,
        }
    }
    fn entry(epoch: u64, id: &str) -> Entry {
        Entry {
            scope: scope(epoch),
            node: Node {
                space_usage: None,
                id: id.into(),
                parent_id: Some("root".into()),
                name: id.into(),
                path: NativePath::new(Path::new("/test-fixture")),
                kind: NodeKind::Directory,
                logical_bytes: 100,
                allocated_bytes: None,
                modified_ns: None,
                identity: id.into(),
                volume_id: "fixture".into(),
                link_count: 1,
                complete: true,
                is_link_or_reparse: false,
            },
        }
    }
    fn target() -> Rectangle {
        Rectangle::new(Point::new(0.0, 350.0), Size::new(500.0, 130.0))
    }
    fn bound_controller() -> Controller {
        let controller = Controller::default();
        controller.synchronize(Some(&scope(1)), true, true);
        controller
    }

    fn moved(position: Point) -> UiEvent {
        UiEvent::Mouse(mouse::Event::CursorMoved { position })
    }
    fn pressed() -> UiEvent {
        UiEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }
    fn mouse_released() -> UiEvent {
        UiEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }

    #[test]
    fn ordered_endpoint_survives_release_with_stale_outside_frame_cursor() {
        let controller = bound_controller();
        let origin = Point::new(100.0, 100.0);
        let endpoint = Point::new(100.0, 400.0);
        let stale = mouse::Cursor::Available(Point::new(-200.0, -200.0));
        controller.ordered_cursor(&moved(origin), stale);
        controller.ordered_cursor(&pressed(), stale);
        assert!(controller.arm(entry(1, "node"), controller.window_pointer().unwrap()));
        controller.ordered_cursor(&moved(endpoint), stale);
        controller.track(controller.window_pointer().unwrap(), 480.0);
        let release_cursor = controller.ordered_cursor(&mouse_released(), stale);
        assert_eq!(release_cursor.position(), Some(endpoint));
        controller.track(controller.window_pointer().unwrap(), 480.0);
        assert!(
            controller
                .finish(controller.window_pointer(), target(), Some(&scope(1)), true)
                .is_some()
        );
        assert!(
            controller
                .finish(controller.window_pointer(), target(), Some(&scope(1)), true)
                .is_none()
        );
    }

    #[test]
    fn batched_press_dispatch_hits_source_before_later_endpoint() {
        let controller = bound_controller();
        let origin = Point::new(110.0, 110.0);
        let endpoint = Point::new(110.0, 400.0);
        // Every event in this batch receives the same final cursor from iced.
        let final_cursor = mouse::Cursor::Available(endpoint);
        controller.ordered_cursor(&moved(origin), final_cursor);
        let press_cursor = controller.ordered_cursor(&pressed(), final_cursor);
        let translation = Vector::new(0.0, 100.0);
        let scrolled_row = Rectangle::new(Point::new(100.0, 200.0), Size::new(100.0, 30.0));
        assert!((press_cursor + translation).is_over(scrolled_row));
        assert!(!(final_cursor + translation).is_over(scrolled_row));
        assert!(controller.arm(entry(1, "node"), controller.window_pointer().unwrap()));
        controller.ordered_cursor(&moved(endpoint), final_cursor);
        controller.track(controller.window_pointer().unwrap(), 480.0);
        controller.ordered_cursor(&mouse_released(), final_cursor);
        assert!(
            controller
                .finish(controller.window_pointer(), target(), Some(&scope(1)), true)
                .is_some()
        );
    }

    #[test]
    fn ordered_resize_endpoint_is_not_replaced_by_release_frame_cursor() {
        let controller = bound_controller();
        let final_cursor = mouse::Cursor::Available(Point::new(20.0, 800.0));
        controller.ordered_cursor(&moved(Point::new(20.0, 300.0)), final_cursor);
        controller.ordered_cursor(&pressed(), final_cursor);
        assert!(controller.start_resize(controller.window_pointer().unwrap(), 180.0));
        controller.ordered_cursor(&moved(Point::new(20.0, 200.0)), final_cursor);
        assert_eq!(
            controller.track(controller.window_pointer().unwrap(), 900.0),
            Some(280.0)
        );
        controller.ordered_cursor(&mouse_released(), final_cursor);
        assert_eq!(controller.window_pointer(), Some(Point::new(20.0, 200.0)));
        assert_eq!(
            controller.track(controller.window_pointer().unwrap(), 900.0),
            None
        );
        assert!(matches!(
            controller.0.borrow().session,
            Session::Resize { height, .. } if height == 280.0
        ));
        assert!(
            controller
                .finish(controller.window_pointer(), target(), Some(&scope(1)), true)
                .is_none()
        );
    }

    #[test]
    fn explicit_exit_cannot_revive_pointer_from_future_frame_cursor() {
        for exit in [
            UiEvent::Mouse(mouse::Event::CursorLeft),
            UiEvent::Window(window::Event::Unfocused),
            UiEvent::Window(window::Event::Closed),
        ] {
            let controller = bound_controller();
            let future = mouse::Cursor::Available(Point::new(100.0, 400.0));
            controller.ordered_cursor(&moved(Point::new(20.0, 20.0)), future);
            assert_eq!(
                controller.ordered_cursor(&exit, future),
                mouse::Cursor::Unavailable
            );
            assert_eq!(
                controller.ordered_cursor(&pressed(), future),
                mouse::Cursor::Unavailable
            );
            assert_eq!(controller.window_pointer(), None);
            assert_eq!(
                controller
                    .ordered_cursor(&moved(Point::new(40.0, 40.0)), future)
                    .position(),
                Some(Point::new(40.0, 40.0))
            );
        }
    }

    #[test]
    fn unknown_pointer_falls_back_once_and_dispatch_preserves_levitation() {
        let controller = bound_controller();
        let origin = Point::new(20.0, 20.0);
        assert_eq!(
            controller
                .ordered_cursor(&pressed(), mouse::Cursor::Available(origin))
                .position(),
            Some(origin)
        );
        let later = Point::new(100.0, 400.0);
        assert_eq!(
            controller.ordered_cursor(&mouse_released(), mouse::Cursor::Levitating(later)),
            mouse::Cursor::Levitating(origin)
        );
        assert_eq!(controller.window_pointer(), Some(origin));
        assert_eq!(
            controller
                .ordered_cursor(&moved(later), mouse::Cursor::Unavailable)
                .position(),
            Some(later)
        );
    }

    #[test]
    fn direct_jump_beyond_source_promotes_and_drops_exactly_once() {
        let controller = bound_controller();
        assert!(controller.arm(entry(1, "node"), Point::new(100.0, 100.0)));
        let generation = controller.generation();
        controller.track(Point::new(100.0, 400.0), 480.0);
        assert!(controller.is_dragging());
        assert_eq!(controller.generation(), generation);
        assert_eq!(
            controller
                .finish(
                    Some(Point::new(100.0, 400.0)),
                    target(),
                    Some(&scope(1)),
                    true
                )
                .unwrap()
                .node
                .id,
            "node"
        );
        assert_ne!(controller.generation(), generation);
        assert!(
            controller
                .finish(
                    Some(Point::new(100.0, 400.0)),
                    target(),
                    Some(&scope(1)),
                    true
                )
                .is_none()
        );
    }

    #[test]
    fn actual_release_point_wins_over_previous_hover() {
        let controller = bound_controller();
        assert!(controller.arm(entry(1, "node"), Point::new(100.0, 100.0)));
        controller.track(Point::new(100.0, 400.0), 480.0);
        assert!(
            controller
                .finish(
                    Some(Point::new(100.0, 200.0)),
                    target(),
                    Some(&scope(1)),
                    true
                )
                .is_none()
        );
        assert!(controller.is_idle());
    }

    #[test]
    fn threshold_is_sticky_and_candidate_cleanup_cannot_consume_drag() {
        let controller = bound_controller();
        let item = entry(1, "node");
        let origin = Point::new(100.0, 100.0);
        assert!(controller.arm(item.clone(), origin));
        controller.track(Point::new(105.9, 100.0), 480.0);
        assert!(!controller.is_dragging());
        assert!(controller.candidate_for(&item));
        controller.track(Point::new(106.0, 100.0), 480.0);
        controller.track(origin, 480.0);
        controller.cancel_candidate();
        assert!(controller.is_dragging());
        assert!(!controller.candidate_for(&item));
        assert!(
            controller
                .finish(Some(origin), target(), Some(&scope(1)), true)
                .is_none()
        );
        assert!(controller.is_idle());
    }

    #[test]
    fn short_candidate_never_drops_and_can_be_cleared_for_click() {
        let controller = bound_controller();
        let item = entry(1, "node");
        assert!(controller.arm(item.clone(), Point::new(100.0, 400.0)));
        assert!(controller.candidate_for(&item));
        controller.cancel_candidate();
        assert!(controller.is_idle());
        assert!(controller.arm(item, Point::new(100.0, 400.0)));
        controller.track(Point::new(104.0, 400.0), 480.0);
        assert!(
            controller
                .finish(
                    Some(Point::new(104.0, 400.0)),
                    target(),
                    Some(&scope(1)),
                    true
                )
                .is_none()
        );
    }

    #[test]
    fn stale_scope_and_disabled_session_cannot_publish_drop() {
        let controller = bound_controller();
        assert!(!controller.arm(entry(2, "stale"), Point::ORIGIN));
        assert!(controller.arm(entry(1, "node"), Point::ORIGIN));
        controller.track(Point::new(100.0, 400.0), 480.0);
        let generation = controller.generation();
        controller.synchronize(Some(&scope(2)), true, true);
        assert!(controller.is_idle());
        assert_ne!(controller.generation(), generation);
        assert!(
            controller
                .finish(
                    Some(Point::new(100.0, 400.0)),
                    target(),
                    Some(&scope(2)),
                    true
                )
                .is_none()
        );
        controller.synchronize(Some(&scope(1)), false, true);
        assert!(!controller.arm(entry(1, "node"), Point::ORIGIN));
        controller.synchronize(Some(&scope(1)), true, true);
        assert!(controller.arm(entry(1, "node"), Point::ORIGIN));
        controller.track(Point::new(100.0, 400.0), 480.0);
        assert!(
            controller
                .finish(
                    Some(Point::new(100.0, 400.0)),
                    target(),
                    Some(&scope(1)),
                    false
                )
                .is_none()
        );
    }

    #[test]
    fn cancel_invalidates_even_idle_local_clicks() {
        let controller = bound_controller();
        let initial = controller.generation();
        controller.cancel();
        assert_ne!(controller.generation(), initial);
        assert!(controller.arm(entry(1, "node"), Point::ORIGIN));
        controller.track(Point::new(100.0, 400.0), 480.0);
        controller.cancel();
        assert!(controller.is_idle());
        assert!(
            controller
                .finish(
                    Some(Point::new(100.0, 400.0)),
                    target(),
                    Some(&scope(1)),
                    true
                )
                .is_none()
        );
    }

    #[test]
    fn separator_and_item_sessions_are_mutually_exclusive() {
        let controller = bound_controller();
        assert!(controller.arm(entry(1, "node"), Point::ORIGIN));
        assert!(!controller.start_resize(Point::new(10.0, 300.0), 180.0));
        controller.cancel();
        assert!(controller.start_resize(Point::new(10.0, 300.0), 180.0));
        assert!(!controller.arm(entry(1, "node"), Point::ORIGIN));
        assert_eq!(
            controller.track(Point::new(10.0, -200.0), 480.0),
            Some(232.0)
        );
        assert_eq!(
            controller.track(Point::new(10.0, 1000.0), 480.0),
            Some(64.0)
        );
        controller.cancel_candidate();
        assert!(controller.is_resizing());
        controller.finish(None, target(), Some(&scope(1)), true);
        assert!(controller.is_idle());
    }

    #[test]
    fn layout_uses_local_workspace_including_offset_and_short_banners() {
        let mut panel = PanelState::default();
        panel.open();
        let geometry = Geometry::new(
            Rectangle::new(Point::new(24.0, 120.0), Size::new(500.0, 380.0)),
            panel,
        );
        assert_eq!(
            geometry.content,
            Rectangle::new(Point::new(24.0, 120.0), Size::new(500.0, 240.0))
        );
        assert_eq!(geometry.divider.height, 8.0);
        assert_eq!(geometry.collector.height, 132.0);
        assert_eq!(geometry.collector.y + geometry.collector.height, 500.0);
        for height in [0.0, 4.0, 8.0, 30.0, 100.0, 311.0, 480.0] {
            let geometry = Geometry::new(Rectangle::with_size(Size::new(500.0, height)), panel);
            assert!(geometry.content.height >= 0.0);
            assert!(geometry.collector.height >= 0.0);
            assert_eq!(
                geometry.content.height + geometry.divider.height + geometry.collector.height,
                height
            );
        }
    }

    #[test]
    fn shrink_and_collapse_preserve_useful_expanded_preference() {
        let mut panel = PanelState::default();
        assert_eq!(panel.effective_height(900.0), 64.0);
        panel.resize(400.0);
        assert_eq!(panel.effective_height(900.0), 400.0);
        assert_eq!(panel.effective_height(480.0), 232.0);
        assert_eq!(panel.effective_height(900.0), 400.0);
        panel.resize(64.0);
        assert!(!panel.is_expanded());
        panel.open();
        assert_eq!(panel.effective_height(900.0), 400.0);
        panel.resize(80.0);
        assert_eq!(panel.effective_height(900.0), 80.0);
        panel.toggle();
        panel.open();
        assert_eq!(panel.effective_height(900.0), 400.0);
    }

    #[test]
    fn short_panel_opens_to_useful_default_and_rejects_nonfinite_resize() {
        let mut panel = PanelState::default();
        panel.resize(80.0);
        panel.toggle();
        panel.toggle();
        assert!(panel.effective_height(900.0) >= 150.0);
        let height = panel.effective_height(900.0);
        panel.resize(f32::NAN);
        panel.resize(f32::INFINITY);
        assert_eq!(panel.effective_height(900.0), height);
        assert_eq!(panel.effective_height(f32::NAN), 0.0);
    }

    #[test]
    fn cancellation_events_and_invalid_points_are_explicit() {
        assert!(canceled(&UiEvent::Mouse(mouse::Event::CursorLeft)));
        assert!(canceled(&UiEvent::Window(window::Event::Unfocused)));
        assert!(canceled(&UiEvent::Window(window::Event::Closed)));
        assert!(!canceled(&UiEvent::Mouse(mouse::Event::CursorMoved {
            position: Point::ORIGIN
        })));
        let controller = bound_controller();
        assert!(!controller.arm(entry(1, "node"), Point::new(f32::NAN, 0.0)));
        assert!(!controller.start_resize(Point::new(0.0, f32::INFINITY), 180.0));
    }
}
