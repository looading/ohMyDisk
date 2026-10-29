//! Bounded occupancy-only sunburst and typed, non-destructive synthetic sectors.
use super::{
    Message, interaction,
    space_view::{Capacity, node_bytes},
};
use iced::widget::canvas::{self, Action, Frame, Geometry, Path};
use iced::{Color, Point, Rectangle, Renderer, Theme, mouse};
use ohmydisk_core::{Node, NodeKind};
use std::f32::consts::{FRAC_PI_2, TAU};
use std::{cell::RefCell, collections::HashMap};

const MAX_DEPTH: usize = 5;
const MIN_SWEEP: f32 = 0.009;
const MAX_SIBLINGS: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Node(String),
    Aggregate { owner_id: String },
    Available,
    Unattributed,
    UsedOverview,
}
impl Target {
    pub fn node_id(&self) -> Option<&str> {
        if let Self::Node(id) = self {
            Some(id)
        } else {
            None
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub target: Target,
    pub depth: usize,
    pub start: f32,
    pub end: f32,
    pub bytes: u64,
    pub color_index: usize,
}
impl Segment {
    fn synthetic(target: Target, start: f32, end: f32, bytes: u64) -> Self {
        Self {
            target,
            start,
            end,
            bytes,
            depth: 0,
            color_index: 0,
        }
    }
}

pub fn layout(root_id: &str, total: u64, nodes: &[Node]) -> Vec<Segment> {
    hierarchy(root_id, total, TAU, nodes)
}
pub fn capacity_layout(root_id: &str, capacity: Capacity, nodes: &[Node]) -> Vec<Segment> {
    if capacity.total == 0 {
        return Vec::new();
    }
    let angle = |bytes: u64| TAU * (bytes as f64 / capacity.total as f64) as f32;
    let known_end = angle(capacity.attributed);
    let mut output = hierarchy(root_id, capacity.attributed, known_end, nodes);
    let remainder_end = known_end + angle(capacity.remainder);
    if capacity.remainder > 0 {
        output.push(Segment::synthetic(
            Target::Unattributed,
            known_end,
            remainder_end,
            capacity.remainder,
        ));
    }
    if capacity.available > 0 {
        output.push(Segment::synthetic(
            Target::Available,
            remainder_end,
            TAU,
            capacity.available,
        ));
    }
    output
}
pub fn overview_layout(total: u64, available: u64) -> Vec<Segment> {
    let Some(used) = total.checked_sub(available) else {
        return Vec::new();
    };
    if total == 0 {
        return Vec::new();
    }
    let split = TAU * (used as f64 / total as f64) as f32;
    let mut output = Vec::new();
    if used > 0 {
        output.push(Segment::synthetic(Target::UsedOverview, 0.0, split, used));
    }
    if available > 0 {
        output.push(Segment::synthetic(Target::Available, split, TAU, available));
    }
    output
}
fn hierarchy(root: &str, total: u64, sweep: f32, nodes: &[Node]) -> Vec<Segment> {
    if total == 0 {
        return Vec::new();
    }
    let mut children: HashMap<&str, Vec<&Node>> = HashMap::new();
    for node in nodes {
        if let Some(parent) = node.parent_id.as_deref() {
            children.entry(parent).or_default().push(node);
        }
    }
    for entries in children.values_mut() {
        entries.sort_by(|a, b| node_bytes(b).cmp(&node_bytes(a)).then(a.id.cmp(&b.id)));
    }
    let mut output = Vec::new();
    layout_level(root, total, 0.0, sweep, 0, 0, &children, &mut output);
    output
}
#[allow(clippy::too_many_arguments)]
fn layout_level(
    parent: &str,
    total: u64,
    start: f32,
    sweep: f32,
    depth: usize,
    palette: usize,
    children: &HashMap<&str, Vec<&Node>>,
    output: &mut Vec<Segment>,
) {
    if depth >= MAX_DEPTH || total == 0 {
        return;
    }
    let entries = children.get(parent).map(Vec::as_slice).unwrap_or(&[]);
    // An inconsistent subtree is never made to fit by scaling or truncation.
    let sum = entries.iter().try_fold(0_u64, |sum, node| {
        sum.checked_add(node_bytes(node).unwrap_or(0))
    });
    if sum.is_none_or(|sum| sum > total) {
        return;
    }
    let mut angle = start;
    let mut represented = 0_u64;
    let mut displayed = 0;
    for (index, node) in entries.iter().enumerate() {
        let Some(bytes) = node_bytes(node) else {
            continue;
        };
        if bytes == 0 {
            continue;
        }
        let width = sweep * (bytes as f64 / total as f64) as f32;
        if width < MIN_SWEEP || displayed >= MAX_SIBLINGS {
            continue;
        }
        let color_index = if depth == 0 { index } else { palette };
        output.push(Segment {
            target: Target::Node(node.id.clone()),
            depth,
            start: angle,
            end: angle + width,
            bytes,
            color_index,
        });
        if node.kind == NodeKind::Directory {
            layout_level(
                &node.id,
                bytes,
                angle,
                width,
                depth + 1,
                color_index,
                children,
                output,
            );
        }
        represented += bytes;
        angle += width;
        displayed += 1;
    }
    let remainder = total - represented;
    if remainder > 0 {
        output.push(Segment {
            target: Target::Aggregate {
                owner_id: parent.into(),
            },
            depth,
            start: angle,
            end: start + sweep,
            bytes: remainder,
            color_index: palette,
        });
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub center: Point,
    pub hole: f32,
    pub ring: f32,
}
impl Metrics {
    pub fn new(width: f32, height: f32) -> Self {
        let radius = (width.min(height) * 0.465).max(1.0);
        Self {
            center: Point::new(width / 2.0, height / 2.0),
            hole: radius * 0.27,
            ring: radius * 0.73 / MAX_DEPTH as f32,
        }
    }
    fn in_center(self, point: Point) -> bool {
        point.distance(self.center) < self.hole
    }
}
pub fn hit_test(segments: &[Segment], metrics: Metrics, point: Point) -> Option<usize> {
    let radius = point.distance(metrics.center);
    if radius < metrics.hole || radius >= metrics.hole + metrics.ring * MAX_DEPTH as f32 {
        return None;
    }
    let depth = ((radius - metrics.hole) / metrics.ring).floor() as usize;
    let angle = ((point.y - metrics.center.y).atan2(point.x - metrics.center.x) + FRAC_PI_2)
        .rem_euclid(TAU);
    segments.iter().position(|segment| {
        (segment.depth == depth || synthetic_band(segment))
            && angle >= segment.start
            && angle < segment.end
    })
}
fn synthetic_band(segment: &Segment) -> bool {
    matches!(
        segment.target,
        Target::Available | Target::Unattributed | Target::UsedOverview
    )
}
pub fn color(index: usize, depth: usize) -> Color {
    const COLORS: [(f32, f32, f32); 9] = [
        (0.25, 0.82, 0.70),
        (0.29, 0.70, 0.94),
        (0.53, 0.48, 0.98),
        (0.83, 0.40, 0.86),
        (0.98, 0.45, 0.56),
        (0.99, 0.67, 0.36),
        (0.86, 0.83, 0.34),
        (0.58, 0.84, 0.40),
        (0.30, 0.76, 0.86),
    ];
    let (r, g, b) = COLORS[index % COLORS.len()];
    let mix = depth as f32 * 0.09;
    Color::from_rgb(
        r + (1.0 - r) * mix,
        g + (1.0 - g) * mix,
        b + (1.0 - b) * mix,
    )
}
pub struct Sunburst<'a> {
    pub segments: &'a [Segment],
    pub hover: Option<Target>,
    pub collected: Vec<String>,
    pub can_go_up: bool,
    pub controller: interaction::Controller,
    pub scope: Option<interaction::Scope>,
    pub resolve_drag: Box<dyn Fn(&str) -> Option<interaction::Entry> + 'a>,
}
impl Sunburst<'_> {
    fn draw_base(&self, frame: &mut Frame, metrics: Metrics, center_hover: bool) {
        for segment in self.segments {
            let inner = metrics.hole + segment.depth as f32 * metrics.ring + 0.7;
            let outer = if synthetic_band(segment) {
                metrics.hole + metrics.ring * MAX_DEPTH as f32 - 1.0
            } else {
                inner + metrics.ring - 1.4
            };
            let gap = 0.004_f32.min((segment.end - segment.start) * 0.10);
            let start = segment.start + gap / 2.0 - FRAC_PI_2;
            let end = segment.end - gap / 2.0 - FRAC_PI_2;
            let steps = (((end - start) * 100.0).ceil() as usize).max(2);
            let path = Path::new(|builder| {
                for index in 0..=steps {
                    let angle = start + (end - start) * index as f32 / steps as f32;
                    let point = Point::new(
                        metrics.center.x + outer * angle.cos(),
                        metrics.center.y + outer * angle.sin(),
                    );
                    if index == 0 {
                        builder.move_to(point);
                    } else {
                        builder.line_to(point);
                    }
                }
                for index in (0..=steps).rev() {
                    let angle = start + (end - start) * index as f32 / steps as f32;
                    builder.line_to(Point::new(
                        metrics.center.x + inner * angle.cos(),
                        metrics.center.y + inner * angle.sin(),
                    ));
                }
                builder.close();
            });
            let mut fill = match segment.target {
                Target::Available => Color::from_rgb(0.12, 0.16, 0.22),
                Target::Unattributed => Color::from_rgb(0.38, 0.38, 0.49),
                Target::UsedOverview => Color::from_rgb(0.30, 0.45, 0.66),
                Target::Aggregate { .. } => Color::from_rgb(0.27, 0.31, 0.39),
                Target::Node(_) => color(segment.color_index, segment.depth),
            };
            let hovered = self.hover.as_ref() == Some(&segment.target);
            let selected = segment
                .target
                .node_id()
                .is_some_and(|id| self.collected.iter().any(|item| item == id));
            if hovered {
                fill.r += (1.0 - fill.r) * 0.20;
                fill.g += (1.0 - fill.g) * 0.20;
                fill.b += (1.0 - fill.b) * 0.20;
            }
            if selected {
                fill.a = 0.48;
            }
            frame.fill(&path, fill);
            if hovered || selected {
                frame.stroke(
                    &path,
                    canvas::Stroke::default()
                        .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.75))
                        .with_width(1.2),
                );
            }
        }
        if self.segments.is_empty() {
            for depth in 0..MAX_DEPTH {
                frame.stroke(
                    &Path::circle(
                        metrics.center,
                        metrics.hole + (depth as f32 + 0.5) * metrics.ring,
                    ),
                    canvas::Stroke::default()
                        .with_color(Color::from_rgb(0.12, 0.15, 0.21))
                        .with_width(metrics.ring - 2.0),
                );
            }
        }
        if center_hover {
            frame.stroke(
                &Path::circle(metrics.center, metrics.hole - 2.0),
                canvas::Stroke::default()
                    .with_color(Color::from_rgba(0.7, 0.78, 0.9, 0.6))
                    .with_width(1.0),
            );
        }
    }
}

