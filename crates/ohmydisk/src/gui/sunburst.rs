//! Bounded, immutable-snapshot sunburst layout and pointer hit testing.
use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, TAU};

use iced::widget::canvas::{self, Action, Frame, Geometry, Path};
use iced::{Color, Point, Rectangle, Renderer, Theme, mouse};
use ohmydisk_core::model::{Node, NodeKind};

use super::Message;

const MAX_DEPTH: usize = 4;
const MIN_SWEEP: f32 = 0.016;
const MAX_SIBLINGS: usize = 36;

#[derive(Debug, Clone)]
pub struct Segment {
    /// Aggregates are display objects and never cleanup targets.
    pub node_id: Option<String>,
    pub owner_id: String,
    pub depth: usize,
    pub start: f32,
    pub end: f32,
    pub bytes: u64,
    pub color_index: usize,
}

/// Missing descendants and tiny entries occupy an explicit aggregate sector.
/// Directory bytes are used only to divide their parent's angle, never summed
/// with descendant bytes to calculate a snapshot total.
pub fn layout(root_id: &str, total: u64, nodes: &[Node]) -> Vec<Segment> {
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
        entries.sort_by(|a, b| b.logical_bytes.cmp(&a.logical_bytes).then(a.id.cmp(&b.id)));
    }
    let mut segments = Vec::new();
    layout_level(root_id, total, 0.0, TAU, 0, 0, &children, &mut segments);
    segments
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
    let mut angle = start;
    let mut represented = 0_u64;
    let mut displayed = 0;
    for (index, node) in entries.iter().enumerate() {
        let remaining = total.saturating_sub(represented);
        let bytes = node.logical_bytes.min(remaining);
        if bytes == 0 {
            continue;
        }
        let width = sweep * (bytes as f64 / total as f64) as f32;
        if width < MIN_SWEEP || displayed >= MAX_SIBLINGS {
            continue;
        }
        let color_index = if depth == 0 { index } else { palette };
        output.push(Segment {
            node_id: Some(node.id.clone()),
            owner_id: parent.to_owned(),
            depth,
            start: angle,
            end: (angle + width).min(start + sweep),
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
        represented = represented.saturating_add(bytes);
        angle += width;
        displayed += 1;
    }
    let remainder = total.saturating_sub(represented);
    if remainder > 0 && start + sweep - angle > 0.0001 {
        output.push(Segment {
            node_id: None,
            owner_id: parent.to_owned(),
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
        let radius = (width.min(height) * 0.46).max(1.0);
        Self {
            center: Point::new(width / 2.0, height / 2.0),
            hole: radius * 0.29,
            ring: radius * 0.71 / MAX_DEPTH as f32,
        }
    }
}

pub fn hit_test(segments: &[Segment], metrics: Metrics, point: Point) -> Option<usize> {
    let dx = point.x - metrics.center.x;
    let dy = point.y - metrics.center.y;
    let radius = dx.hypot(dy);
    if radius < metrics.hole || radius >= metrics.hole + metrics.ring * MAX_DEPTH as f32 {
        return None;
    }
    let depth = ((radius - metrics.hole) / metrics.ring).floor() as usize;
    let angle = (dy.atan2(dx) + FRAC_PI_2).rem_euclid(TAU);
    segments
        .iter()
        .position(|s| s.depth == depth && angle >= s.start && angle < s.end)
}

pub fn color(index: usize, depth: usize) -> Color {
    const COLORS: [(f32, f32, f32); 9] = [
        (0.31, 0.40, 0.96),
        (0.39, 0.32, 0.90),
        (0.52, 0.32, 0.91),
        (0.31, 0.56, 0.94),
        (0.35, 0.46, 0.85),
        (0.57, 0.40, 0.88),
        (0.28, 0.62, 0.83),
        (0.48, 0.48, 0.96),
        (0.39, 0.38, 0.76),
    ];
    let (r, g, b) = COLORS[index % COLORS.len()];
    let light = depth as f32 * 0.075;
    Color::from_rgb(
        (r + light).min(1.0),
        (g + light).min(1.0),
        (b + light).min(1.0),
    )
}

pub struct Sunburst<'a> {
    pub segments: &'a [Segment],
    pub hover: Option<&'a str>,
    pub collected: Vec<String>,
}

#[derive(Default)]
pub struct PointerState {
    hover: Option<usize>,
    pressed: Option<usize>,
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
        let hit = cursor.position_in(bounds).and_then(|point| {
            hit_test(
                self.segments,
                Metrics::new(bounds.width, bounds.height),
                point,
            )
        });
        match event {
            iced::Event::Mouse(mouse::Event::CursorMoved { .. }) if hit != state.hover => {
                state.hover = hit;
                let id = hit.and_then(|i| self.segments[i].node_id.clone());
                Some(Action::publish(Message::Hover(id)))
            }
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if hit.is_some() =>
            {
                state.pressed = hit;
                let id = hit.and_then(|i| self.segments[i].node_id.clone());
                Some(Action::publish(Message::DragStarted(id)).and_capture())
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.pressed.is_some() =>
            {
                let pressed = state.pressed.take();
                let outside = !cursor.is_over(bounds);
                let segment =
                    if pressed == hit { hit } else { None }.map(|i| self.segments[i].clone());
                Some(Action::publish(Message::ChartReleased { segment, outside }).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let metrics = Metrics::new(bounds.width, bounds.height);
        for segment in self.segments {
            let inner = metrics.hole + segment.depth as f32 * metrics.ring + 1.8;
            let outer = inner + metrics.ring - 3.6;
            let gap = (0.008_f32).min((segment.end - segment.start) * 0.12);
            let start = segment.start + gap / 2.0 - FRAC_PI_2;
            let end = segment.end - gap / 2.0 - FRAC_PI_2;
            let steps = (((end - start) * 60.0).ceil() as usize).max(2);
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
            let mut fill = color(segment.color_index, segment.depth);
            if segment.node_id.is_none() {
                fill = Color::from_rgb(0.23, 0.25, 0.36);
            }
            let hovered = segment
                .node_id
                .as_deref()
                .is_some_and(|id| Some(id) == self.hover);
            let selected = segment
                .node_id
                .as_ref()
                .is_some_and(|id| self.collected.contains(id));
            if hovered {
                fill = Color::from_rgb(
                    (fill.r + 0.16).min(1.0),
                    (fill.g + 0.16).min(1.0),
                    (fill.b + 0.10).min(1.0),
                );
            }
            if selected {
                fill.a = 0.42;
            }
            frame.fill(&path, fill);
            if selected {
                frame.stroke(
                    &path,
                    canvas::Stroke::default()
                        .with_color(Color::from_rgb(0.70, 0.73, 1.0))
                        .with_width(1.2),
                );
            }
        }
        if self.segments.is_empty() {
            for depth in 0..MAX_DEPTH {
                let radius = metrics.hole + (depth as f32 + 0.5) * metrics.ring;
                frame.stroke(
                    &Path::circle(metrics.center, radius),
                    canvas::Stroke::default()
                        .with_color(Color::from_rgb(0.13, 0.15, 0.22))
                        .with_width(metrics.ring - 4.0),
                );
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor
            .position_in(bounds)
            .and_then(|point| {
                hit_test(
                    self.segments,
                    Metrics::new(bounds.width, bounds.height),
                    point,
                )
            })
            .is_some()
        {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ohmydisk_core::model::{NativePath, NodeKind};
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
            logical_bytes: bytes,
            allocated_bytes: None,
            modified_ns: None,
            identity: id.into(),
            volume_id: "test".into(),
            link_count: 1,
            complete: true,
            is_link_or_reparse: false,
        }
    }
    #[test]
    fn angles_respect_parent_and_missing_data_becomes_aggregate() {
        let segments = layout(
            "root",
            100,
            &[
                node("a", "root", 60, true),
                node("b", "root", 40, false),
                node("a1", "a", 30, false),
            ],
        );
        let root: Vec<_> = segments.iter().filter(|s| s.depth == 0).collect();
        assert_eq!(root.len(), 2);
        assert!((root[0].end - TAU * 0.6).abs() < 0.001);
        let aggregate = segments
            .iter()
            .find(|s| s.depth == 1 && s.node_id.is_none())
            .unwrap();
        assert_eq!(aggregate.bytes, 30);
        assert!((aggregate.end - root[0].end).abs() < 0.001);
    }
    #[test]
    fn hit_testing_rejects_hole_and_respects_ring_depth() {
        let segments = layout(
            "root",
            100,
            &[node("a", "root", 100, true), node("a1", "a", 100, false)],
        );
        let m = Metrics::new(400.0, 400.0);
        assert!(hit_test(&segments, m, m.center).is_none());
        let index = hit_test(
            &segments,
            m,
            Point::new(m.center.x, m.center.y - m.hole - m.ring * 1.5),
        )
        .unwrap();
        assert_eq!(segments[index].node_id.as_deref(), Some("a1"));
        assert!(hit_test(&segments, m, Point::new(900.0, 900.0)).is_none());
    }
    #[test]
    fn zero_sizes_and_empty_scans_are_safe() {
        assert!(layout("root", 0, &[node("a", "root", 0, false)]).is_empty());
        let segments = layout("root", 100, &[]);
        assert_eq!(segments.len(), 1);
        assert!(segments[0].node_id.is_none());
    }
}
