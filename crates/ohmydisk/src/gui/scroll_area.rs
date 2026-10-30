//! Keep scrollbars beside their contents when a pane overflows.

use iced::Element;
use iced::widget::{Scrollable, scrollable};

const SCROLLBAR_WIDTH: f32 = 10.0;
const SCROLLBAR_SPACING: f32 = 8.0;

fn scrollbar() -> scrollable::Scrollbar {
    // iced reserves the track width for embedded bars. Keep the thumb no wider
    // than the track so its drawing also stays inside the reserved gutter.
    scrollable::Scrollbar::new()
        .width(SCROLLBAR_WIDTH)
        .scroller_width(SCROLLBAR_WIDTH)
        .spacing(SCROLLBAR_SPACING)
}

pub(super) fn vertical<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
) -> Scrollable<'a, Message> {
    Scrollable::with_direction(content, scrollable::Direction::Vertical(scrollbar()))
}

pub(super) fn horizontal<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
) -> Scrollable<'a, Message> {
    Scrollable::with_direction(content, scrollable::Direction::Horizontal(scrollbar()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::{layout, widget::Tree};
    use iced::widget::{Row, Space};
    use iced::{Length, Size};

    fn assert_near(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
    }

    #[test]
    fn vertical_gutter_tracks_overflow_and_wrapped_content_at_both_widths() {
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 16.0.into());

        for (width, content_height) in [(500.0, 138.0), (1100.0, 89.0)] {
            let cards =
                Row::with_children((0..5).map(|_| Space::new().width(230).height(40).into()))
                    .width(Length::Fill)
                    .spacing(8)
                    .wrap()
                    .vertical_spacing(9);
            let mut widget: Element<'_, ()> = vertical(cards)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
            let mut tree = Tree::new(widget.as_widget());

            // Reuse the tree as resizing removes overflow and then restores it.
            for (height, overflow) in [(60.0, true), (220.0, false), (60.0, true)] {
                let viewport = Size::new(width, height);
                let node = widget.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, viewport),
                );
                let content = &node.children()[0];
                let gutter = if overflow {
                    SCROLLBAR_WIDTH + SCROLLBAR_SPACING
                } else {
                    0.0
                };

                assert_eq!(node.size(), viewport);
                assert_near(content.size().width, width - gutter);
                assert_eq!(content.size().height > height, overflow);
                assert_near(content.size().height, content_height);
                for card in content.children() {
                    assert!(card.bounds().x + card.size().width <= content.size().width);
                }
            }
        }
    }

    #[test]
    fn horizontal_gutter_tracks_overflow_and_resize_at_both_widths() {
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, 16.0.into());

        for width in [500.0, 1100.0] {
            let mut widget: Element<'_, ()> =
                horizontal(Space::new().width(1600).height(Length::Fill))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into();
            let mut tree = Tree::new(widget.as_widget());

            for (viewport_width, overflow) in [(width, true), (1800.0, false), (width, true)] {
                let viewport = Size::new(viewport_width, 80.0);
                let node = widget.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, viewport),
                );
                let content = &node.children()[0];
                let gutter = if overflow {
                    SCROLLBAR_WIDTH + SCROLLBAR_SPACING
                } else {
                    0.0
                };

                assert_eq!(node.size(), viewport);
                assert_near(content.size().height, viewport.height - gutter);
                assert_eq!(content.size().width > viewport.width, overflow);
            }
        }
    }
}