/// Size invalidation belongs to canvas::Cache; all other painted inputs use value equality.
#[derive(Debug)]
struct GeometryKey {
    segments: Vec<Segment>,
    hover: Option<Target>,
    collected: Vec<String>,
    center_hover: bool,
}
impl GeometryKey {
    fn capture(
        segments: &[Segment],
        hover: Option<&Target>,
        collected: &[String],
        center_hover: bool,
    ) -> Self {
        Self {
            segments: segments.to_vec(),
            hover: hover.cloned(),
            collected: collected.to_vec(),
            center_hover,
        }
    }
    fn matches(
        &self,
        segments: &[Segment],
        hover: Option<&Target>,
        collected: &[String],
        center_hover: bool,
    ) -> bool {
        self.segments == segments
            && self.hover.as_ref() == hover
            && self.collected == collected
            && self.center_hover == center_hover
    }
}

#[derive(Default)]
pub struct PointerState {
    geometry: canvas::Cache<Renderer>,
    geometry_key: RefCell<Option<GeometryKey>>,
    hover: Option<usize>,
    pressed: Option<usize>,
    center_pressed: bool,
    origin: Option<Point>,
    dragging: bool,
    generation: u64,
    scope: Option<interaction::Scope>,
}
impl PointerState {
    fn synchronize(&mut self, generation: u64, scope: &Option<interaction::Scope>) {
        if self.generation != generation || &self.scope != scope {
            self.pressed = None;
            self.center_pressed = false;
            self.origin = None;
            self.dragging = false;
            self.generation = generation;
            self.scope = scope.clone();
        }
    }
}
impl canvas::Program<Message> for Sunburst<'_> {
    type State = PointerState;
    fn update(
        &self,
        state: &mut Self::State,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        state.synchronize(self.controller.generation(), &self.scope);
        // Workspace records pointer positions in event order. The cursor
        // argument may already reflect a later event from this UI batch.
        // This chart is not inside a Scrollable, so its layout bounds are also
        // in window coordinates.
        let window_position = self.controller.window_pointer().or(cursor.position());
        let position = window_position
            .filter(|point| bounds.contains(*point))
            .map(|point| Point::new(point.x - bounds.x, point.y - bounds.y));
        let metrics = Metrics::new(bounds.width, bounds.height);
        let hit = position.and_then(|point| hit_test(self.segments, metrics, point));
        match event {
            iced::Event::Mouse(mouse::Event::CursorMoved { position: point }) => {
                if let Some(origin) = state.origin {
                    state.dragging |= origin.distance(*point) >= 6.0;
                }
                if self.controller.is_dragging() {
                    state.dragging = true;
                    if state.hover.take().is_some() {
                        return Some(Action::publish(Message::ChartHover(None)));
                    }
                    return Some(Action::request_redraw());
                }
                if hit != state.hover {
                    state.hover = hit;
                    Some(Action::publish(Message::ChartHover(
                        hit.map(|i| self.segments[i].target.clone()),
                    )))
                } else if state.pressed.is_some() {
                    Some(Action::request_redraw())
                } else {
                    None
                }
            }
            iced::Event::Mouse(mouse::Event::CursorLeft) => {
                state.hover = None;
                Some(Action::publish(Message::ChartHover(None)))
            }
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if hit.is_some() =>
            {
                state.pressed = hit;
                state.origin = window_position;
                state.dragging = false;
                if let Some(id) = hit.and_then(|i| self.segments[i].target.node_id())
                    && let Some(entry) = (self.resolve_drag)(id)
                    && let Some(point) = window_position
                {
                    self.controller.arm(entry, point);
                }
                state.generation = self.controller.generation();
                Some(Action::request_redraw().and_capture())
            }
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if self.can_go_up && position.is_some_and(|point| metrics.in_center(point)) =>
            {
                state.center_pressed = true;
                state.origin = window_position;
                state.dragging = false;
                state.generation = self.controller.generation();
                Some(Action::request_redraw().and_capture())
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.center_pressed =>
            {
                state.center_pressed = false;
                state.origin = None;
                if !state.dragging && position.is_some_and(|point| metrics.in_center(point)) {
                    Some(Action::publish(Message::Up).and_capture())
                } else {
                    Some(Action::request_redraw())
                }
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.pressed.is_some() =>
            {
                let pressed = state.pressed.take();
                state.origin = None;
                let dragging = state.dragging || self.controller.is_dragging();
                let segment = if !dragging && pressed == hit {
                    hit.map(|i| self.segments[i].clone())
                } else {
                    None
                };
                state.dragging = false;
                if self.controller.is_dragging() {
                    // The workspace owns release hit testing and submits the
                    // only drop, after the canvas has cleared its press state.
                    return Some(Action::request_redraw().and_capture());
                }
                self.controller.cancel_candidate();
                Some(Action::publish(Message::ChartReleased { segment }).and_capture())
            }
            _ => None,
        }
    }
    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let metrics = Metrics::new(bounds.width, bounds.height);
        let center_hover = self.can_go_up
            && cursor
                .position_in(bounds)
                .is_some_and(|point| metrics.in_center(point));
        let invalidated = {
            let previous = state.geometry_key.borrow();
            previous.as_ref().is_none_or(|key| {
                !key.matches(
                    self.segments,
                    self.hover.as_ref(),
                    &self.collected,
                    center_hover,
                )
            })
        };
        if invalidated {
            state.geometry.clear();
            let key = GeometryKey::capture(
                self.segments,
                self.hover.as_ref(),
                &self.collected,
                center_hover,
            );
            *state.geometry_key.borrow_mut() = Some(key);
        }
        // Both key borrows above end before Cache::draw invokes the frame closure.
        let base = state.geometry.draw(renderer, bounds.size(), |frame| {
            self.draw_base(frame, metrics, center_hover);
        });
        // The workspace draws the drag ghost beyond canvas bounds. Idle frames
        // still reuse only the cached geometry, with no empty live overlay.
        vec![base]
    }
    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let metrics = Metrics::new(bounds.width, bounds.height);
        if cursor.position_in(bounds).is_some_and(|point| {
            hit_test(self.segments, metrics, point).is_some()
                || (self.can_go_up && metrics.in_center(point))
        }) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ohmydisk_core::{NativePath, NodeSpace};
    fn node(id: &str, parent: &str, bytes: u64, directory: bool) -> Node {
        Node {
            id: id.into(),
            parent_id: Some(parent.into()),
            name: id.into(),
            path: NativePath::new(std::path::Path::new("/tmp")),
            kind: if directory {
                NodeKind::Directory
            } else {
                NodeKind::File
            },
            logical_bytes: bytes * 100,
            allocated_bytes: None,
            space_usage: Some(NodeSpace {
                attributed_bytes: bytes,
                measured_entries: 1,
                unmeasured_entries: 0,
            }),
            modified_ns: None,
            identity: id.into(),
            volume_id: "test".into(),
            link_count: 1,
            complete: true,
            is_link_or_reparse: false,
        }
    }
    #[test]
    fn geometry_cache_compares_values_instead_of_slice_identity_or_length() {
        let segments = layout("root", 100, &[node("a", "root", 100, false)]);
        let selected = vec!["a".to_owned()];
        let hover = Target::Node("a".into());
        let key = GeometryKey::capture(&segments, Some(&hover), &selected, false);
        assert!(key.matches(
            &segments.clone(),
            Some(&hover.clone()),
            &selected.clone(),
            false
        ));
        let mut changed = segments.clone();
        changed[0].end -= 0.1;
        assert!(!key.matches(&changed, Some(&hover), &selected, false));
        changed = segments.clone();
        changed[0].color_index += 1;
        assert!(!key.matches(&changed, Some(&hover), &selected, false));
        changed = segments.clone();
        changed[0].target = Target::Node("b".into());
        assert!(!key.matches(&changed, Some(&hover), &selected, false));
        changed = segments.clone();
        changed[0].bytes += 1;
        assert!(!key.matches(&changed, Some(&hover), &selected, false));
    }
    #[test]
    fn geometry_cache_invalidates_for_focus_collection_and_center_outline() {
        let segments = layout("root", 100, &[node("a", "root", 100, false)]);
        let hover = Target::Node("a".into());
        let selected = vec!["a".to_owned()];
        let key = GeometryKey::capture(&segments, Some(&hover), &selected, false);
        assert!(!key.matches(&segments, None, &selected, false));
        assert!(!key.matches(&segments, Some(&Target::Available), &selected, false));
        assert!(!key.matches(&segments, Some(&hover), &["b".to_owned()], false));
        assert!(!key.matches(&segments, Some(&hover), &selected, true));
    }
    #[test]
    fn capacity_ring_conserves_observation_and_synthetic_targets_are_typed() {
        let nodes = [
            node("available", "root", 500, true),
            node("child", "available", 300, false),
        ];
        let segments = capacity_layout(
            "root",
            Capacity {
                total: 995,
                available: 245,
                attributed: 500,
                remainder: 250,
            },
            &nodes,
        );
        assert_eq!(
            segments
                .iter()
                .filter(|s| s.depth == 0)
                .map(|s| s.bytes)
                .sum::<u64>(),
            995
        );
        assert!(
            segments
                .iter()
                .any(|s| s.target == Target::Node("available".into()))
        );
        assert!(segments.iter().any(|s| s.target == Target::Available));
        assert!((segments.last().unwrap().end - TAU).abs() < 0.0001);
    }
    #[test]
    fn tiny_folder_attribution_remains_accounted_in_the_volume_ring() {
        let capacity = Capacity {
            total: 995_000_000_000,
            available: 245_000_000_000,
            attributed: 8_800_000,
            remainder: 749_991_200_000,
        };
        let segments = capacity_layout(
            "root",
            capacity,
            &[node("small-folder", "root", 8_800_000, true)],
        );
        assert_eq!(
            segments
                .iter()
                .filter(|segment| segment.depth == 0)
                .map(|segment| segment.bytes)
                .sum::<u64>(),
            capacity.total
        );
        assert!(
            segments
                .iter()
                .any(|segment| matches!(segment.target, Target::Aggregate { .. })
                    && segment.bytes == 8_800_000)
        );
    }
    #[test]
    fn subtree_geometry_uses_attributed_bytes_and_never_caps_invalid_data() {
        let segments = layout(
            "root",
            100,
            &[
                node("a", "root", 60, true),
                node("b", "root", 40, false),
                node("c", "a", 30, false),
            ],
        );
        assert!((segments[0].end - TAU * 0.6).abs() < 0.001);
        assert!(segments.iter().any(|s| s.depth == 1
            && matches!(s.target, Target::Aggregate { .. })
            && s.bytes == 30));
        assert!(layout("root", 10, &[node("too-large", "root", 20, false)]).is_empty());
        let m = Metrics::new(400.0, 400.0);
        assert!(hit_test(&segments, m, m.center).is_none());
    }
    #[test]
    fn unmeasured_files_do_not_become_logical_wedges() {
        let mut unknown = node("unknown", "root", 900, false);
        unknown.space_usage = None;
        assert!(layout("root", 0, &[unknown]).is_empty());
    }
    #[test]
    fn canceled_generation_or_changed_scope_cannot_reuse_a_pressed_sector() {
        let scope = Some(interaction::Scope {
            scan_id: "first".into(),
            epoch: 3,
        });
        let mut pointer = PointerState::default();
        pointer.synchronize(4, &scope);
        pointer.pressed = Some(0);
        pointer.origin = Some(Point::new(10.0, 10.0));
        pointer.dragging = true;
        pointer.synchronize(5, &scope);
        assert!(pointer.pressed.is_none());
        assert!(pointer.origin.is_none());
        assert!(!pointer.dragging);
        pointer.pressed = Some(0);
        pointer.center_pressed = true;
        pointer.synchronize(
            5,
            &Some(interaction::Scope {
                scan_id: "second".into(),
                epoch: 3,
            }),
        );
        assert!(pointer.pressed.is_none());
        assert!(!pointer.center_pressed);
    }
    #[test]
    fn first_move_outside_the_canvas_still_crosses_the_drag_threshold() {
        let controller = interaction::Controller::default();
        let scope = Some(interaction::Scope {
            scan_id: "scan".into(),
            epoch: 1,
        });
        let segments = overview_layout(100, 25);
        let chart = Sunburst {
            segments: &segments,
            hover: None,
            collected: vec![],
            can_go_up: false,
            controller,
            scope: scope.clone(),
            resolve_drag: Box::new(|_| None),
        };
        let mut pointer = PointerState::default();
        pointer.synchronize(chart.controller.generation(), &scope);
        pointer.pressed = Some(0);
        pointer.origin = Some(Point::new(30.0, 30.0));
        let outside = Point::new(300.0, 300.0);
        let _ = canvas::Program::update(
            &chart,
            &mut pointer,
            &iced::Event::Mouse(mouse::Event::CursorMoved { position: outside }),
            Rectangle::new(Point::ORIGIN, iced::Size::new(100.0, 100.0)),
            mouse::Cursor::Available(outside),
        );
        assert!(pointer.dragging);
    }
}
